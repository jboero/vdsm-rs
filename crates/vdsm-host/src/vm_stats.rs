//! `Host.getAllVmStats` — engine polls this every ~15s. We pull the
//! current registry snapshot from vdsm-virt and shape each record into
//! the stats dict engine expects per VM.

use serde_json::{json, Value};

use vdsm_rpc::JsonRpcError;
use vdsm_virt::{registry, verbs::{reconcile_from_libvirt, record_to_stats}};

pub async fn get_all_vm_stats(_params: Value) -> Result<Value, JsonRpcError> {
    // Pick up libvirt domains we didn't define ourselves (e.g. after a
    // vdsmd restart, or VMs started by tests / virsh).
    reconcile_from_libvirt().await;
    let reg = registry().read().await;
    let stats: Vec<Value> = reg.values().map(record_to_stats).collect();
    // Bare array — engine's JsonResponseUtil wraps in
    // `{statsList: [...], status: STATUS_DONE}` itself.
    Ok(json!(stats))
}
