//! supervdsmd — the only root component of vdsm-rs.
//!
//! Listens on a Unix socket and executes a closed set of privileged
//! storage operations on behalf of the unprivileged `vdsmd`. Access is
//! gated two ways: filesystem perms (`root:vdsm 0660`) and an
//! `SO_PEERCRED` check that the connecting process runs as the vdsm uid.

mod exec;

use std::os::unix::fs::PermissionsExt;
use std::path::Path;

use clap::Parser;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};
use tracing::{error, info, warn};

use vdsm_common::supervdsm::{PrivOp, PrivResult, SOCK_PATH};
use vdsm_common::{logging, VDSM_RS_VERSION};

#[derive(Parser, Debug)]
#[command(name = "supervdsmd", version, about = "vdsm-rs privileged helper")]
struct Args {
    /// Override log level (trace|debug|info|warn|error).
    #[arg(long, default_value = "info")]
    log_level: String,

    /// Socket path to listen on.
    #[arg(long, default_value = SOCK_PATH)]
    socket: String,

    /// Service account whose uid is allowed to connect.
    #[arg(long, default_value = "vdsm")]
    user: String,
}

/// Resolve `(uid, gid)` for a username from /etc/passwd — no libc/NSS
/// dependency, and the vdsm account is always a local sysuser anyway.
fn lookup_user(name: &str) -> anyhow::Result<(u32, u32)> {
    let passwd = std::fs::read_to_string("/etc/passwd")?;
    for line in passwd.lines() {
        let mut f = line.split(':');
        if f.next() == Some(name) {
            let _passwd = f.next();
            let uid = f.next().and_then(|s| s.parse().ok());
            let gid = f.next().and_then(|s| s.parse().ok());
            if let (Some(uid), Some(gid)) = (uid, gid) {
                return Ok((uid, gid));
            }
        }
    }
    anyhow::bail!("user {name} not found in /etc/passwd")
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args = Args::parse();
    logging::init(&args.log_level);

    let (allowed_uid, vdsm_gid) = lookup_user(&args.user)?;

    // Fresh socket each start — a stale file from an unclean shutdown
    // would make bind() fail with EADDRINUSE.
    let sock_path = args.socket.clone();
    if Path::new(&sock_path).exists() {
        let _ = std::fs::remove_file(&sock_path);
    }
    let listener = UnixListener::bind(&sock_path)?;

    // root:vdsm 0660 — only root and the vdsm group can even open it.
    std::os::unix::fs::chown(&sock_path, Some(0), Some(vdsm_gid))?;
    std::fs::set_permissions(&sock_path, std::fs::Permissions::from_mode(0o660))?;

    info!(
        version = VDSM_RS_VERSION,
        socket = %sock_path,
        allowed_uid,
        "supervdsmd listening"
    );

    loop {
        match listener.accept().await {
            Ok((stream, _)) => {
                tokio::spawn(async move {
                    if let Err(e) = handle(stream, allowed_uid).await {
                        warn!(error = %e, "connection handler error");
                    }
                });
            }
            Err(e) => {
                error!(error = %e, "accept failed");
            }
        }
    }
}

async fn handle(stream: UnixStream, allowed_uid: u32) -> anyhow::Result<()> {
    // Authenticate by peer credentials before reading a single byte.
    let cred = stream.peer_cred()?;
    if cred.uid() != allowed_uid {
        warn!(peer_uid = cred.uid(), allowed_uid, "rejecting unauthorized peer");
        let (_, mut wr) = stream.into_split();
        let denied = PrivResult::failure("unauthorized: peer uid not permitted");
        let mut line = serde_json::to_string(&denied)?;
        line.push('\n');
        let _ = wr.write_all(line.as_bytes()).await;
        return Ok(());
    }

    let (rd, mut wr) = stream.into_split();
    let mut reader = BufReader::new(rd);
    let mut req = String::new();
    reader.read_line(&mut req).await?;
    if req.trim().is_empty() {
        return Ok(());
    }

    let result = match serde_json::from_str::<PrivOp>(req.trim()) {
        Ok(op) => {
            info!(?op, "executing privileged op");
            exec::execute(op).await
        }
        Err(e) => PrivResult::failure(format!("malformed request: {e}")),
    };

    let mut line = serde_json::to_string(&result)?;
    line.push('\n');
    wr.write_all(line.as_bytes()).await?;
    wr.flush().await?;
    Ok(())
}
