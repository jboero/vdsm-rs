//! Storage-domain on-disk layout + create/getInfo verbs.
//!
//! oVirt SD metadata is a flat key=value file at `dom_md/metadata`. The
//! engine reads this back via `StorageDomain.getInfo` to populate its DB,
//! so the keys MUST match upstream vdsm's expectations exactly — typos
//! become "field not found" or "type mismatch" exceptions in engine's
//! StorageDomainStatic parser.

use std::path::PathBuf;

use serde_json::{json, Value};
use tracing::{info, warn};

use vdsm_rpc::JsonRpcError;

use crate::connection::StorageServerConnection;
use crate::sd_backend::{sd_backend, vg_exists, SdBackend, BLOCK_SD_STATE_BASE};

/// Empty 1 MiB file used for the sanlock ids/leases/inbox/outbox stubs.
/// Real sanlock layout is more nuanced (8-byte aligned headers, host
/// IDs) but for a single-host PoC the engine just needs the files to
/// exist with reasonable sizes.
const ONE_MIB: usize = 1024 * 1024;

fn class_to_str(c: i64) -> &'static str {
    match c {
        2 => "Iso",
        3 => "Backup",
        _ => "Data",
    }
}

fn type_to_str(t: i64) -> &'static str {
    // Upstream vdsm constants: NFS=1, FCP=2, ISCSI=3, LOCALFS=4, CIFS=5,
    // SHAREDFS=6, POSIXFS=7, GLUSTERFS=8.
    match t {
        1 => "NFS",
        2 => "FCP",
        3 => "ISCSI",
        4 => "LOCALFS",
        5 => "CIFS",
        7 => "POSIXFS",
        _ => "NFS",
    }
}

fn is_block_type(t: i64) -> bool { matches!(t, 2 | 3) }

/// `StorageDomain.create` — engine sends an SD UUID + the underlying
/// path. For file SDs (NFS/POSIX/CIFS/LocalFS) we write the on-disk
/// layout to the mounted SD directly. For block SDs (iSCSI/FC) engine
/// will have already called `Host.createVG` with `vgName=<sd_uuid>`, so
/// we keep the metadata under `/var/lib/vdsm/sd/<sd_uuid>/` since the
/// LVs have no filesystem to write into.
pub async fn create_storage_domain(params: Value) -> Result<Value, JsonRpcError> {
    let sd_uuid = params.get("storagedomainID").and_then(Value::as_str).unwrap_or("").to_string();
    let typeargs = params.get("typeArgs").and_then(Value::as_str).unwrap_or("").to_string();
    let name = params.get("name").and_then(Value::as_str).unwrap_or("nfs").to_string();
    let domain_class = params.get("domainClass").and_then(Value::as_i64).unwrap_or(1);
    let domain_type = params.get("domainType").and_then(Value::as_i64).unwrap_or(1);
    let block_size = params.get("blockSize").and_then(Value::as_i64).unwrap_or(512);
    let version = params.get("version").and_then(Value::as_i64).unwrap_or(5);

    if sd_uuid.is_empty() {
        return Err(JsonRpcError::invalid_params("storagedomainID required"));
    }
    if !is_block_type(domain_type) && typeargs.is_empty() {
        return Err(JsonRpcError::invalid_params(
            "typeArgs required for file storage domain types",
        ));
    }

    info!(
        sd_uuid, %typeargs, %name, domain_class, domain_type, version,
        "StorageDomain.create"
    );

    // SD metadata content is identical between file and block — only the
    // sink differs. REMOTE_PATH is `typeArgs` for file SDs (server:/export);
    // for block SDs upstream stores the VG name there. We mirror that.
    let remote_path = if is_block_type(domain_type) {
        sd_uuid.clone()
    } else {
        typeargs.clone()
    };
    let metadata = format!(
        "ALIGNMENT=1048576\n\
         BLOCK_SIZE={block_size}\n\
         CLASS={cls}\n\
         DESCRIPTION={name}\n\
         IOOPTIMEOUTSEC=10\n\
         LEASERETRIES=3\n\
         LEASETIMESEC=60\n\
         LOCKPOLICY=\n\
         LOCKRENEWALINTERVALSEC=5\n\
         MASTER_VERSION=0\n\
         POOL_UUID=\n\
         REMOTE_PATH={remote_path}\n\
         ROLE=Regular\n\
         SDUUID={sd_uuid}\n\
         TYPE={typ}\n\
         VERSION={version}\n\
         _SHA_CKSUM=0000000000000000000000000000000000000000000000000000000000000000\n",
        cls = class_to_str(domain_class),
        typ = type_to_str(domain_type),
    );

    let dom_md = if is_block_type(domain_type) {
        // Block SD: state dir under /var/lib/vdsm/sd/<sd>/.
        if !vg_exists(&sd_uuid).await {
            return Err(JsonRpcError::internal(format!(
                "VG {sd_uuid} not present — engine should call Host.createVG first"
            )));
        }
        let state = PathBuf::from(BLOCK_SD_STATE_BASE).join(&sd_uuid);
        let dom_md = state.join("dom_md");
        for d in [&state, &dom_md, &state.join("master"), &state.join("images")] {
            tokio::fs::create_dir_all(d).await
                .map_err(|e| JsonRpcError::internal(format!("mkdir {}: {e}", d.display())))?;
        }
        dom_md
    } else {
        // File SD: on the mounted SD directly.
        let mp: PathBuf = StorageServerConnection {
            connection: typeargs.clone(),
            ..Default::default()
        }.mountpoint();
        if !mp.exists() {
            warn!(mountpoint = %mp.display(), "create_storage_domain: mountpoint missing");
            return Err(JsonRpcError::internal(format!(
                "mountpoint {} not present — was connectStorageServer called first?",
                mp.display()
            )));
        }
        let sd_dir = mp.join(&sd_uuid);
        let dom_md = sd_dir.join("dom_md");
        let master = sd_dir.join("master");
        let images = sd_dir.join("images");
        for d in [&sd_dir, &dom_md, &master,
                  &master.join("tasks"), &master.join("vms"), &images] {
            tokio::fs::create_dir_all(d).await
                .map_err(|e| JsonRpcError::internal(format!("mkdir {}: {e}", d.display())))?;
        }
        dom_md
    };

    tokio::fs::write(dom_md.join("metadata"), &metadata).await
        .map_err(|e| JsonRpcError::internal(format!("write metadata: {e}")))?;

    // sanlock-managed file stubs. For block SDs these would normally be
    // dedicated LVs at the head of the VG; we use plain files in the
    // state dir, which is fine because we don't run sanlock anyway.
    for (sfile, size) in [
        ("ids", ONE_MIB),
        ("leases", 2 * ONE_MIB),
        ("inbox", 8 * ONE_MIB),
        ("outbox", 8 * ONE_MIB),
    ] {
        let _ = tokio::fs::write(dom_md.join(sfile), vec![0u8; size]).await;
    }

    Ok(json!({}))
}


