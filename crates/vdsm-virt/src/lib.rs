//! libvirt wrapper and VM lifecycle for vdsm-rs.
//!
//! Shells out to `virsh` rather than linking libvirt — clean process
//! boundary, easy error mapping via exit codes, no FFI build deps. Can
//! be swapped for the `virt` crate later without rippling.

use std::collections::HashMap;
use std::sync::OnceLock;

use tokio::sync::RwLock;

pub mod domain_xml;
pub mod verbs;

/// Engine surfaces VM state as a string in getAllVmStats — these are
/// the values it knows. Anything else and the UI reports "Unknown".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VmState {
    WaitForLaunch,
    PoweringUp,
    Up,
    Down,
    Paused,
    PoweringDown,
}

impl VmState {
    pub fn as_engine_str(self) -> &'static str {
        match self {
            VmState::WaitForLaunch => "WaitForLaunch",
            VmState::PoweringUp => "Powering up",
            VmState::Up => "Up",
            VmState::Down => "Down",
            VmState::Paused => "Paused",
            VmState::PoweringDown => "Powering down",
        }
    }
}

/// What we keep in-process for each VM. The libvirt domain itself owns
/// the truth; this is just enough to fill out getAllVmStats without
/// re-shelling out to virsh on every poll.
#[derive(Debug, Clone)]
pub struct VmRecord {
    pub vm_id: String,
    pub vm_name: String,
    pub mem_size_mb: u64,
    pub vcpus: u32,
    pub state: VmState,
    pub created_secs: u64,
}

/// Global VM registry. `OnceLock` so dispatch handlers can reach it
/// without threading state through the Dispatcher closure type.
static REGISTRY: OnceLock<RwLock<HashMap<String, VmRecord>>> = OnceLock::new();

pub fn registry() -> &'static RwLock<HashMap<String, VmRecord>> {
    REGISTRY.get_or_init(|| RwLock::new(HashMap::new()))
}

pub use verbs::{dump_xmls, vm_cont, vm_create, vm_destroy, vm_get_stats, vm_pause, vm_shutdown};
