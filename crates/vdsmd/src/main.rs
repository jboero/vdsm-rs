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
        .register("VM.setTicket", vdsm_virt::vm_set_ticket)
        .register("VM.updateDevice", vdsm_virt::vm_update_device)
        .register("VM.hotplugDisk", vdsm_virt::vm_update_device)
        .register("VM.hotunplugDisk", vdsm_virt::vm_update_device)
        .register("VM.hotplugNic", vdsm_virt::vm_update_device)
        .register("VM.hotunplugNic", vdsm_virt::vm_update_device)
        // Live migration — libvirt qemu+tls transport drives the data
        // transfer; we surface progress to engine via getStats.
        .register("VM.migrate", vdsm_virt::vm_migrate)
        .register("VM.migrationCreate", vdsm_virt::vm_migration_create)
        .register("VM.migrationCancel", vdsm_virt::vm_migration_cancel)
        .register("VM.migrateCancel", vdsm_virt::vm_migration_cancel)
        .register("VM.migrateChangeParams", vdsm_virt::vm_migrate_change_params)
        .register("VM.migrateStatus", vdsm_virt::vm_migrate_status)
        .register("Host.dumpxmls", vdsm_virt::dump_xmls)
        // Storage phase 1 — connection-level verbs only (no SD/pool yet).
        .register("StoragePool.connectStorageServer", vdsm_storage::connect_storage_server)
        .register("StoragePool.disconnectStorageServer", vdsm_storage::disconnect_storage_server)
        .register("Host.getStorageServerConnectionsList", vdsm_storage::get_storage_server_connections_list)
        .register("Host.discoverSendTargets", vdsm_storage::discover_send_targets)
        .register("Host.getDeviceList", vdsm_storage::get_device_list)
        // Storage phase 2 — SD lifecycle
        .register("StorageDomain.create", vdsm_storage::create_storage_domain)
        .register("StorageDomain.getInfo", vdsm_storage::get_storage_domain_info)
        .register("StorageDomain.getStats", vdsm_storage::get_storage_domain_stats)
        .register("StorageDomain.attach", vdsm_storage::attach_storage_domain)
        .register("StorageDomain.activate", vdsm_storage::activate_storage_domain)
        .register("StoragePool.connectStoragePool", vdsm_storage::connect_storage_pool)
        // Engine actually calls the short name `StoragePool.connect`
        // (verified from UNIMPLEMENTED-verb log); register both.
        .register("StoragePool.connect", vdsm_storage::connect_storage_pool)
        .register("StoragePool.disconnect", vdsm_storage::connect_storage_pool)
        .register("StoragePool.reconstructMaster", vdsm_storage::create_storage_pool)
        .register("StoragePool.refresh", vdsm_storage::connect_storage_pool)
        .register("StorageDomain.refresh", vdsm_storage::activate_storage_domain)
        .register("StorageDomain.detach", vdsm_storage::attach_storage_domain)
        .register("StorageDomain.deactivate", vdsm_storage::activate_storage_domain)
        .register("StorageDomain.format", vdsm_storage::activate_storage_domain)
        .register("StoragePool.spmStop", vdsm_storage::spm_start)
        // Image + Volume — for VM disk creation
        .register("Volume.create", vdsm_storage::volume_create)
        .register("Volume.getInfo", vdsm_storage::volume_get_info)
        .register("Volume.getSize", vdsm_storage::volume_get_size)
        .register("Volume.delete", vdsm_storage::volume_delete)
        .register("Image.prepare", vdsm_storage::image_prepare)
        .register("Image.teardown", vdsm_storage::image_teardown)
        .register("Image.delete", vdsm_storage::image_delete)
        .register("Image.deleteVolumes", vdsm_storage::image_delete_volumes)
        // iSCSI / FC / LVM — block storage probes from engine UI.
        // None of these activate VM-disk-on-block paths yet; they make
        // the "New Storage Domain" dialog work end-to-end for type
        // selection + discovery.
        .register("Host.discoverSendTargets", vdsm_storage::iscsi_discover_send_targets)
        .register("Host.iscsiDiscoverSendTargets", vdsm_storage::iscsi_discover_send_targets)
        .register("Host.iscsiLogin", vdsm_storage::iscsi_login)
        .register("Host.iscsiLogout", vdsm_storage::iscsi_logout)
        .register("Host.iscsiRescan", vdsm_storage::iscsi_rescan)
        .register("Host.getDeviceList", vdsm_storage::get_device_list)
        .register("Host.getDevicesVisibility", vdsm_storage::get_devices_visibility)
        .register("Host.getLVMVGList", vdsm_storage::get_lvm_vg_list)
        .register("Host.getVGInfo", vdsm_storage::get_vg_info)
        .register("Host.createVG", vdsm_storage::create_vg)
        .register("Host.extendVG", vdsm_storage::extend_vg)
        .register("Host.removeVG", vdsm_storage::remove_vg)
        .register("Host.getPathListStatus", vdsm_storage::get_path_list_status)
        .register("Host.fcScan", vdsm_storage::fc_scan)
        .register("StoragePool.create", vdsm_storage::create_storage_pool)
        .register("StoragePool.spmStart", vdsm_storage::spm_start)
        .register("StoragePool.getSpmStatus", vdsm_storage::get_spm_status)
        .register("StoragePool.getInfo", vdsm_storage::get_storage_pool_info)
        .register("Host.HSMGetAllTasksStatuses", vdsm_storage::hsm_get_all_tasks_statuses)
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
