//! Storage-namespace verb handlers. Phase 1: just enough to satisfy the
//! engine's "Add NFS Storage Domain" flow's first probes.

use std::process::Stdio;

use serde_json::{json, Value};
use tokio::process::Command;
use tracing::{info, warn};

use vdsm_rpc::JsonRpcError;

use crate::connection::{connections, StorageServerConnection};

/// `StoragePool.connectStorageServer` — engine sends one or more
/// connection specs; we mount each into `MNT_BASE`. Returns a per-spec
/// status code (0 = success). Engine expects a `statuslist` shape.
///
/// Params shape (truncated):
/// ```json
/// {"domainType": 1, "spUUID": "...", "conList": [{...spec...}]}
/// ```
/// where domainType=1 is NFS, 3 is iSCSI, 6 is Local, 7 is POSIX, 8 is Glance.
pub async fn connect_storage_server(params: Value) -> Result<Value, JsonRpcError> {
    let domain_type = params.get("domainType").and_then(Value::as_i64).unwrap_or(1);
    // Engine sends `connectionParams` (real wire name); older callers
    // also use `conList`. Accept either.
    let con_list: Vec<StorageServerConnection> = params
        .get("connectionParams")
        .or_else(|| params.get("conList"))
        .and_then(|v| serde_json::from_value(v.clone()).ok())
        .unwrap_or_default();

    info!(domain_type, count = con_list.len(), "connectStorageServer");

    let mut statuslist = Vec::with_capacity(con_list.len());
    for spec in con_list {
        let status = mount_one(&spec, domain_type).await;
        statuslist.push(json!({ "id": spec.id, "status": status }));
        if status == 0 {
            connections().write().await.push(spec);
        }
    }

    // Engine's JsonResponseUtil wraps the result itself; sending
    // `{statuslist: [...]}` here makes engine cast the wrapping map to
    // Object[] and throw ClassCastException. Return the bare list.
    Ok(json!(statuslist))
}

