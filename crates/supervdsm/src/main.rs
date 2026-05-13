use clap::Parser;
use tracing::info;

use vdsm_common::{logging, VDSM_RS_VERSION};

#[derive(Parser, Debug)]
#[command(name = "supervdsmd", version, about = "vdsm-rs privileged helper")]
struct Args {
    /// Override log level (trace|debug|info|warn|error).
    #[arg(long, default_value = "info")]
    log_level: String,
}

fn main() -> anyhow::Result<()> {
    let args = Args::parse();
    logging::init(&args.log_level);

    info!(
        version = VDSM_RS_VERSION,
        "supervdsmd starting (scaffold; no IPC wired)"
    );

    Ok(())
}
