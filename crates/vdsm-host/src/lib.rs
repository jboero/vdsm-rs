//! Host capability detection, stats sampling, NUMA, and CPU topology.

// The Host.getCapabilities response has grown past serde_json's default
// macro recursion limit; bump for this crate only.
#![recursion_limit = "512"]

pub mod caps;
pub mod hardware;
pub mod ping;
pub mod setup_networks;
pub mod stats;
pub mod sysinfo;
pub mod vm_stats;

pub use caps::get_capabilities;
pub use hardware::{get_hardware_info, hostdev_list_by_caps, set_mom_policy, set_mom_policy_parameters};
pub use ping::ping2;
pub use setup_networks::{set_safe_network_config, setup_networks};
pub use stats::get_stats;
pub use vm_stats::get_all_vm_stats;
