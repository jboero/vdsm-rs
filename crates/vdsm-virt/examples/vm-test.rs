//! Direct-call test: drive vdsm-virt's VM.create end-to-end against the
//! local libvirt without going through the JSON-RPC transport. Proves the
//! vmParams → domain XML → libvirt translation works in isolation.
//!
//! Usage:  cargo run -p vdsm-virt --example vm-test [vm_id] [vm_name] [mem_mb] [vcpus]

use serde_json::json;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let mut args = std::env::args().skip(1);
    let vm_id = args
        .next()
        .unwrap_or_else(|| "11111111-2222-3333-4444-555555555555".into());
    let vm_name = args.next().unwrap_or_else(|| format!("vdsm-rs-test-{}", &vm_id[..8]));
    let mem_mb: u64 = args
        .next()
        .and_then(|s| s.parse().ok())
        .unwrap_or(256);
    let vcpus: u32 = args.next().and_then(|s| s.parse().ok()).unwrap_or(1);

    let params = json!({
        "vmID": vm_id,
        "vmParams": {
            "vmName": vm_name,
            "memSize": mem_mb,
            "smp": vcpus,
        }
    });

    println!("==> VM.create");
    println!("{}", serde_json::to_string_pretty(&params)?);
    let resp = vdsm_virt::vm_create(params.clone()).await
        .map_err(|e| anyhow::anyhow!("vm_create failed: {:?}", e))?;
    println!("<== {}", serde_json::to_string_pretty(&resp)?);

    println!("\n==> VM.getStats");
    let resp = vdsm_virt::vm_get_stats(json!({"vmID": vm_id})).await
        .map_err(|e| anyhow::anyhow!("vm_get_stats failed: {:?}", e))?;
    println!("<== {}", serde_json::to_string_pretty(&resp)?);

    println!("\nVM defined and started. Inspect with:");
    println!("  virsh -c qemu:///system list");
    println!("  virsh -c qemu:///system dumpxml {}", vm_name);
    println!("\nTo tear down:");
    println!("  cargo run -p vdsm-virt --example vm-test-destroy -- {}", vm_id);
    Ok(())
}
