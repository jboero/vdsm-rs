//! Tear down what `vm-test` created.

use serde_json::json;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let vm_id = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "11111111-2222-3333-4444-555555555555".into());

    // Re-register so destroy can find the name. In a real session vdsm-virt's
    // registry would already hold this; this example runs in a fresh process.
    let _ = vdsm_virt::vm_create(json!({
        "vmID": vm_id,
        "vmParams": {"vmName": format!("vdsm-rs-test-{}", &vm_id[..8])},
    })).await; // ignore "already running"

    let resp = vdsm_virt::vm_destroy(json!({"vmID": vm_id})).await
        .map_err(|e| anyhow::anyhow!("vm_destroy: {:?}", e))?;
    println!("destroyed: {}", resp);
    Ok(())
}
