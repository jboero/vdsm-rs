use std::path::PathBuf;

use clap::Parser;
use tracing::{info, warn};

use vdsm_common::{config::Config, logging, VDSM_RS_VERSION};
use vdsm_rpc::{Dispatcher, Framing, Server, ServerConfig as RpcConfig};

#[derive(Parser, Debug)]
#[command(name = "vdsmd", version, about = "vdsm-rs host daemon")]
struct Args {
    /// Path to vdsm.toml.
    #[arg(short, long, default_value = "/etc/vdsm/vdsm.toml")]
    config: PathBuf,

    /// Override log level (trace|debug|info|warn|error).
    #[arg(long)]
    log_level: Option<String>,
}

fn main() -> anyhow::Result<()> {
    let args = Args::parse();

    let cfg = if args.config.exists() {
        Config::load(&args.config)?
    } else {
        Config::default()
    };

    let log_level = args
        .log_level
        .as_deref()
        .unwrap_or(&cfg.daemon.log_level);
    logging::init(log_level);

    if !args.config.exists() {
        warn!(
            "config {} not found; using built-in defaults",
            args.config.display()
        );
    }

    info!(
        version = VDSM_RS_VERSION,
        types = vdsm_schema::TYPE_COUNT,
        verbs = vdsm_schema::VERB_COUNT,
        "vdsmd starting, schema loaded {} types, {} verbs",
        vdsm_schema::TYPE_COUNT,
        vdsm_schema::VERB_COUNT,
    );

    info!(
        rpc_bind = %cfg.rpc.bind,
        rpc_port = cfg.rpc.port,
        libvirt_uri = %cfg.libvirt.uri,
        "config loaded"
    );

    // The RPC server is async; everything else stays sync until it needs
    // not to be. Multi-thread runtime — engine is happy to fan many
    // long-lived connections at us.
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;

    let dispatcher = Dispatcher::builder()
        .register("Host.ping2", vdsm_host::ping2)
        .register("Host.getCapabilities", vdsm_host::get_capabilities)
        .register("Host.getStats", vdsm_host::get_stats)
        .register("Host.getAllVmStats", vdsm_host::get_all_vm_stats)
        .register("Host.setupNetworks", vdsm_host::setup_networks)
        .register("Host.setSafeNetworkConfig", vdsm_host::set_safe_network_config)
        .register("Host.getHardwareInfo", vdsm_host::get_hardware_info)
        .register("Host.hostdevListByCaps", vdsm_host::hostdev_list_by_caps)
        .register("Host.setMOMPolicyParameters", vdsm_host::set_mom_policy_parameters)
        .register("Host.setMOMPolicy", vdsm_host::set_mom_policy)
        .register("VM.create", vdsm_virt::vm_create)
        .register("VM.destroy", vdsm_virt::vm_destroy)
        .register("VM.shutdown", vdsm_virt::vm_shutdown)
        .register("VM.cont", vdsm_virt::vm_cont)
        .register("VM.pause", vdsm_virt::vm_pause)
        .register("VM.getStats", vdsm_virt::vm_get_stats)
        .register("Host.dumpxmls", vdsm_virt::dump_xmls)
        .build();

    let server = Server::new(
        RpcConfig {
            bind: cfg.rpc.bind.clone(),
            port: cfg.rpc.port,
            tls_enabled: cfg.rpc.tls_enabled,
            tls_cert: cfg.rpc.tls_cert.clone().into(),
            tls_key: cfg.rpc.tls_key.clone().into(),
            framing: Framing::from_str_lossy(&cfg.rpc.framing),
        },
        dispatcher,
    );

    runtime.block_on(async move {
        tokio::select! {
            res = server.serve() => {
                if let Err(e) = res {
                    tracing::error!(error = %e, "rpc server exited");
                    return Err::<(), anyhow::Error>(e);
                }
                Ok(())
            }
            _ = tokio::signal::ctrl_c() => {
                info!("ctrl-c received, shutting down");
                Ok(())
            }
        }
    })?;

    Ok(())
}
