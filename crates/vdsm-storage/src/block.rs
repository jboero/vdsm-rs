//! iSCSI / FC / LVM / block-device verbs. Real shell-outs to iscsiadm /
//! vgs / lvs / lsblk / multipath. Block-SD volume operations dispatch
//! through [`crate::sd_backend`] — `Volume.create` on a block SD does
//! `lvcreate`, `Image.prepare` does `lvchange -ay`, etc.

use std::process::Stdio;

use serde::Deserialize;
use serde_json::{json, Value};
use tokio::process::Command;
use tracing::{info, warn};

use vdsm_rpc::JsonRpcError;

use crate::sd_backend::{run, PrivOp};

/// Run a *read-only* query binary (lsblk/pvs/vgs) as the vdsm user;
/// capture stdout. None on missing-tool / failure. Privileged/mutating
/// commands go through [`run`]/[`PrivOp`] to supervdsmd instead.
async fn query(bin: &str, args: &[&str]) -> Option<String> {
    let out = Command::new(bin).args(args)
        .stdout(Stdio::piped()).stderr(Stdio::piped())
        .output().await.ok()?;
    if !out.status.success() { return None; }
    Some(String::from_utf8_lossy(&out.stdout).to_string())
}

#[derive(Debug, Deserialize, Default)]
pub struct IscsiTargetSpec {
    #[serde(default)] pub iqn: String,
    #[serde(default)] pub portal: String,
    #[serde(default, alias = "portal_port")] pub port: String,
    #[serde(default)] pub user: String,
    #[serde(default)] pub password: String,
    #[serde(default)] pub tpgt: String,
}

/// `Host.discoverSendTargets` / `Host.iscsiDiscoverSendTargets` — engine
/// asks the host to enumerate iSCSI targets reachable on a given portal.
/// Real impl: `iscsiadm -m discovery -t st -p <portal>:<port>`.
pub async fn iscsi_discover_send_targets(params: Value) -> Result<Value, JsonRpcError> {
    let spec: IscsiTargetSpec = serde_json::from_value(
        params.get("con").cloned().unwrap_or(params.clone()),
    ).unwrap_or_default();
    if spec.portal.is_empty() {
        return Ok(json!({ "fullTargets": [], "targets": [] }));
    }
    let port = if spec.port.is_empty() { "3260".to_string() } else { spec.port.clone() };
    let portal = format!("{}:{}", spec.portal, port);
    info!(%portal, "iscsiDiscoverSendTargets");
    let raw = run(PrivOp::IscsiDiscover { portal: portal.clone() })
        .await.unwrap_or_default();

    // Output lines: "<portal>,<tpgt> <iqn>"
    let mut full = Vec::new();
    let mut iqns = Vec::new();
    for line in raw.lines() {
        let mut parts = line.split_whitespace();
        let (Some(addr), Some(iqn)) = (parts.next(), parts.next()) else { continue; };
        full.push(json!({ "portal": addr, "iqn": iqn }));
        iqns.push(iqn.to_string());
    }
    Ok(json!({ "fullTargets": full, "targets": iqns }))
}

/// `Host.iscsiLogin` — establish a session to an iSCSI target.
pub async fn iscsi_login(params: Value) -> Result<Value, JsonRpcError> {
    let spec: IscsiTargetSpec = serde_json::from_value(
        params.get("con").cloned().unwrap_or(params.clone()),
    ).unwrap_or_default();
    if spec.iqn.is_empty() || spec.portal.is_empty() {
        return Err(JsonRpcError::invalid_params("iqn + portal required"));
    }
    let port = if spec.port.is_empty() { "3260".to_string() } else { spec.port.clone() };
    let portal = format!("{}:{}", spec.portal, port);
    info!(iqn = %spec.iqn, %portal, "iscsiLogin");
    let _ = run(PrivOp::IscsiLogin { iqn: spec.iqn.clone(), portal }).await;
    Ok(json!({}))
}

