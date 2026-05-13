//! `Host.getHardwareInfo` / `Host.hostdevListByCaps` / `Host.setMOMPolicyParameters`
//! — engine calls these during `InitVdsOnUpCommand`. We return minimal but
//! well-formed responses so InitVdsOnUp doesn't log scary errors. None of
//! them gate the Up transition, but unblocking them quiets the host event
//! log.

use serde_json::{json, Value};

use vdsm_rpc::JsonRpcError;

use crate::sysinfo;

pub async fn get_hardware_info(_params: Value) -> Result<Value, JsonRpcError> {
    let cpu = sysinfo::cpu_info();
    // Read DMI from /sys/class/dmi/id/* instead of hardcoded QEMU/Standard PC.
    // Empty fallbacks for fields the platform doesn't expose (e.g. some Dells
    // ship without product_serial visible to userspace).
    let manuf = sysinfo::dmi_field("sys_vendor");
    let product = sysinfo::dmi_field("product_name");
    let version = sysinfo::dmi_field("product_version");
    let serial = sysinfo::dmi_field("product_serial");
    let family = sysinfo::dmi_field("sys_family");
    let uuid = {
        let dmi_uuid = sysinfo::dmi_field("product_uuid");
        if dmi_uuid.is_empty() { sysinfo::vdsm_id() } else { dmi_uuid }
    };

    Ok(json!({
        "systemManufacturer": manuf,
        "systemProductName":  product,
        "systemSerialNumber": serial,
        "systemVersion":      version,
        "systemUUID":         uuid,
        "systemFamily":       family,
        "cpuModel":           cpu.model_name,
    }))
}

pub async fn hostdev_list_by_caps(_params: Value) -> Result<Value, JsonRpcError> {
    Ok(json!({}))
}

pub async fn set_mom_policy_parameters(_params: Value) -> Result<Value, JsonRpcError> {
    Ok(json!({}))
}

pub async fn set_mom_policy(_params: Value) -> Result<Value, JsonRpcError> {
    Ok(json!({}))
}

pub async fn set_haircut(_params: Value) -> Result<Value, JsonRpcError> {
    Ok(json!({}))
}
