//! `VM.*` verb implementations. Shell out to virsh; record state in the
//! in-process registry so getAllVmStats can report without re-querying
//! libvirt every poll.

use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::{json, Value};
use tokio::process::Command;
use tracing::{info, warn};

use vdsm_rpc::JsonRpcError;

use crate::{domain_xml::DomainSpec, registry, VmRecord, VmState};

const VIRSH: &str = "/usr/bin/virsh";
const URI: &str = "qemu:///system";

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn vm_id_from(params: &Value) -> Result<String, JsonRpcError> {
    params
        .get("vmID")
        .and_then(Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| JsonRpcError::invalid_params("vmID missing"))
}

/// Run virsh and capture stdout. On nonzero exit, returns the stderr as
/// the error message — engine logs it verbatim.
async fn run_virsh(args: &[&str], stdin_payload: Option<&str>) -> Result<String, String> {
    let mut cmd = Command::new(VIRSH);
    cmd.arg("-c").arg(URI);
    cmd.args(args);
    if stdin_payload.is_some() {
        cmd.stdin(std::process::Stdio::piped());
    }
    cmd.stdout(std::process::Stdio::piped());
    cmd.stderr(std::process::Stdio::piped());

    let mut child = cmd.spawn().map_err(|e| format!("spawn virsh: {e}"))?;
    if let Some(payload) = stdin_payload {
        if let Some(mut stdin) = child.stdin.take() {
            use tokio::io::AsyncWriteExt;
            stdin
                .write_all(payload.as_bytes())
                .await
                .map_err(|e| format!("virsh stdin: {e}"))?;
            drop(stdin);
        }
    }
    let out = child.wait_with_output().await.map_err(|e| e.to_string())?;
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr).trim().to_string();
        return Err(stderr);
    }
    Ok(String::from_utf8_lossy(&out.stdout).to_string())
}

pub async fn vm_create(params: Value) -> Result<Value, JsonRpcError> {
    let vm_id = vm_id_from(&params)?;
    let spec = DomainSpec::from_vm_params(&vm_id, &params);
    let xml = spec.to_xml();

    info!(
        vm_id = %vm_id, name = %spec.name, mem_mb = spec.mem_mb,
        vcpus = spec.vcpus, "VM.create"
    );

    // virsh define accepts XML on stdin via `define /dev/stdin`.
    if let Err(e) = run_virsh(&["define", "/dev/stdin"], Some(&xml)).await {
        return Err(JsonRpcError::internal(format!("virsh define: {e}")));
    }
    if let Err(e) = run_virsh(&["start", &spec.name], None).await {
        // Define succeeded but start failed — leave domain defined so
        // operator can debug; engine just sees the error.
        warn!(vm_id = %vm_id, error = %e, "virsh start failed");
        return Err(JsonRpcError::internal(format!("virsh start: {e}")));
    }

    let record = VmRecord {
        vm_id: vm_id.clone(),
        vm_name: spec.name.clone(),
        mem_size_mb: spec.mem_mb,
        vcpus: spec.vcpus,
        state: VmState::Up,
        created_secs: now_secs(),
    };
    registry().write().await.insert(vm_id.clone(), record);

    // Bare struct — engine's JsonResponseUtil wraps. See host-Up gotcha #1.
    Ok(json!({
        "vmList": {
            "vmId": vm_id,
            "status": VmState::Up.as_engine_str(),
        }
    }))
}

pub async fn vm_destroy(params: Value) -> Result<Value, JsonRpcError> {
    let vm_id = vm_id_from(&params)?;
    let name = registry()
        .read()
        .await
        .get(&vm_id)
        .map(|r| r.vm_name.clone())
        .unwrap_or_else(|| vm_id.clone());

    let _ = run_virsh(&["destroy", &name], None).await; // already-off is fine
    let _ = run_virsh(&["undefine", &name], None).await;
    registry().write().await.remove(&vm_id);
    Ok(json!({}))
}

pub async fn vm_shutdown(params: Value) -> Result<Value, JsonRpcError> {
    let vm_id = vm_id_from(&params)?;
    let name = registry()
        .read()
        .await
        .get(&vm_id)
        .map(|r| r.vm_name.clone())
        .unwrap_or_else(|| vm_id.clone());
    let _ = run_virsh(&["shutdown", &name], None).await;
    if let Some(rec) = registry().write().await.get_mut(&vm_id) {
        rec.state = VmState::PoweringDown;
    }
    Ok(json!({}))
}

pub async fn vm_cont(params: Value) -> Result<Value, JsonRpcError> {
    let vm_id = vm_id_from(&params)?;
    let name = registry()
        .read()
        .await
        .get(&vm_id)
        .map(|r| r.vm_name.clone())
        .unwrap_or_else(|| vm_id.clone());
    let _ = run_virsh(&["resume", &name], None).await;
    if let Some(rec) = registry().write().await.get_mut(&vm_id) {
        rec.state = VmState::Up;
    }
    Ok(json!({}))
}

