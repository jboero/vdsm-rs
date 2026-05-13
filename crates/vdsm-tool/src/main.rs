//! `vdsm-tool` — compatibility shim for ovirt-engine's host-deploy.
//!
//! Real upstream vdsm-tool is a sprawling Python entry point that runs
//! configurators, generates host UUIDs, manages bond / network state,
//! and so on. host-deploy only invokes a small handful of subcommands,
//! and almost all of them are "do this side-effect, exit 0 if fine". For
//! v0 we acknowledge the call and return success, leaving the actual
//! plumbing to the daemon proper. As real subsystems land, individual
//! subcommands graduate from no-op to real behavior.
//!
//! Subcommands invoked by host-deploy (per oVirt master read 2026-04-30):
//!
//!   vdsm-tool configure --force        — run all configurators
//!   vdsm-tool config-lvm-filter -y     — write /etc/lvm/lvm.conf filter
//!   vdsm-tool ovn-config <c> <i> <fqdn> — Open vSwitch external-ids
//!   vdsm-tool is-configured            — exit 0 if all configurators happy
//!   vdsm-tool validate-config          — same, stricter
//!   vdsm-tool register                 — engine registration
//!   vdsm-tool vdsm-id                  — print /etc/vdsm/vdsm.id
//!
//! Anything else gets a polite "not implemented" on stderr and exit 0
//! (host-deploy ignores stdout/stderr; an exit-0 keeps it moving).

use std::fs;
use std::io::Write;
use std::path::Path;
use std::process::Command;

use clap::{Parser, Subcommand};
use tracing::{info, warn};

const VDSM_ID_PATH: &str = "/etc/vdsm/vdsm.id";

#[derive(Parser, Debug)]
#[command(
    name = "vdsm-tool",
    version,
    about = "vdsm-rs host configuration helper (host-deploy compat shim)",
    disable_help_subcommand = true
)]
struct Args {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand, Debug)]
enum Cmd {
    /// Run all configurators (no-op in v0; vdsmd self-configures).
    Configure {
        #[arg(long)]
        force: bool,
    },
    /// Write the multipath/LVM device filter (v0: no-op).
    #[command(name = "config-lvm-filter")]
    ConfigLvmFilter {
        /// Assume "yes" to all prompts (real tool uses this; we ignore).
        #[arg(short = 'y')]
        yes: bool,
    },
    /// OVN configuration. v0 doesn't ship OVN; report that and exit 0.
    #[command(name = "ovn-config")]
    OvnConfig {
        central: Option<String>,
        tunnel_iface: Option<String>,
        host_fqdn: Option<String>,
    },
    /// Exit 0 if every configurator reports configured. v0: always 0.
    #[command(name = "is-configured")]
    IsConfigured,
    /// Stricter form of is-configured. v0: always 0.
    #[command(name = "validate-config")]
    ValidateConfig,
    /// Engine registration handshake. v0: no-op (engine registration
    /// happens via the JSON-RPC channel once vdsmd is up, not via a
    /// pre-flight CLI call).
    Register {
        /// engine FQDN (ignored for now)
        #[arg(long)]
        engine_fqdn: Option<String>,
    },
    /// Print /etc/vdsm/vdsm.id, generating it from the system UUID
    /// if absent. host-deploy uses the printed value to identify the
    /// host on the engine side.
    #[command(name = "vdsm-id")]
    VdsmId,

    /// Generate a self-signed RSA 2048 cert + key suitable for booting
    /// vdsmd locally without an engine to push real certs. Shells out
    /// to `openssl req -x509`.
    #[command(name = "gen-test-certs")]
    GenTestCerts {
        /// Where to write the cert PEM. Default matches engine push path.
        #[arg(long, default_value = "/etc/pki/vdsm/certs/vdsmcert.pem")]
        cert: String,
        /// Where to write the key PEM.
        #[arg(long, default_value = "/etc/pki/vdsm/keys/vdsmkey.pem")]
        key: String,
        /// Subject CN. Defaults to system hostname.
        #[arg(long)]
        cn: Option<String>,
        /// Validity in days.
        #[arg(long, default_value_t = 365)]
        days: u32,
    },

    /// Catch-all for any subcommand we haven't modeled yet.
    #[command(external_subcommand)]
    Unknown(Vec<String>),
}

fn main() -> anyhow::Result<()> {
    // Best-effort tracing init; vdsm-tool is invoked non-interactively
    // by host-deploy so we keep it quiet by default.
    let _ = tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn")),
        )
        .with_writer(std::io::stderr)
        .try_init();

    let args = Args::parse();
    match args.cmd {
        Cmd::Configure { force } => {
            info!(force, "vdsm-tool configure: nothing to do (vdsmd self-configures)");
        }
        Cmd::ConfigLvmFilter { yes: _ } => {
            info!("vdsm-tool config-lvm-filter: skipped (NFS-only v0; no LVM hosts)");
        }
        Cmd::OvnConfig { .. } => {
            warn!("vdsm-tool ovn-config: OVN support deferred; exiting cleanly");
        }
        Cmd::IsConfigured | Cmd::ValidateConfig => {
            // Both succeed silently in v0.
        }
        Cmd::Register { engine_fqdn } => {
            info!(?engine_fqdn, "vdsm-tool register: handled at JSON-RPC layer");
        }
        Cmd::VdsmId => {
            let id = ensure_vdsm_id()?;
            println!("{id}");
        }
        Cmd::GenTestCerts { cert, key, cn, days } => {
            gen_test_certs(&cert, &key, cn.as_deref(), days)?;
        }
        Cmd::Unknown(argv) => {
            let name = argv.first().map(String::as_str).unwrap_or("<empty>");
            let mut stderr = std::io::stderr().lock();
            let _ = writeln!(
                stderr,
                "vdsm-tool: subcommand {name:?} not implemented in vdsm-rs v0; ignoring"
            );
        }
    }
    Ok(())
}

