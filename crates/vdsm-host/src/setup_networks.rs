//! `Host.setupNetworks` — engine's InitVdsOnUpCommand calls this right
//! after caps to reconcile cluster networks against the host's view.
//! No-op for now: we accept whatever the engine pushes and report
//! success so the host transitions from Initializing → Up. The engine
//! then re-reads caps and finds ovirtmgmt already there.

use serde_json::{json, Value};

use vdsm_rpc::JsonRpcError;

pub async fn setup_networks(_params: Value) -> Result<Value, JsonRpcError> {
    Ok(json!({}))
}

pub async fn set_safe_network_config(_params: Value) -> Result<Value, JsonRpcError> {
    Ok(json!({}))
}
