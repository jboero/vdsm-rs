//! Image + Volume verbs — engine calls these when creating VM disks.
//!
//! Dispatches by SD backend ([`SdBackend`]):
//!
//! - **File SD** (NFS / POSIX / CIFS / LocalFS):
//!   ```text
//!   <sd>/images/<imageID>/<volumeID>          (qcow2 or raw payload)
//!   <sd>/images/<imageID>/<volumeID>.meta     (key=value metadata)
//!   <sd>/images/<imageID>/<volumeID>.lease    (sanlock lease — stub)
//!   ```
//!
//! - **Block SD** (iSCSI / FC, VG name == SD UUID):
//!   - Payload is `/dev/<sd_uuid>/<volumeID>` (LV); raw or qcow2-on-LV.
//!   - Metadata sidecars live off-LV at
//!     `/var/lib/vdsm/sd/<sd_uuid>/images/<imageID>/<volumeID>.meta`
//!     because we don't put a filesystem on the LV. Real upstream vdsm
//!     stores per-volume metadata as LV tags; for the PoC the sidecar is
//!     simpler and engine never reads it directly (only via our getInfo).

use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::{json, Value};
use tokio::process::Command;
use tracing::{info, warn};

use vdsm_rpc::JsonRpcError;

use crate::sd_backend::{sd_backend, sudo, SdBackend};

const ZERO_UUID: &str = "00000000-0000-0000-0000-000000000000";

fn vol_format_str(v: i64) -> &'static str {
    // upstream constants: COW=4, RAW=5
    match v {
        4 => "COW",
        _ => "RAW",
    }
}

fn vol_alloc_str(v: i64) -> &'static str {
    // SPARSE=0, PREALLOCATED=1
    match v {
        1 => "PREALLOCATED",
        _ => "SPARSE",
    }
}

/// Build the on-disk metadata blob; identical text for file and block SDs.
fn build_meta(
    size_bytes: u64, ctime: u64, desc: &str, disk_type: &str, sd_id: &str,
    fmt_str: &str, img_id: &str, src_vol: &str, alloc_str: &str,
) -> String {
    format!(
        "CAP={size_bytes}\n\
         CTIME={ctime}\n\
         DESCRIPTION={desc}\n\
         DISKTYPE={disk_type}\n\
         DOMAIN={sd_id}\n\
         FORMAT={fmt_str}\n\
         GEN=0\n\
         IMAGE={img_id}\n\
         LEGALITY=LEGAL\n\
         PUUID={src_vol}\n\
         TYPE={alloc_str}\n\
         VOLTYPE=LEAF\n\
         EOF\n",
    )
}

pub async fn volume_create(params: Value) -> Result<Value, JsonRpcError> {
    let vol_id = params.get("volumeID").and_then(Value::as_str).unwrap_or("").to_string();
    let sd_id = params.get("storagedomainID").and_then(Value::as_str).unwrap_or("").to_string();
    let img_id = params.get("imageID").and_then(Value::as_str).unwrap_or("").to_string();
    let size_bytes: u64 = params.get("size").and_then(Value::as_str)
        .and_then(|s| s.parse().ok()).unwrap_or(1_073_741_824);
    let fmt = params.get("volFormat").and_then(Value::as_i64).unwrap_or(5);
    let prealloc = params.get("preallocate").and_then(Value::as_i64).unwrap_or(0);
    let desc = params.get("desc").and_then(Value::as_str).unwrap_or("").to_string();
    let src_vol = params.get("srcVolUUID").and_then(Value::as_str).unwrap_or(ZERO_UUID).to_string();
    let disk_type = params.get("diskType").and_then(Value::as_str).unwrap_or("DATA").to_string();

    if vol_id.is_empty() || sd_id.is_empty() || img_id.is_empty() {
        return Err(JsonRpcError::invalid_params("volumeID/storagedomainID/imageID required"));
    }

    info!(%vol_id, %sd_id, %img_id, size_bytes, fmt, prealloc, "Volume.create");

    let fmt_str = vol_format_str(fmt);
    let alloc_str = vol_alloc_str(prealloc);
    let qemu_fmt = if fmt_str == "COW" { "qcow2" } else { "raw" };
    let ctime = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    let meta_blob = build_meta(
        size_bytes, ctime, &desc, &disk_type, &sd_id, fmt_str, &img_id, &src_vol, alloc_str,
    );

    match sd_backend(&sd_id).await {
        Some(SdBackend::File { sd_path }) => {
            create_file_volume(
                &sd_path, &img_id, &vol_id, size_bytes,
                qemu_fmt, alloc_str, &meta_blob,
            ).await?;
        }
        Some(SdBackend::Block { vg_name, state_dir }) => {
            create_block_volume(
                &vg_name, &state_dir, &img_id, &vol_id, size_bytes,
                qemu_fmt, &meta_blob,
            ).await?;
        }
        None => return Err(JsonRpcError::internal(format!("SD {sd_id} not found"))),
    }

    Ok(json!({"uuid": vol_id}))
}

