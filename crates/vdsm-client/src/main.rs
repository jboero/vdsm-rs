//! `vdsm-client` — thin JSON-RPC CLI for vdsm-rs.
//!
//! Real upstream vdsm-client is a Python wrapper that opens a TLS
//! connection to vdsmd, builds a JSON-RPC request, prints the response.
//! hosted-engine's playbooks invoke things like:
//!
//!     vdsm-client Volume getInfo storagepoolID=... ...
//!     vdsm-client Image  prepare  storagepoolID=... ...
//!
//! For v0 the daemon's RPC listener isn't wired yet, so we accept the
//! same command shape, log it, and return a structured "not implemented"
//! payload on stdout. host-deploy doesn't exec vdsm-client (only
//! hosted-engine setup does), so this is purely so the binary exists in
//! `$PATH` and `which vdsm-client` succeeds during host-deploy probes.

use clap::Parser;
use tracing::warn;

#[derive(Parser, Debug)]
#[command(
    name = "vdsm-client",
    version,
    about = "vdsm-rs JSON-RPC CLI (compatibility shim)",
    trailing_var_arg = true,
    disable_help_subcommand = true
)]
struct Args {
    /// Connection host (engine or 'localhost'); ignored in v0.
    #[arg(short = 'a', long, default_value = "localhost")]
    address: String,

    /// Connection port (default 54321 matches upstream).
    #[arg(short = 'p', long, default_value_t = 54321)]
    port: u16,

    /// JSON-RPC namespace (e.g. Host, VM, Volume, Image, StorageDomain).
    namespace: Option<String>,

    /// Method name on the namespace (e.g. getCapabilities, ping2).
    method: Option<String>,

    /// Positional `key=value` arguments forwarded as JSON-RPC params.
    args: Vec<String>,
}

fn main() -> anyhow::Result<()> {
    let _ = tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn")),
        )
        .with_writer(std::io::stderr)
        .try_init();

    let args = Args::parse();
    let (Some(ns), Some(method)) = (args.namespace.as_deref(), args.method.as_deref()) else {
        eprintln!("vdsm-client: usage: vdsm-client <Namespace> <Method> [key=value...]");
        std::process::exit(2);
    };
    let verb = format!("{ns}.{method}");

    warn!(
        verb,
        host = %args.address,
        port = args.port,
        argc = args.args.len(),
        "vdsm-client: JSON-RPC transport not implemented in v0"
    );

    // Mirror the upstream client's response shape so any caller blindly
    // parsing JSON gets a consistent failure rather than an empty stdout.
    println!(
        "{{\"status\":{{\"code\":-32601,\"message\":\"vdsm-rs v0: {} not implemented\"}}}}",
        verb
    );
    std::process::exit(1);
}