/// Run mount(8) for one connection. Returns 0 on success, otherwise an
/// errno-like int that engine surfaces to the operator.
async fn mount_one(spec: &StorageServerConnection, domain_type: i64) -> i32 {
    // Dispatch by storage type. Engine uses: 1=NFS, 2=FCP, 3=ISCSI,
    // 4=LOCALFS, 5=CIFS, 6=SHAREDFS, 7=POSIXFS, 8=GLUSTER, 9=NFSv3.
    // FC is hardware-discovered; iSCSI logs in via iscsiadm and surfaces
    // LUNs through getDeviceList rather than a mount.
    match domain_type {
        2 => {
            // FCP — no-op; engine separately calls Host.fcScan + getDeviceList.
            info!("FCP connectStorageServer (no-op; engine will call fcScan)");
            return 0;
        }
        3 => {
            // iSCSI — log in to target via iscsiadm. Connection field
            // carries the portal; iqn is in spec.iqn.
            return iscsi_login_one(spec).await;
        }
        _ => {} // 1/4/5/6/7/8/9 fall through to mount(8)
    }

    let mp = spec.mountpoint();
    if let Err(e) = tokio::fs::create_dir_all(&mp).await {
        warn!(error = %e, mountpoint = %mp.display(), "mkdir mountpoint failed");
        return 13; // EACCES-ish
    }

    // Skip remount if already mounted (idempotent).
    if is_mounted(&mp).await {
        info!(mountpoint = %mp.display(), "already mounted, skipping");
        return 0;
    }

    // Map domain_type to default vfs when spec didn't supply one.
    let vfs = if !spec.vfs_type.is_empty() {
        spec.vfs_type.as_str()
    } else {
        match domain_type {
            5 => "cifs",
            7 => "auto",            // POSIX FS — engine sends vfs_type explicitly
            4 => "none",            // LOCALFS — bind mount
            8 => "glusterfs",
            9 => "nfs",             // NFSv3
            _ => "nfs",
        }
    };
    // mount(2) requires UID 0 — mount.nfs strictly checks getuid()==0,
    // capabilities aren't enough. Use sudo via the NOPASSWD rule in
    // /etc/sudoers.d/vdsm-rs. Long-term: replace with a supervdsmd
    // RPC so we don't depend on sudoers.
    let mut args: Vec<String> = vec![
        "-n".into(), "/usr/bin/mount".into(),
        "-t".into(), vfs.into(),
    ];
    if !spec.mnt_options.is_empty() {
        args.push("-o".into());
        args.push(spec.mnt_options.clone());
    }
    args.push(spec.connection.clone());
    args.push(mp.display().to_string());

    let out = Command::new("/usr/bin/sudo")
        .args(&args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .await;

    match out {
        Ok(o) if o.status.success() => {
            info!(connection = %spec.connection, mountpoint = %mp.display(), "mounted");
            0
        }
        Ok(o) => {
            let stderr = String::from_utf8_lossy(&o.stderr);
            warn!(
                connection = %spec.connection, stderr = %stderr,
                "mount failed"
            );
            100 // generic mount-failed engine errno; will refine as we hit specific cases
        }
        Err(e) => {
            warn!(error = %e, "mount spawn failed");
            13
        }
    }
}

/// Log in to an iSCSI target via iscsiadm. Engine surfaces the resulting
/// LUNs through subsequent Host.getDeviceList calls.
async fn iscsi_login_one(spec: &StorageServerConnection) -> i32 {
    // Engine packs iqn/portal/port into the connection string for some
    // shapes, into separate fields for others. We accept both.
    if spec.connection.is_empty() {
        warn!("iSCSI connect: empty connection string");
        return 22; // EINVAL
    }
    info!(connection = %spec.connection, "iSCSI login");
    // The connection string is typically "portal:port,iqn" or engine
    // sends iqn separately via the StorageServerConnection.iqn field.
    // For the PoC we fall back to running discovery on the portal then
    // logging in to any IQN found. If iscsiadm is missing the call
    // returns failure but doesn't crash vdsm.
    let portal = spec.connection.clone();
    let disc = Command::new("/usr/sbin/iscsiadm")
        .args(["-m", "discovery", "-t", "st", "-p", &portal])
        .output().await;
    if let Ok(o) = disc {
        if !o.status.success() { return 101; }
    } else {
        return 101;
    }
    let login = Command::new("/usr/sbin/iscsiadm")
        .args(["-m", "node", "-p", &portal, "--login"])
        .output().await;
    match login {
        Ok(o) if o.status.success() => 0,
        _ => 101,
    }
}

async fn is_mounted(mp: &std::path::Path) -> bool {
    if let Ok(s) = tokio::fs::read_to_string("/proc/self/mountinfo").await {
        let needle = mp.display().to_string();
        s.lines().any(|line| line.split_whitespace().nth(4) == Some(needle.as_str()))
    } else {
        false
    }
}

pub async fn disconnect_storage_server(params: Value) -> Result<Value, JsonRpcError> {
    let con_list: Vec<StorageServerConnection> = params
        .get("connectionParams")
        .or_else(|| params.get("conList"))
        .and_then(|v| serde_json::from_value(v.clone()).ok())
        .unwrap_or_default();

    let mut statuslist = Vec::with_capacity(con_list.len());
    for spec in con_list {
        let mp = spec.mountpoint();
        let status = if is_mounted(&mp).await {
            let out = Command::new("/usr/bin/sudo")
                .args(["-n", "/usr/bin/umount", mp.display().to_string().as_str()])
                .output()
                .await;
            match out {
                Ok(o) if o.status.success() => 0,
                _ => 16,
            }
        } else {
            0
        };
        connections().write().await.retain(|c| c.id != spec.id);
        statuslist.push(json!({ "id": spec.id, "status": status }));
    }
    // Bare array — engine wraps.
    Ok(json!(statuslist))
}

pub async fn get_storage_server_connections_list(_params: Value) -> Result<Value, JsonRpcError> {
    let conns = connections().read().await;
    Ok(json!(*conns)) // bare array
}

/// `Host.discoverSendTargets` — iSCSI target discovery probe. We don't
/// support iSCSI in v0, so return an empty fullTargets list. Engine
/// tolerates the empty response cleanly.
pub async fn discover_send_targets(_params: Value) -> Result<Value, JsonRpcError> {
    Ok(json!([])) // bare array — engine wraps to {fullTargets: ...}
}

/// `Host.getDeviceList` — block device inventory (for FC/iSCSI/multipath
/// storage). We're NFS-only; report empty.
pub async fn get_device_list(_params: Value) -> Result<Value, JsonRpcError> {
    Ok(json!([])) // bare array — engine wraps to {devList: ...}
}