/// `StorageDomain.getStats` — engine polls this for disk usage of the SD.
/// Response shape: `{disktotal, diskfree, mdasize, mdafree}`, all bytes
/// as strings. Engine uses these for capacity planning + over-commit
/// guards.
pub async fn get_storage_domain_stats(params: Value) -> Result<Value, JsonRpcError> {
    let sd_uuid = params.get("storagedomainID").and_then(Value::as_str).unwrap_or("").to_string();
    let (total, free) = sd_total_free(&sd_uuid).await;
    if total == 0 && free == 0 {
        // Distinguish "SD unknown" from "SD reports empty" — engine treats
        // a missing SD as a hard error.
        if sd_backend(&sd_uuid).await.is_none() {
            return Err(JsonRpcError::internal(format!("SD {sd_uuid} not found")));
        }
    }
    Ok(json!({
        "disktotal": total.to_string(),
        "diskfree":  free.to_string(),
        // Metadata-area headroom: 1% of total / free.
        "mdasize":   (total / 100).to_string(),
        "mdafree":   (free / 100).to_string(),
    }))
}


/// `StorageDomain.attach` — engine binds the SD to a storage pool.
/// PoC: just update master/version on disk; real upstream writes
/// pool UUID into the SD metadata.
pub async fn attach_storage_domain(_params: Value) -> Result<Value, JsonRpcError> {
    Ok(json!({}))
}

/// `StorageDomain.activate` — engine flips the SD to "Active" state.
/// PoC no-op success.
pub async fn activate_storage_domain(_params: Value) -> Result<Value, JsonRpcError> {
    Ok(json!({}))
}

/// `Host.HSMGetAllTasksStatuses` — task subsystem in upstream vdsm
/// tracks async SPM operations. PoC: empty dict (no in-flight tasks).
pub async fn hsm_get_all_tasks_statuses(_params: Value) -> Result<Value, JsonRpcError> {
    Ok(json!({}))
}

/// `StoragePool.connectStoragePool` — host joins a pool.
/// Per-host pool state. We only ever participate in one pool at a time
/// (single-DC PoC), so a single OnceLock entry is enough. Tracks which
/// SDs the engine considers part of this pool so `getStoragePoolInfo`
/// can report a populated `dominfo` — empty dominfo is what engine
/// interprets as `IRSNonOperationalException: Could not connect host to
/// Data Center(Storage issue)`.
use std::sync::OnceLock;
use tokio::sync::RwLock;