pub async fn iscsi_logout(params: Value) -> Result<Value, JsonRpcError> {
    let spec: IscsiTargetSpec = serde_json::from_value(
        params.get("con").cloned().unwrap_or(params.clone()),
    ).unwrap_or_default();
    if spec.iqn.is_empty() { return Ok(json!({})); }
    let port = if spec.port.is_empty() { "3260".to_string() } else { spec.port.clone() };
    let portal = format!("{}:{}", spec.portal, port);
    info!(iqn = %spec.iqn, %portal, "iscsiLogout");
    let _ = run(PrivOp::IscsiLogout { iqn: spec.iqn.clone(), portal }).await;
    Ok(json!({}))
}

pub async fn iscsi_rescan(_params: Value) -> Result<Value, JsonRpcError> {
    let _ = run(PrivOp::IscsiRescan).await;
    Ok(json!({}))
}

/// `Host.getDeviceList` — block devices visible to the host. Reports
/// iSCSI LUNs and FC devices. We use `lsblk -J -o ...,TRAN` to learn
/// the transport ("iscsi" / "fc" / "sas" / "sata"), so engine sees the
/// correct devtype rather than every disk labelled iSCSI.
pub async fn get_device_list(_params: Value) -> Result<Value, JsonRpcError> {
    let raw = query("/usr/bin/lsblk",
        &["-J", "-b", "-o", "NAME,SIZE,TYPE,WWN,VENDOR,MODEL,SERIAL,FSTYPE,TRAN"])
        .await.unwrap_or_default();
    let parsed: serde_json::Value = serde_json::from_str(&raw).unwrap_or(json!({}));
    let mut out = Vec::new();
    if let Some(blockdevices) = parsed.get("blockdevices").and_then(|v| v.as_array()) {
        for d in blockdevices {
            if d.get("type").and_then(|t| t.as_str()) != Some("disk") { continue; }
            let name = d.get("name").and_then(|v| v.as_str()).unwrap_or("").to_string();
            let wwn = d.get("wwn").and_then(|v| v.as_str()).unwrap_or("").to_string();
            if wwn.is_empty() { continue; }
            let tran = d.get("tran").and_then(|v| v.as_str()).unwrap_or("").to_lowercase();
            let devtype = match tran.as_str() {
                "iscsi" => "iSCSI",
                "fc"    => "FCP",
                "sas"   => "SAS",
                _       => "SCSI",
            };
            // Pull pv/vg UUIDs if this device is already an LVM PV.
            let (pv_uuid, vg_uuid) = pv_membership(&format!("/dev/{name}")).await;
            out.push(json!({
                "GUID":       wwn.trim_start_matches("0x").to_string(),
                "devtype":    devtype,
                "capacity":   d.get("size").cloned().unwrap_or(json!("0")),
                "vendorID":   d.get("vendor").cloned().unwrap_or(json!("")),
                "productID":  d.get("model").cloned().unwrap_or(json!("")),
                "serial":     d.get("serial").cloned().unwrap_or(json!("")),
                "fwrev":      "",
                "status":     if vg_uuid.is_empty() { "free" } else { "used" },
                "pathstatus": [json!({ "physdev": name, "state": "active" })],
                "pvUUID":     pv_uuid,
                "vgUUID":     vg_uuid,
                "pathlist":   [],
            }));
        }
    }
    Ok(json!(out))
}

/// Look up `(pv_uuid, vg_uuid)` for a block device. Empty strings if the
/// device is not an LVM PV. We resolve the VG UUID via a second pvs call
/// because `pvs -o vg_uuid` returns the VG UUID directly.
async fn pv_membership(dev: &str) -> (String, String) {
    let Some(raw) = query("/usr/sbin/pvs",
        &["--noheadings", "-o", "pv_uuid,vg_uuid", dev]).await else {
        return (String::new(), String::new());
    };
    let mut it = raw.split_whitespace();
    let pv = it.next().unwrap_or("").to_string();
    let vg = it.next().unwrap_or("").to_string();
    (pv, vg)
}

