//! `Host.ping2` — keepalive. Engine sends this every few seconds while
//! the host is up; absence of a response within ~30s marks the host
//! Non-Responsive.

use serde_json::{json, Value};

use vdsm_rpc::JsonRpcError;

pub async fn ping2(_params: Value) -> Result<Value, JsonRpcError> {
    Ok(json!({
        "status": { "code": 0, "message": "Done" }
    }))
}