async fn create_file_volume(
    sd_path: &Path, img_id: &str, vol_id: &str, size_bytes: u64,
    qemu_fmt: &str, alloc_str: &str, meta_blob: &str,
) -> Result<(), JsonRpcError> {
    let img_dir = sd_path.join("images").join(img_id);
    tokio::fs::create_dir_all(&img_dir).await
        .map_err(|e| JsonRpcError::internal(format!("mkdir {}: {e}", img_dir.display())))?;

    let vol_path = img_dir.join(vol_id);
    let mut args: Vec<String> = vec!["create".into(), "-f".into(), qemu_fmt.into()];
    if alloc_str == "PREALLOCATED" {
        args.push("-o".into());
        args.push("preallocation=falloc".into());
    }
    args.push(vol_path.display().to_string());
    args.push(size_bytes.to_string());

    let out = Command::new("/usr/bin/qemu-img").args(&args).output().await
        .map_err(|e| JsonRpcError::internal(format!("qemu-img spawn: {e}")))?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr).to_string();
        warn!(error = %err, "qemu-img create failed");
        return Err(JsonRpcError::internal(format!("qemu-img create: {err}")));
    }

    tokio::fs::write(img_dir.join(format!("{vol_id}.meta")), meta_blob).await
        .map_err(|e| JsonRpcError::internal(format!("write .meta: {e}")))?;
    let _ = tokio::fs::write(img_dir.join(format!("{vol_id}.lease")), vec![0u8; 1024 * 1024]).await;
    Ok(())
}

async fn create_block_volume(
    vg: &str, state_dir: &Path, img_id: &str, vol_id: &str, size_bytes: u64,
    qemu_fmt: &str, meta_blob: &str,
) -> Result<(), JsonRpcError> {
    // LV size: extents are 128 MiB (matches our vgcreate -s). Round up so
    // qcow2-on-LV has its overhead headroom; engine sends size as virtual
    // disk capacity, the LV must be at least that big on raw, and a bit
    // bigger for qcow2 (we just round to nearest extent above virtual size
    // — qcow2 metadata fits easily).
    let extent_bytes: u64 = 128 * 1024 * 1024;
    let rounded = size_bytes.div_ceil(extent_bytes) * extent_bytes;
    let size_arg = format!("{rounded}B");

    let lv_path = format!("{vg}/{vol_id}");

    // lvcreate -W y (force wipe of any prior signatures) -n <vol_id> -L <size>B <vg>
    if sudo("/usr/sbin/lvcreate",
        &["-W", "y", "-Z", "n", "-n", vol_id, "-L", &size_arg, vg]
    ).await.is_none() {
        return Err(JsonRpcError::internal(format!("lvcreate failed for {lv_path}")));
    }

    let dev = format!("/dev/{vg}/{vol_id}");
    if qemu_fmt == "qcow2" {
        // Format the LV block device as a qcow2 image of the requested
        // virtual size. Engine sees only the virtual size; the LV gives
        // qcow2 a slightly-overprovisioned backing store.
        let out = Command::new("/usr/bin/qemu-img")
            .args(["create", "-f", "qcow2", &dev, &size_bytes.to_string()])
            .output().await
            .map_err(|e| JsonRpcError::internal(format!("qemu-img spawn: {e}")))?;
        if !out.status.success() {
            let err = String::from_utf8_lossy(&out.stderr).to_string();
            // Roll back the LV so we don't leak orphan storage.
            let _ = sudo("/usr/sbin/lvremove", &["-f", "-y", &lv_path]).await;
            return Err(JsonRpcError::internal(format!("qemu-img create qcow2-on-LV: {err}")));
        }
    }
    // Raw block: nothing more to do — the LV's bytes ARE the disk.

    // Sidecar metadata.
    let meta_dir = state_dir.join("images").join(img_id);
    tokio::fs::create_dir_all(&meta_dir).await
        .map_err(|e| JsonRpcError::internal(format!("mkdir state {}: {e}", meta_dir.display())))?;
    tokio::fs::write(meta_dir.join(format!("{vol_id}.meta")), meta_blob).await
        .map_err(|e| JsonRpcError::internal(format!("write block .meta: {e}")))?;
    Ok(())
}