/// Read `/etc/vdsm/vdsm.id`; if missing, generate it from the system UUID
/// (mirroring real upstream behavior so engine sees a stable host id).
fn ensure_vdsm_id() -> anyhow::Result<String> {
    let path = Path::new(VDSM_ID_PATH);
    if let Ok(existing) = fs::read_to_string(path) {
        let trimmed = existing.trim();
        if !trimmed.is_empty() {
            return Ok(trimmed.into());
        }
    }
    let id = read_system_uuid()?;
    if let Some(parent) = path.parent() {
        let _ = fs::create_dir_all(parent);
    }
    if let Err(e) = fs::write(path, format!("{id}\n")) {
        warn!(%e, "could not persist {}; returning value anyway", path.display());
    }
    Ok(id)
}

fn gen_test_certs(
    cert_path: &str,
    key_path: &str,
    cn: Option<&str>,
    days: u32,
) -> anyhow::Result<()> {
    let hostname = std::fs::read_to_string("/proc/sys/kernel/hostname")
        .map(|s| s.trim().to_string())
        .unwrap_or_else(|_| "localhost".into());
    let cn = cn.unwrap_or(&hostname);

    for p in [cert_path, key_path] {
        if let Some(parent) = Path::new(p).parent() {
            fs::create_dir_all(parent).ok();
        }
    }

    let status = Command::new("openssl")
        .args([
            "req",
            "-x509",
            "-newkey",
            "rsa:2048",
            "-keyout",
            key_path,
            "-out",
            cert_path,
            "-days",
            &days.to_string(),
            "-nodes",
            "-subj",
            &format!("/CN={cn}"),
            "-addext",
            &format!("subjectAltName=DNS:{cn},DNS:localhost,IP:127.0.0.1"),
        ])
        .status()?;
    if !status.success() {
        anyhow::bail!("openssl req failed (rc={:?})", status.code());
    }

    // Match the perms + ownership engine's vdsm-certificates ansible
    // role uses when it pushes real certs. Without this the daemon (run
    // as user `vdsm`) can't read the key it's supposed to terminate
    // TLS with.
    chmod_path(cert_path, 0o640)?;
    chmod_path(key_path, 0o440)?;
    if running_as_root() {
        if let Some((uid, gid)) = lookup_vdsm_kvm() {
            chown_path(cert_path, uid, gid);
            chown_path(key_path, uid, gid);
        } else {
            warn!(
                "vdsm:kvm not found in /etc/passwd+/etc/group; certs left as root-owned. \
                 Install the vdsm-rs RPM (which ships the sysusers config) before running \
                 gen-test-certs, or chown manually."
            );
        }
    } else {
        warn!(
            "gen-test-certs not running as root; ownership unchanged. The vdsm daemon \
             (running as user vdsm) will not be able to read these certs unless you \
             chown them yourself."
        );
    }

    info!(cert = cert_path, key = key_path, cn, days, "wrote test certs");
    Ok(())
}

fn chmod_path(path: &str, mode: u32) -> anyhow::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let meta = fs::metadata(path)?;
    let mut perms = meta.permissions();
    perms.set_mode(mode);
    fs::set_permissions(path, perms)?;
    Ok(())
}

fn chown_path(path: &str, uid: u32, gid: u32) {
    // Use the chown(2) syscall via libc; we don't want a heavy dep just
    // for this so we shell to /usr/bin/chown.
    let _ = std::process::Command::new("/usr/bin/chown")
        .arg(format!("{uid}:{gid}"))
        .arg(path)
        .status();
}

fn running_as_root() -> bool {
    // SAFETY: getuid is always safe; never returns an error.
    unsafe { libc_getuid() == 0 }
}

extern "C" {
    #[link_name = "getuid"]
    fn libc_getuid() -> u32;
}

fn lookup_vdsm_kvm() -> Option<(u32, u32)> {
    let uid = read_passwd_uid("vdsm")?;
    let gid = read_group_gid("kvm")?;
    Some((uid, gid))
}

fn read_passwd_uid(name: &str) -> Option<u32> {
    let raw = fs::read_to_string("/etc/passwd").ok()?;
    for line in raw.lines() {
        let mut it = line.split(':');
        if it.next()? == name {
            let _passwd = it.next()?;
            let uid = it.next()?.parse().ok()?;
            return Some(uid);
        }
    }
    None
}

fn read_group_gid(name: &str) -> Option<u32> {
    let raw = fs::read_to_string("/etc/group").ok()?;
    for line in raw.lines() {
        let mut it = line.split(':');
        if it.next()? == name {
            let _passwd = it.next()?;
            let gid = it.next()?.parse().ok()?;
            return Some(gid);
        }
    }
    None
}

fn read_system_uuid() -> anyhow::Result<String> {
    // Preferred: the kernel-exposed product_uuid (no subprocess, no root
    // required on most Fedora installs).
    for path in &[
        "/sys/class/dmi/id/product_uuid",
        "/proc/device-tree/system-id",
    ] {
        if let Ok(s) = fs::read_to_string(path) {
            let t = s.trim();
            if !t.is_empty() {
                return Ok(t.into());
            }
        }
    }
    // Fall back to dmidecode the way real vdsm-tool does.
    let out = Command::new("dmidecode")
        .args(["-s", "system-uuid"])
        .output()?;
    if !out.status.success() {
        anyhow::bail!(
            "dmidecode -s system-uuid failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
    let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if s.is_empty() {
        anyhow::bail!("dmidecode returned an empty system-uuid");
    }
    Ok(s)
}
