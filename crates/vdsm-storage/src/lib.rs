//! Storage subsystem for vdsm-rs.
//!
//! v0 supports file-based (NFS) storage domains only. LVM/SAN, Gluster,
//! and sanlock-backed distributed locking are explicitly deferred —
//! see `PROMPT.md` for the scope cuts.
//!
//! oVirt's on-disk SD layout under the NFS mountpoint:
//!
//! ```text
//! /rhev/data-center/mnt/<server>:_<export_path>/<sd_uuid>/
//! ├── dom_md/
//! │   ├── metadata     SD-level metadata (key=value)
//! │   ├── ids          sanlock IDs (stubbed for v0)
//! │   ├── leases       sanlock leases (stubbed)
//! │   └── inbox/outbox
//! ├── master/
//! │   ├── tasks/       SPM async-task state
//! │   └── vms/         VM template OVFs
//! └── images/
//!     └── <image_uuid>/
//!         ├── <vol_uuid>          qcow2/raw payload
//!         ├── <vol_uuid>.meta     volume metadata (key=value)
//!         └── <vol_uuid>.lease    sanlock lease (stubbed)
//! ```

pub mod block;
pub mod connection;
pub mod domain;
pub mod sd_backend;
pub mod verbs;
pub mod volumes;

pub use block::{
    create_vg, extend_vg, fc_scan, get_devices_visibility, get_lvm_vg_list,
    get_path_list_status, get_vg_info, iscsi_discover_send_targets, iscsi_login,
    iscsi_logout, iscsi_rescan, remove_vg,
};

pub use domain::{
    activate_storage_domain, attach_storage_domain, connect_storage_pool,
    create_storage_domain, create_storage_pool, get_spm_status,
    get_storage_domain_info, get_storage_domain_stats, get_storage_pool_info,
    hsm_get_all_tasks_statuses, spm_start,
};
pub use verbs::{
    connect_storage_server, disconnect_storage_server, discover_send_targets,
    get_device_list, get_storage_server_connections_list,
};
pub use volumes::{
    image_delete, image_delete_volumes, image_prepare, image_teardown,
    volume_create, volume_delete, volume_get_info, volume_get_size,
};