pub async fn volume_get_info(params: Value) -> Result<Value, JsonRpcError> {
    let vol_id = params.get("volumeID").and_then(Value::as_str).unwrap_or("").to_string();
    let sd_id = params.get("storagedomainID").and_then(Value::as_str).unwrap_or("").to_string();
    let img_id = params.get("imageID").and_then(Value::as_str).unwrap_or("").to_string();

    let Some(backend) = sd_backend(&sd_id).await else {
        return Err(JsonRpcError::internal(format!("SD {sd_id} not found")));
    };

    let (meta_path, apparent) = match &backend {
        SdBackend::File { sd_path } => {
            let img_dir = sd_path.join("images").join(&img_id);
            let vol_path = img_dir.join(&vol_id);
            let apparent = tokio::fs::metadata(&vol_path).await.map(|md| md.len()).unwrap_or(0);
            (img_dir.join(format!("{vol_id}.meta")), apparent)
        }
        SdBackend::Block { vg_name, state_dir } => {
            let dev = format!("/dev/{vg_name}/{vol_id}");
            // Apparent size = LV size in bytes (lvs --units B).
            let apparent = lv_size_bytes(vg_name, &vol_id).await.unwrap_or(0);
            let _ = dev;
            (state_dir.join("images").join(&img_id).join(format!("{vol_id}.meta")), apparent)
        }
    };

    let content = tokio::fs::read_to_string(&meta_path).await
        .map_err(|e| JsonRpcError::internal(format!("read meta: {e}")))?;
    let mut m = std::collections::HashMap::<String, String>::new();
    for line in content.lines() {
        if let Some((k, v)) = line.split_once('=') {
            m.insert(k.trim().into(), v.trim().into());
        }
    }
    let g = |k: &str| m.get(k).cloned().unwrap_or_default();
    Ok(json!({
        "uuid":         vol_id,
        "image":        img_id,
        "domain":       sd_id,
        "voltype":      g("VOLTYPE"),
        "format":       g("FORMAT"),
        "type":         g("TYPE"),
        "disktype":     g("DISKTYPE"),
        "capacity":     g("CAP"),
        "apparentsize": apparent.to_string(),
        "truesize":     apparent.to_string(),
        "parent":       g("PUUID"),
        "description":  g("DESCRIPTION"),
        "legality":     g("LEGALITY"),
        "ctime":        g("CTIME"),
        "mtime":        "0",
        "status":       "OK",
        "children":     [],
    }))
}

async fn lv_size_bytes(vg: &str, lv: &str) -> Option<u64> {
    let out = Command::new("/usr/sbin/lvs")
        .args(["--noheadings", "--units", "B", "--nosuffix",
               "-o", "lv_size", &format!("{vg}/{lv}")])
        .output().await.ok()?;
    if !out.status.success() { return None; }
    String::from_utf8_lossy(&out.stdout).trim().parse().ok()
}

pub async fn volume_get_size(params: Value) -> Result<Value, JsonRpcError> {
    let info = volume_get_info(params).await?;
    Ok(json!({
        "apparentsize": info.get("apparentsize").cloned().unwrap_or(json!("0")),
        "truesize":     info.get("truesize").cloned().unwrap_or(json!("0")),
    }))
}

pub async fn volume_delete(params: Value) -> Result<Value, JsonRpcError> {
    let vol_id = params.get("volumeID").and_then(Value::as_str).unwrap_or("").to_string();
    let sd_id = params.get("storagedomainID").and_then(Value::as_str).unwrap_or("").to_string();
    let img_id = params.get("imageID").and_then(Value::as_str).unwrap_or("").to_string();
    match sd_backend(&sd_id).await {
        Some(SdBackend::File { sd_path }) => {
            let img_dir = sd_path.join("images").join(&img_id);
            let _ = tokio::fs::remove_file(img_dir.join(&vol_id)).await;
            let _ = tokio::fs::remove_file(img_dir.join(format!("{vol_id}.meta"))).await;
            let _ = tokio::fs::remove_file(img_dir.join(format!("{vol_id}.lease"))).await;
        }
        Some(SdBackend::Block { vg_name, state_dir }) => {
            let _ = sudo("/usr/sbin/lvremove",
                &["-f", "-y", &format!("{vg_name}/{vol_id}")]).await;
            let meta = state_dir.join("images").join(&img_id).join(format!("{vol_id}.meta"));
            let _ = tokio::fs::remove_file(meta).await;
        }
        None => {}
    }
    Ok(json!({}))
}