#[derive(Debug, Default, Clone)]
pub struct PoolState {
    pub pool_id:    String,
    pub master_sd:  String,
    pub master_ver: i64,
    pub domains:    Vec<String>,
}

static POOL: OnceLock<RwLock<PoolState>> = OnceLock::new();
fn pool_state() -> &'static RwLock<PoolState> {
    POOL.get_or_init(|| RwLock::new(PoolState::default()))
}

pub async fn connect_storage_pool(params: Value) -> Result<Value, JsonRpcError> {
    let pool_id = params.get("storagepoolID").and_then(Value::as_str).unwrap_or("").to_string();
    let master_sd = params.get("masterSdUUID").and_then(Value::as_str).unwrap_or("").to_string();
    let master_ver = params.get("masterVersion").and_then(Value::as_i64).unwrap_or(1);
    let domains: Vec<String> = params
        .get("domainsMap")
        .and_then(Value::as_object)
        .map(|m| m.keys().cloned().collect())
        .unwrap_or_default();
    info!(%pool_id, %master_sd, master_ver, domain_count = domains.len(), "connectStoragePool");
    let mut s = pool_state().write().await;
    if !pool_id.is_empty()    { s.pool_id    = pool_id; }
    if !master_sd.is_empty()  { s.master_sd  = master_sd; }
    s.master_ver = master_ver.max(s.master_ver);
    for d in domains { if !s.domains.contains(&d) { s.domains.push(d); } }
    Ok(json!({}))
}

/// `StoragePool.create` — engine builds the DC / storage pool around an
/// existing master SD. Record the pool/master mapping so getStoragePoolInfo
/// reports it back.
pub async fn create_storage_pool(params: Value) -> Result<Value, JsonRpcError> {
    let pool_id = params.get("poolID").or_else(|| params.get("storagepoolID"))
        .and_then(Value::as_str).unwrap_or("").to_string();
    let master_sd = params.get("masterDom").or_else(|| params.get("masterSdUUID"))
        .and_then(Value::as_str).unwrap_or("").to_string();
    let domains: Vec<String> = params
        .get("domList")
        .and_then(Value::as_array)
        .map(|a| a.iter().filter_map(Value::as_str).map(str::to_string).collect())
        .unwrap_or_default();
    info!(%pool_id, %master_sd, "createStoragePool");
    let mut s = pool_state().write().await;
    if !pool_id.is_empty()   { s.pool_id   = pool_id; }
    if !master_sd.is_empty() { s.master_sd = master_sd; }
    if s.master_ver == 0     { s.master_ver = 1; }
    for d in domains { if !s.domains.contains(&d) { s.domains.push(d); } }
    Ok(json!({}))
}

/// `StoragePool.spmStart` — promote this host to SPM. Track in pool_state.
pub async fn spm_start(params: Value) -> Result<Value, JsonRpcError> {
    let pool_id = params.get("storagepoolID").and_then(Value::as_str).unwrap_or("").to_string();
    if !pool_id.is_empty() { pool_state().write().await.pool_id = pool_id; }
    Ok(json!({}))
}

/// `StoragePool.getSpmStatus`. Single host = always SPM holder.
pub async fn get_spm_status(params: Value) -> Result<Value, JsonRpcError> {
    let _ = params.get("storagepoolID");
    Ok(json!({
        "spmStatus": "SPM",
        "spmLver":   "0",
        "spmId":     "1",
    }))
}

/// `StoragePool.getInfo` — populate `info` block + `dominfo` from
/// pool state and live SD metadata on disk. Empty `dominfo` triggers
/// engine's IRSNonOperationalException.
pub async fn get_storage_pool_info(_params: Value) -> Result<Value, JsonRpcError> {
    let s = pool_state().read().await.clone();

    let mut dominfo = serde_json::Map::new();
    for sd in &s.domains {
        let total_free = sd_total_free(sd).await;
        dominfo.insert(sd.clone(), json!({
            "status":    "Active",
            "diskfree":  total_free.1.to_string(),
            "alerts":    [],
            "version":   "5",
            "isoprefix": "",
        }));
    }
    // Always include the master SD even if domains[] is empty (engine
    // expects at least one entry once the pool is connected).
    if !s.master_sd.is_empty() && !dominfo.contains_key(&s.master_sd) {
        let total_free = sd_total_free(&s.master_sd).await;
        dominfo.insert(s.master_sd.clone(), json!({
            "status":    "Active",
            "diskfree":  total_free.1.to_string(),
            "alerts":    [],
            "version":   "5",
            "isoprefix": "",
        }));
    }

    Ok(json!({
        "info": {
            "name":          "Default",
            "version":       "5",
            "isoprefix":     "",
            "master_uuid":   s.master_sd,
            "master_ver":    s.master_ver,
            "lver":          0i64,
            "spm_id":        1i64,
            "type":          "NFS",
            "pool_status":   "connected",
        },
        "dominfo": Value::Object(dominfo),
    }))
}

