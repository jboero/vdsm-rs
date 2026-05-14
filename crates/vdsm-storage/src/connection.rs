//! Storage server connections — the part that mounts NFS exports into
//! `/rhev/data-center/mnt/`. Engine drives this via `StoragePool.connectStorageServer`
//! with an array of connection specs:
//!
//! ```json
//! [{"id":"<uuid>", "connection":"server:/export", "iqn":"", "portal":"",
//!   "user":"", "password":"", "port":"", "vfs_type":"nfs", "protocol_version":"auto"}]
//! ```
//!
//! We translate each spec to a `mount -t <vfs> <server>:<export> <local-mountpoint>`
//! shell-out. The privileged mount call lives behind supervdsmd in real
//! upstream vdsm; for v0 we exec mount(8) directly from vdsmd_t (covered by
//! the SELinux policy module + ExecStartPre-supplied root helpers if needed).

use std::path::PathBuf;
use std::sync::OnceLock;

use serde::{Deserialize, Serialize};
use tokio::sync::RwLock;

/// Where engine expects NFS mounts to live. The full mountpoint is
/// `MNT_BASE/<server>:_<escaped_export>` (engine builds this same string
/// and uses it to address the SD).
pub const MNT_BASE: &str = "/rhev/data-center/mnt";

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct StorageServerConnection {
    /// Engine-assigned connection UUID.
    pub id: String,
    /// `server:/export` for NFS, `target:port` for iSCSI, etc.
    pub connection: String,
    /// Filesystem type passed to mount(8). For NFS this is `nfs` or `nfs4`.
    #[serde(default, rename = "vfs_type")]
    pub vfs_type: String,
    /// Mount-option string for the `-o` argument.
    #[serde(default, rename = "mnt_options")]
    pub mnt_options: String,
    /// NFS version selector engine sends; we map to mount options.
    #[serde(default)]
    pub protocol_version: String,
}

impl StorageServerConnection {
    /// Path under MNT_BASE for this connection. Engine builds the same
    /// path on its side to address the SD; substitution rule is `/` → `_`
    /// in the export path.
    pub fn mountpoint(&self) -> PathBuf {
        let escaped = self.connection.replace('/', "_");
        PathBuf::from(MNT_BASE).join(escaped)
    }
}

/// In-process registry of active storage server connections. Maps the
/// engine-assigned connection UUID to the spec we mounted. Used by
/// `getStorageServerConnectionsList` to report back to engine and by
/// `disconnectStorageServer` to find the mountpoint to unmount.
static CONNECTIONS: OnceLock<RwLock<Vec<StorageServerConnection>>> = OnceLock::new();

pub fn connections() -> &'static RwLock<Vec<StorageServerConnection>> {
    CONNECTIONS.get_or_init(|| RwLock::new(Vec::new()))
}