pub async fn vm_pause(params: Value) -> Result<Value, JsonRpcError> {
    let vm_id = vm_id_from(&params)?;
    let name = registry()
        .read()
        .await
        .get(&vm_id)
        .map(|r| r.vm_name.clone())
        .unwrap_or_else(|| vm_id.clone());
    let _ = run_virsh(&["suspend", &name], None).await;
    if let Some(rec) = registry().write().await.get_mut(&vm_id) {
        rec.state = VmState::Paused;
    }
    Ok(json!({}))
}

/// `Host.dumpxmls(vmIds=[...])` — engine ingests external VMs by asking
/// for their libvirt XML. We `virsh dumpxml` each UUID directly so the
/// XML matches what's actually running, not what we synthesized.
pub async fn dump_xmls(params: Value) -> Result<Value, JsonRpcError> {
    // Engine's DumpXmlsVDSCommand sends `{vmList: [...]}`, not vmIds.
    let ids: Vec<String> = params
        .get("vmList")
        .or_else(|| params.get("vmIds"))
        .and_then(Value::as_array)
        .map(|a| a.iter().filter_map(Value::as_str).map(str::to_string).collect())
        .unwrap_or_default();

    let mut out = serde_json::Map::new();
    for id in ids {
        match run_virsh(&["dumpxml", &id], None).await {
            Ok(xml) => {
                out.insert(id, Value::String(xml));
            }
            Err(e) => {
                tracing::warn!(vm_id = %id, error = %e, "dumpxml failed");
            }
        }
    }
    Ok(Value::Object(out))
}

pub async fn vm_get_stats(params: Value) -> Result<Value, JsonRpcError> {
    let vm_id = vm_id_from(&params)?;
    let reg = registry().read().await;
    let stats = reg
        .get(&vm_id)
        .map(record_to_stats)
        .unwrap_or_else(|| json!({"vmId": vm_id, "status": "Down"}));
    Ok(json!({ "statsList": [stats] }))
}

/// Scan libvirt for running domains and reconcile into the registry.
/// Domains libvirt knows about but we don't get a minimal record so
/// engine still sees them in getAllVmStats. Survives vdsmd restarts.
pub async fn reconcile_from_libvirt() {
    let Ok(out) = run_virsh(&["list", "--name", "--state-running"], None).await else {
        return;
    };
    let names: Vec<String> = out
        .lines()
        .map(|l| l.trim())
        .filter(|l| !l.is_empty())
        .map(str::to_string)
        .collect();

    let mut reg = registry().write().await;
    // Drop registry entries whose domain disappeared from libvirt.
    reg.retain(|_, r| names.contains(&r.vm_name));

    for name in names {
        // Pull domuuid so we have a stable key. Skip on error.
        let Ok(uuid_out) = run_virsh(&["domuuid", &name], None).await else {
            continue;
        };
        let uuid = uuid_out.trim().to_string();
        if uuid.is_empty() {
            continue;
        }
        if reg.contains_key(&uuid) {
            continue;
        }
        // Best-effort mem + vcpu from `virsh dominfo`.
        let info = run_virsh(&["dominfo", &name], None).await.unwrap_or_default();
        let mem_mb = info
            .lines()
            .find_map(|l| l.strip_prefix("Used memory:").map(str::trim))
            .and_then(|s| s.split_whitespace().next())
            .and_then(|n| n.parse::<u64>().ok())
            .map(|kb| kb / 1024)
            .unwrap_or(0);
        let vcpus = info
            .lines()
            .find_map(|l| l.strip_prefix("CPU(s):").map(str::trim))
            .and_then(|s| s.parse::<u32>().ok())
            .unwrap_or(1);
        reg.insert(
            uuid.clone(),
            VmRecord {
                vm_id: uuid,
                vm_name: name,
                mem_size_mb: mem_mb,
                vcpus,
                state: VmState::Up,
                created_secs: now_secs(),
            },
        );
    }
}

pub fn record_to_stats(r: &VmRecord) -> Value {
    json!({
        "vmId": r.vm_id,
        "vmName": r.vm_name,
        "status": r.state.as_engine_str(),
        "memSize": r.mem_size_mb,
        "vcpuCount": r.vcpus,
        "elapsedTime": (now_secs().saturating_sub(r.created_secs)).to_string(),
        "monitorResponse": "0",
        "displayInfo": [{"type": "vnc", "port": "5900", "ipAddress": "127.0.0.1"}],
        "kvmEnable": "true",
        "exitCode": 0,
        "exitMessage": "",
        "guestCPUCount": -1,
        "vmType": "kvm",
        "session": "Unknown",
        "clientIp": "",
        "username": "",
        "appsList": [],
        "guestIPs": "",
        "guestFQDN": "",
    })
}