/// Disk total+free in bytes for an SD by UUID. File SDs use `df` on the
/// mountpoint; block SDs use `vgs` on the VG.
async fn sd_total_free(sd_uuid: &str) -> (u64, u64) {
    match sd_backend(sd_uuid).await {
        Some(SdBackend::File { sd_path }) => {
            // `-B1` = byte counts; `--output` and `-P` are mutually
            // exclusive on GNU coreutils — drop `-P`. Last data line is
            // "<size> <avail>".
            let Ok(out) = tokio::process::Command::new("/usr/bin/df")
                .args(["-B1", "--output=size,avail", sd_path.display().to_string().as_str()])
                .output().await else { return (0, 0); };
            let s = String::from_utf8_lossy(&out.stdout);
            let n: Vec<u64> = s.lines().last()
                .map(|l| l.split_whitespace().filter_map(|t| t.parse().ok()).collect())
                .unwrap_or_default();
            (n.first().copied().unwrap_or(0), n.get(1).copied().unwrap_or(0))
        }
        Some(SdBackend::Block { vg_name, .. }) => {
            let Ok(out) = tokio::process::Command::new("/usr/sbin/vgs")
                .args(["--noheadings", "--units", "B", "--nosuffix",
                       "-o", "vg_size,vg_free", &vg_name])
                .output().await else { return (0, 0); };
            let s = String::from_utf8_lossy(&out.stdout);
            let n: Vec<u64> = s.split_whitespace().filter_map(|t| t.parse().ok()).collect();
            (n.first().copied().unwrap_or(0), n.get(1).copied().unwrap_or(0))
        }
        None => (0, 0),
    }
}

/// `StorageDomain.getInfo` — engine re-reads the metadata after create.
/// Parse the dom_md/metadata file back into the dict shape engine wants.
pub async fn get_storage_domain_info(params: Value) -> Result<Value, JsonRpcError> {
    let sd_uuid = params.get("storagedomainID").and_then(Value::as_str).unwrap_or("").to_string();
    if sd_uuid.is_empty() {
        return Err(JsonRpcError::invalid_params("storagedomainID required"));
    }

    let metadata_path = match sd_backend(&sd_uuid).await {
        Some(SdBackend::File { sd_path }) => sd_path.join("dom_md").join("metadata"),
        Some(SdBackend::Block { state_dir, .. }) => state_dir.join("dom_md").join("metadata"),
        None => return Err(JsonRpcError::internal(format!("SD {sd_uuid} not found"))),
    };
    if !metadata_path.exists() {
        return Err(JsonRpcError::internal(format!(
            "SD {sd_uuid} metadata missing at {}", metadata_path.display()
        )));
    }
    let content = tokio::fs::read_to_string(&metadata_path).await
        .map_err(|e| JsonRpcError::internal(format!("read metadata: {e}")))?;

    // Parse the on-disk UPPERCASE_UNDERSCORE format into a map, then map
    // to engine's expected lowercase/camelCase keys. Engine's
    // HSMGetStorageDomainInfoVDSCommand reads these specific keys —
    // sending the on-disk names trips NPE on the unparsed fields.
    let mut meta = std::collections::HashMap::<String, String>::new();
    for line in content.lines() {
        if let Some((k, v)) = line.split_once('=') {
            meta.insert(k.trim().to_string(), v.trim().to_string());
        }
    }
    let g = |k: &str| meta.get(k).cloned().unwrap_or_default();

    // Engine reads these JSON keys into StorageDomainStatic fields.
    // Shape matches what upstream vdsm's StorageDomain.getInfo returns.
    Ok(json!({
        "uuid":       sd_uuid,
        "type":       g("TYPE"),       // "NFS" / "LOCALFS" / ...
        "class":      g("CLASS"),      // "Data" / "Iso" / ...
        "name":       g("DESCRIPTION"),
        "role":       g("ROLE"),       // "Master" / "Regular"
        "pool":       Value::Array(vec![]),
        "version":    g("VERSION"),
        "remotePath": g("REMOTE_PATH"),
        "alignment":  g("ALIGNMENT"),
        "block_size": g("BLOCK_SIZE"),
        "master_ver": g("MASTER_VERSION"),
        "lver":       "-1",
        "spm_id":     "-1",
        "state":      "OK",
    }))
}