/// `Host.getDevicesVisibility` — engine asks for a subset of devices to
/// confirm they're still visible after iSCSI login or rescan.
pub async fn get_devices_visibility(params: Value) -> Result<Value, JsonRpcError> {
    let ids: Vec<String> = params.get("devices")
        .or_else(|| params.get("guids"))
        .and_then(Value::as_array)
        .map(|a| a.iter().filter_map(Value::as_str).map(str::to_string).collect())
        .unwrap_or_default();
    let mut visibility = serde_json::Map::new();
    for id in ids {
        let visible = tokio::fs::metadata(format!("/dev/disk/by-id/wwn-0x{id}")).await.is_ok()
            || tokio::fs::metadata(format!("/dev/disk/by-id/{id}")).await.is_ok();
        visibility.insert(id, json!(visible));
    }
    Ok(json!(visibility))
}

/// `Host.getLVMVGList` — list LVM volume groups (potential block-SD VGs).
pub async fn get_lvm_vg_list(_params: Value) -> Result<Value, JsonRpcError> {
    let raw = query("/usr/sbin/vgs",
        &["--reportformat", "json", "-o", "vg_uuid,vg_name,vg_size,vg_free,vg_tags"])
        .await.unwrap_or_default();
    let parsed: serde_json::Value = serde_json::from_str(&raw).unwrap_or(json!({}));
    let mut out = Vec::new();
    if let Some(reports) = parsed.get("report").and_then(|v| v.as_array()) {
        for r in reports {
            if let Some(vgs) = r.get("vg").and_then(|v| v.as_array()) {
                for vg in vgs {
                    out.push(json!({
                        "vgUUID":    vg.get("vg_uuid").cloned().unwrap_or(json!("")),
                        "name":      vg.get("vg_name").cloned().unwrap_or(json!("")),
                        "size":      vg.get("vg_size").cloned().unwrap_or(json!("0")),
                        "free":      vg.get("vg_free").cloned().unwrap_or(json!("0")),
                        "attr":      "",
                        "state":     "OK",
                        "pvList":    [],
                    }));
                }
            }
        }
    }
    Ok(json!(out))
}

pub async fn get_vg_info(params: Value) -> Result<Value, JsonRpcError> {
    let vg = params.get("vgUUID").or_else(|| params.get("vgName"))
        .and_then(Value::as_str).unwrap_or("").to_string();
    if vg.is_empty() { return Err(JsonRpcError::invalid_params("vgUUID or vgName required")); }
    let list = get_lvm_vg_list(json!({})).await?;
    if let Some(arr) = list.as_array() {
        for v in arr {
            let id = v.get("vgUUID").and_then(Value::as_str).unwrap_or("");
            let name = v.get("name").and_then(Value::as_str).unwrap_or("");
            if id == vg || name == vg { return Ok(v.clone()); }
        }
    }
    Err(JsonRpcError::internal(format!("VG {vg} not found")))
}

