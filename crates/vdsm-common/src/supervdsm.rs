//! supervdsm IPC — the privileged-operation boundary.
//!
//! `vdsmd` runs as the unprivileged `vdsm` user. A small set of storage
//! operations (mount, LVM, iSCSI, FC rescan) need root. Upstream vdsm
//! solves this with `supervdsmd`: a root daemon exposing a *closed* set
//! of operations over a Unix socket. We do the same.
//!
//! Why not sudoers? A `NOPASSWD: /usr/bin/mount` rule lets the vdsm user
//! run mount with *arbitrary* arguments — `mount --bind`, `mount -o
//! remount`, loop mounts of attacker-controlled images, etc. That's
//! root-equivalent. supervdsmd instead accepts a typed [`PrivOp`] with
//! semantic fields and builds the command itself, so the unprivileged
//! side can only ask for operations we explicitly modelled.
//!
//! Wire format: one JSON object per line (NDJSON), request then response,
//! one op per connection. The socket is `root:vdsm 0660` and supervdsmd
//! additionally checks `SO_PEERCRED` so only the vdsm uid can drive it.

use std::path::Path;

use serde::{Deserialize, Serialize};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;

/// Default socket path. supervdsmd's unit sets `RuntimeDirectory=supervdsm`
/// so `/run/supervdsm` exists root-owned, mode 0755 (traversable by all).
pub const SOCK_PATH: &str = "/run/supervdsm/sock";

/// The closed set of privileged operations. Each variant carries only
/// semantic fields; supervdsmd constructs the actual argv server-side.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum PrivOp {
    Mount {
        fstype: String,
        spec: String,
        target: String,
        #[serde(default)]
        options: String,
    },
    Umount {
        target: String,
    },
    IscsiDiscover {
        portal: String,
    },
    IscsiLogin {
        iqn: String,
        portal: String,
    },
    IscsiLogout {
        iqn: String,
        portal: String,
    },
    IscsiRescan,
    Pvcreate {
        device: String,
    },
    Vgcreate {
        vg: String,
        devices: Vec<String>,
    },
    Vgextend {
        vg: String,
        device: String,
    },
    Vgremove {
        vg: String,
    },
    Lvcreate {
        vg: String,
        lv: String,
        size_bytes: u64,
    },
    /// `active=true` → `lvchange -ay -K`; `false` → `lvchange -an`.
    Lvchange {
        vg: String,
        lv: String,
        active: bool,
    },
    Lvremove {
        vg: String,
        lv: String,
    },
    FcScan,
    MultipathList,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PrivResult {
    pub ok: bool,
    pub code: i32,
    #[serde(default)]
    pub stdout: String,
    #[serde(default)]
    pub stderr: String,
}

impl PrivResult {
    pub fn failure(msg: impl Into<String>) -> Self {
        Self { ok: false, code: -1, stdout: String::new(), stderr: msg.into() }
    }
}

/// Connect to supervdsmd, send one op, await its result. Returns an
/// error only for transport failures (socket missing, IO); a command
/// that ran but exited non-zero comes back as `Ok(PrivResult{ok:false})`.
pub async fn call(op: &PrivOp) -> std::io::Result<PrivResult> {
    call_at(SOCK_PATH, op).await
}

pub async fn call_at(sock: impl AsRef<Path>, op: &PrivOp) -> std::io::Result<PrivResult> {
    let stream = UnixStream::connect(sock.as_ref()).await?;
    let (rd, mut wr) = stream.into_split();

    let mut line = serde_json::to_string(op)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    line.push('\n');
    wr.write_all(line.as_bytes()).await?;
    wr.flush().await?;

    let mut reader = BufReader::new(rd);
    let mut resp = String::new();
    reader.read_line(&mut resp).await?;
    serde_json::from_str(resp.trim())
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))
}

/// Convenience for callers that just want stdout-or-nothing, matching the
/// old `sudo()` helper's `Option<String>` ergonomics so call sites stay
/// compact. `None` = transport error OR non-zero exit.
pub async fn run(op: PrivOp) -> Option<String> {
    match call(&op).await {
        Ok(r) if r.ok => Some(r.stdout),
        Ok(r) => {
            tracing::warn!(?op, code = r.code, stderr = %r.stderr.trim(),
                "supervdsm op failed");
            None
        }
        Err(e) => {
            tracing::warn!(?op, error = %e, "supervdsm transport error");
            None
        }
    }
}
