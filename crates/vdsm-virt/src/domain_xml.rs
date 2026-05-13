//! Synthesize libvirt domain XML from engine vmParams.
//!
//! v0: stateless — no disks attached. Engine sends a huge `vmParams` dict
//! with `devices`, `drives`, `displaySpecParams` etc.; we read only what
//! libvirt needs to boot a domain. Missing fields use safe fallbacks so
//! a sparse engine request still produces a startable VM.

use serde_json::Value;

const DEFAULT_MEM_MB: u64 = 1024;
const DEFAULT_VCPUS: u32 = 1;

/// Pull the first defined string at any of these JSON path tails.
fn first_str<'a>(p: &'a Value, keys: &[&str]) -> Option<&'a str> {
    for k in keys {
        if let Some(s) = p.get(*k).and_then(Value::as_str) {
            return Some(s);
        }
    }
    None
}

fn first_u64(p: &Value, keys: &[&str]) -> Option<u64> {
    for k in keys {
        if let Some(v) = p.get(*k) {
            if let Some(n) = v.as_u64() {
                return Some(n);
            }
            if let Some(s) = v.as_str() {
                if let Ok(n) = s.parse::<u64>() {
                    return Some(n);
                }
            }
        }
    }
    None
}

pub struct DomainSpec {
    pub uuid: String,
    pub name: String,
    pub mem_mb: u64,
    pub vcpus: u32,
}

impl DomainSpec {
    pub fn from_vm_params(vm_id: &str, params: &Value) -> Self {
        // params may be either the top-level vmParams dict directly, or
        // {vmID, vmParams: {...}} — engine sends the latter at VM.create.
        let p = params.get("vmParams").unwrap_or(params);

        let name = first_str(p, &["vmName"])
            .unwrap_or(vm_id)
            .to_string();
        let mem_mb = first_u64(p, &["memSize"]).unwrap_or(DEFAULT_MEM_MB);
        let smp = first_u64(p, &["smp", "maxVCpus"])
            .unwrap_or(DEFAULT_VCPUS as u64) as u32;

        DomainSpec {
            uuid: vm_id.to_string(),
            name,
            mem_mb,
            vcpus: smp.max(1),
        }
    }

    pub fn to_xml(&self) -> String {
        // Minimal stateless q35 domain — no disks, no network. Will boot
        // to "no bootable device" but registers as running in libvirt.
        // That's enough for engine getAllVmStats to report VM=Up.
        format!(
            r#"<domain type='kvm'>
  <name>{name}</name>
  <uuid>{uuid}</uuid>
  <memory unit='MiB'>{mem}</memory>
  <currentMemory unit='MiB'>{mem}</currentMemory>
  <vcpu placement='static'>{vcpus}</vcpu>
  <os>
    <type arch='x86_64' machine='q35'>hvm</type>
    <boot dev='hd'/>
    <boot dev='cdrom'/>
    <boot dev='network'/>
  </os>
  <features>
    <acpi/>
    <apic/>
  </features>
  <cpu mode='host-passthrough' check='none'/>
  <clock offset='utc'/>
  <on_poweroff>destroy</on_poweroff>
  <on_reboot>restart</on_reboot>
  <on_crash>destroy</on_crash>
  <devices>
    <emulator>/usr/bin/qemu-system-x86_64</emulator>
    <controller type='usb' model='qemu-xhci'/>
    <serial type='pty'><target port='0'/></serial>
    <console type='pty'><target type='serial' port='0'/></console>
    <input type='tablet' bus='usb'/>
    <graphics type='vnc' port='-1' autoport='yes' listen='127.0.0.1'/>
    <video><model type='virtio'/></video>
    <memballoon model='virtio'/>
  </devices>
</domain>
"#,
            name = xml_escape(&self.name),
            uuid = self.uuid,
            mem = self.mem_mb,
            vcpus = self.vcpus,
        )
    }
}

fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}
