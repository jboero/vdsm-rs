//! Detect whether an SD UUID resolves to a file SD (under MNT_BASE) or a
//! block SD (an LVM VG whose name equals the SD UUID).
//!
//! Upstream vdsm tracks this via the SD's `dom_md/metadata` TYPE field,
//! but that file lives on the SD itself — chicken-and-egg for the
//! detector. We use a simpler convention: a block SD is a VG named after
//! its UUID (this is what `Host.createVG` does — engine passes the SD
//! UUID as `vgName`), and a file SD has a directory at
//! `MNT_BASE/<server>:_<export>/<sd_uuid>/`.

use std::path::PathBuf;

use tokio::process::Command;

use crate::connection::MNT_BASE;

/// State directory for block-SD sidecar metadata. We can't put per-volume
/// .meta files on the LV itself without a filesystem on it; instead we
/// shadow the file-SD layout under `/var/lib/vdsm/sd/<sd_uuid>/images/...`.
pub const BLOCK_SD_STATE_BASE: &str = "/var/lib/vdsm/sd";

#[derive(Debug, Clone)]
pub enum SdBackend {
    /// File SD: `<sd_path>` is `MNT_BASE/<server>:_<export>/<sd_uuid>`.
    File { sd_path: PathBuf },
    /// Block SD: payload is LVs in `vg_name` (= sd_uuid); per-volume
    /// metadata + image dirs live under `BLOCK_SD_STATE_BASE/<sd_uuid>/`.
    Block { vg_name: String, state_dir: PathBuf },
}

impl SdBackend {
    pub fn is_block(&self) -> bool { matches!(self, SdBackend::Block { .. }) }
}

pub async fn sd_backend(sd_uuid: &str) -> Option<SdBackend> {
    if sd_uuid.is_empty() { return None; }

    // Check for a VG with this exact name. `vgs --select` is the safest
    // form — `vgs <name>` errors loudly if the VG doesn't exist, which is
    // a normal/expected case for file SDs.
    if vg_exists(sd_uuid).await {
        return Some(SdBackend::Block {
            vg_name: sd_uuid.to_string(),
            state_dir: PathBuf::from(BLOCK_SD_STATE_BASE).join(sd_uuid),
        });
    }

    // Fall back: scan MNT_BASE for an SD dir.
    let base = std::path::Path::new(MNT_BASE);
    let mut entries = tokio::fs::read_dir(base).await.ok()?;
    while let Ok(Some(ent)) = entries.next_entry().await {
        let p = ent.path().join(sd_uuid);
        if p.is_dir() {
            return Some(SdBackend::File { sd_path: p });
        }
    }
    None
}

pub async fn vg_exists(name: &str) -> bool {
    let out = Command::new("/usr/sbin/vgs")
        .args(["--noheadings", "-o", "vg_name", "--select", &format!("vg_name={name}")])
        .output().await;
    match out {
        Ok(o) => o.status.success() && !o.stdout.is_empty()
                  && String::from_utf8_lossy(&o.stdout).trim() == name,
        Err(_) => false,
    }
}

// Privileged operations now go through supervdsmd — see
// [`vdsm_common::supervdsm`]. Read-only LVM *queries* (`vgs`/`lvs`/`pvs`)
// stay as direct `Command` calls here because they were never privileged
// (never behind sudo); only mutating/privileged ops cross the supervdsm
// boundary.
pub use vdsm_common::supervdsm::{call, run, PrivOp};