/// `Image.prepare` — engine calls before VM uses the disk. For file SDs
/// just returns the absolute path. For block SDs, activates the LV
/// (`lvchange -ay`) and returns `/dev/<vg>/<vol>` so libvirt's domain
/// XML can reference it as a block disk.
pub async fn image_prepare(params: Value) -> Result<Value, JsonRpcError> {
    let vol_id = params.get("volumeID").and_then(Value::as_str).unwrap_or("").to_string();
    let sd_id = params.get("storagedomainID").and_then(Value::as_str).unwrap_or("").to_string();
    let img_id = params.get("imageID").and_then(Value::as_str).unwrap_or("").to_string();
    info!(%vol_id, %sd_id, %img_id, "Image.prepare");
    match sd_backend(&sd_id).await {
        Some(SdBackend::File { sd_path }) => {
            let vol_path = sd_path.join("images").join(&img_id).join(&vol_id);
            if !vol_path.exists() {
                return Err(JsonRpcError::internal(
                    format!("volume {} missing", vol_path.display())));
            }
            Ok(json!({"path": vol_path.display().to_string()}))
        }
        Some(SdBackend::Block { vg_name, .. }) => {
            let lv = format!("{vg_name}/{vol_id}");
            // Activate the LV — engine may have left it inactive on a
            // previous teardown. Failing here is fatal: libvirt can't open
            // an inactive LV.
            if sudo("/usr/sbin/lvchange", &["-a", "y", "-K", &lv]).await.is_none() {
                return Err(JsonRpcError::internal(format!("lvchange -ay {lv}")));
            }
            let dev = format!("/dev/{vg_name}/{vol_id}");
            if !PathBuf::from(&dev).exists() {
                return Err(JsonRpcError::internal(format!("device {dev} did not appear")));
            }
            Ok(json!({"path": dev}))
        }
        None => Err(JsonRpcError::internal(format!("SD {sd_id} not found"))),
    }
}

pub async fn image_teardown(params: Value) -> Result<Value, JsonRpcError> {
    let vol_id = params.get("volumeID").and_then(Value::as_str).unwrap_or("").to_string();
    let sd_id = params.get("storagedomainID").and_then(Value::as_str).unwrap_or("").to_string();
    if let Some(SdBackend::Block { vg_name, .. }) = sd_backend(&sd_id).await {
        if !vol_id.is_empty() {
            let _ = sudo("/usr/sbin/lvchange",
                &["-a", "n", &format!("{vg_name}/{vol_id}")]).await;
        }
    }
    Ok(json!({}))
}

pub async fn image_delete(params: Value) -> Result<Value, JsonRpcError> {
    let sd_id = params.get("storagedomainID").and_then(Value::as_str).unwrap_or("").to_string();
    let img_id = params.get("imageID").and_then(Value::as_str).unwrap_or("").to_string();
    match sd_backend(&sd_id).await {
        Some(SdBackend::File { sd_path }) => {
            let _ = tokio::fs::remove_dir_all(sd_path.join("images").join(&img_id)).await;
        }
        Some(SdBackend::Block { vg_name, state_dir }) => {
            // Find all volume UUIDs for this image by listing the sidecar dir,
            // lvremove each, then remove the dir.
            let img_dir = state_dir.join("images").join(&img_id);
            if let Ok(mut rd) = tokio::fs::read_dir(&img_dir).await {
                while let Ok(Some(ent)) = rd.next_entry().await {
                    let name = ent.file_name().to_string_lossy().to_string();
                    if let Some(vol) = name.strip_suffix(".meta") {
                        let _ = sudo("/usr/sbin/lvremove",
                            &["-f", "-y", &format!("{vg_name}/{vol}")]).await;
                    }
                }
            }
            let _ = tokio::fs::remove_dir_all(img_dir).await;
        }
        None => {}
    }
    Ok(json!({}))
}

pub async fn image_delete_volumes(params: Value) -> Result<Value, JsonRpcError> {
    image_delete(params).await
}