/// `Host.createVG` — create LVM VG on a set of block devices. Engine
/// sends either `name=<sd_uuid>` or `vgName=<sd_uuid>`. For block SDs
/// the VG name **must** equal the SD UUID so `sd_backend()` can find
/// it later.
pub async fn create_vg(params: Value) -> Result<Value, JsonRpcError> {
    let name = params.get("vgName")
        .or_else(|| params.get("name"))
        .and_then(Value::as_str).unwrap_or("").to_string();
    let devices: Vec<String> = params.get("devlist")
        .and_then(Value::as_array)
        .map(|a| a.iter().filter_map(Value::as_str).map(str::to_string).collect())
        .unwrap_or_default();
    if name.is_empty() || devices.is_empty() {
        return Err(JsonRpcError::invalid_params("vgName + devlist required"));
    }
    info!(%name, device_count = devices.len(), "createVG");
    for d in &devices {
        if run(PrivOp::Pvcreate { device: d.clone() }).await.is_none() {
            return Err(JsonRpcError::internal(format!("pvcreate {d} failed")));
        }
    }
    if run(PrivOp::Vgcreate { vg: name.clone(), devices: devices.clone() }).await.is_none() {
        warn!("vgcreate failed");
        return Err(JsonRpcError::internal("vgcreate failed"));
    }
    let info = get_vg_info(json!({"vgName": name})).await.unwrap_or(json!({}));
    let uuid = info.get("vgUUID").and_then(Value::as_str).unwrap_or("").to_string();
    Ok(json!({ "uuid": uuid }))
}

pub async fn extend_vg(params: Value) -> Result<Value, JsonRpcError> {
    let vg = params.get("vgUUID").or_else(|| params.get("vgName"))
        .and_then(Value::as_str).unwrap_or("").to_string();
    let devices: Vec<String> = params.get("devlist")
        .and_then(Value::as_array)
        .map(|a| a.iter().filter_map(Value::as_str).map(str::to_string).collect())
        .unwrap_or_default();
    if vg.is_empty() || devices.is_empty() {
        return Err(JsonRpcError::invalid_params("vg + devlist required"));
    }
    for d in &devices {
        let _ = run(PrivOp::Pvcreate { device: d.clone() }).await;
        let _ = run(PrivOp::Vgextend { vg: vg.clone(), device: d.clone() }).await;
    }
    Ok(json!({}))
}

pub async fn remove_vg(params: Value) -> Result<Value, JsonRpcError> {
    let vg = params.get("vgUUID").or_else(|| params.get("vgName"))
        .and_then(Value::as_str).unwrap_or("").to_string();
    if vg.is_empty() { return Err(JsonRpcError::invalid_params("vg required")); }
    let _ = run(PrivOp::Vgremove { vg }).await;
    Ok(json!({}))
}

/// Multipath inventory. Parses `multipath -ll -j` (JSON, available on
/// multipath-tools ≥ 0.8). If multipath isn't installed, returns empty.
pub async fn get_path_list_status(_params: Value) -> Result<Value, JsonRpcError> {
    let Some(raw) = run(PrivOp::MultipathList).await else {
        return Ok(json!([]));
    };
    let parsed: serde_json::Value = serde_json::from_str(&raw).unwrap_or(json!({}));
    let mut out = Vec::new();
    if let Some(maps) = parsed.get("maps").and_then(|v| v.as_array()) {
        for m in maps {
            let name = m.get("name").and_then(Value::as_str).unwrap_or("");
            let wwn = m.get("uuid").and_then(Value::as_str).unwrap_or("");
            let mut paths = Vec::new();
            if let Some(pgs) = m.get("path_groups").and_then(Value::as_array) {
                for pg in pgs {
                    if let Some(ps) = pg.get("paths").and_then(Value::as_array) {
                        for p in ps {
                            paths.push(json!({
                                "physdev": p.get("dev").cloned().unwrap_or(json!("")),
                                "state":   p.get("dm_st").cloned().unwrap_or(json!("")),
                                "type":    p.get("chk_st").cloned().unwrap_or(json!("")),
                            }));
                        }
                    }
                }
            }
            out.push(json!({
                "name":   name,
                "GUID":   wwn,
                "paths":  paths,
                "status": "active",
            }));
        }
    }
    Ok(json!(out))
}

/// `Host.fcScan` — rescan FC fabric. The sysfs `scan` nodes are
/// root-writable only, so the whole iteration runs inside supervdsmd.
pub async fn fc_scan(_params: Value) -> Result<Value, JsonRpcError> {
    let _ = run(PrivOp::FcScan).await;
    Ok(json!({}))
}
