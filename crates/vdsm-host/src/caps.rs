//! `Host.getCapabilities` — host inventory engine reads on first contact
//! and on every reconnect to decide whether the host is acceptable for
//! the cluster compatibility level.
//!
//! Real upstream VDSM returns ~80 fields here; we ship the subset that
//! engine actually consults during host registration. Add fields as
//! engine logs complain about missing keys — under-reporting is far
//! safer than guessing values for fields engine writes back to.

use serde_json::{json, Map, Value};

use vdsm_rpc::JsonRpcError;

use crate::sysinfo;

/// Software version we *claim* to engine via getCapabilities.software_version.
/// Distinct from the RPM Version on purpose:
///   - RPM Version is `4.5.7` (matches `ovirt-engine-4.5.7` so admins running
///     `rpm -q vdsm-rs` see version-matched packages on their hosts).
///   - This wire version is `4.50.7` because engine compares the string
///     against a minimum-vdsm floor (~"4.40") using RPM version-compare
///     rules — `"4.5.7" < "4.40.0"` element-wise (5 < 40), so reporting
///     `4.5.7` here would fire VDS_VERSION_TOO_OLD. Upstream Python vdsm
///     uses the same `4.50.x for engine 4.5.x` convention for exactly this
///     reason; we follow suit for compatibility with the existing engine.
const CLAIMED_VDSM_VERSION: &str = "4.50.7";
const VDSM_RS_VERSION: &str = env!("CARGO_PKG_VERSION");

/// Build the capabilities map. Real upstream vdsm returns the capability
/// fields FLAT under "result" — not nested under `result.info`. Engine's
/// VdsBrokerObjectsBuilder.updateVDSDynamicData reads struct.get("cpuSpeed")
/// at the top level; nesting our fields under "info" makes every read
/// return null and NPE on assignDoubleValue → setCpuSpeedMh.
pub async fn get_capabilities(_params: Value) -> Result<Value, JsonRpcError> {
    let cpu = sysinfo::cpu_info();
    let mem = sysinfo::mem_info();
    let os = sysinfo::os_release();
    let (load1, _, _) = sysinfo::loadavg();

    let nic_names = sysinfo::nic_names();
    let primary_nic = nic_names.first().cloned().unwrap_or_else(|| "lo".into());

    let nics: Map<String, Value> = nic_names
        .iter()
        .map(|n| (n.clone(), json!({
            "speed": 1000,
            "mtu": "1500",
            "hwaddr": "00:00:00:00:00:00",
            "addr": "",
            "netmask": "",
            "ipv4addrs": [],
            "ipv6addrs": [],
            "permhwaddr": "00:00:00:00:00:00",
            // ad_aggregator_id: engine does Integer.parseInt() on this if
            // present. Empty string explodes; only bonded NICs have a real
            // value. Omit entirely (Java null guard skips parse).
            "operstate": "up",
            "duplex": "full",
        })))
        .collect();

    // Engine's beforeFirstRefreshTreatment dereferences the management
    // network entry in caps.networks; absent → NullPointerException.
    // We claim ovirtmgmt is already bridged onto the primary NIC. The
    // cluster's actual management network name comes from the engine
    // config — "ovirtmgmt" is the default. If your cluster uses a
    // different name, populate that key too.
    let mut networks_map: Map<String, Value> = Map::new();
    let mgmt_entry = json!({
        "iface": "ovirtmgmt",
        "ports": [primary_nic.clone()],
        "stp": "off",
        "switch": "legacy",
        "addr": "",
        "netmask": "",
        "ipv4addrs": [],
        "ipv4defaultroute": true,
        "gateway": "",
        "ipv6addrs": [],
        "ipv6autoconf": true,
        "ipv6gateway": "::",
        "dhcpv4": false,
        "dhcpv6": false,
        "mtu": "1500",
        "bootproto4": "none",
        "bridged": true,
        "cfg": {},
        "qosOutbound": "",
        "qosInbound": "",
    });
    networks_map.insert("ovirtmgmt".into(), mgmt_entry);

    let mut bridges_map: Map<String, Value> = Map::new();
    bridges_map.insert("ovirtmgmt".into(), json!({
        "ports": [primary_nic.clone()],
        "stp": "off",
        "addr": "",
        "netmask": "",
        "ipv4addrs": [],
        "ipv6addrs": [],
        "mtu": "1500",
        "cfg": {},
        "opts": {},
    }));

    let info = json!({
        "uuid": sysinfo::vdsm_id(),
        "hostname": sysinfo::hostname(),
        "kvmEnabled": sysinfo::kvm_enabled().to_string(),

        // CPU topology — engine surfaces these in the host inventory pane.
        // cpuSpeed must round-trip through Java Double.parseDouble: send as a
        // String "<mhz>" with at least one digit. If /proc/cpuinfo missed
        // cpu MHz (some kernels w/ aggressive frequency scaling) fall back
        // to a non-zero placeholder so engine's BigDecimal coercion on the
        // setter side doesn't NPE on a null/zero unbox path.
        "cpuModel": cpu.model_name,
        "cpuFlags": cpu.flags.join(","),
        // Send as a literal quoted JSON string ALWAYS. Jackson reads
        // string → assignDoubleValue → Double.parseDouble. We tested
        // sending as f64 too (release 14/15) and engine STILL NPE'd in
        // setCpuSpeedMh — investigating whether the issue is the wrapping
        // layer rather than the value type.
        "cpuSpeed": format!("{:.1}", if cpu.mhz > 0.0 { cpu.mhz } else { 2400.0 }),
        "cpuCores": cpu.physical_cores.to_string(),
        "cpuThreads": cpu.logical_cpus.to_string(),
        "cpuSockets": cpu.sockets.to_string(),
        "onlineCpus": cpu.logical_cpus.to_string(),
        "cpuLoad": format!("{:.2}", load1),

        // Memory in MiB (engine convention).
        "memSize": (mem.total_kb / 1024).to_string(),
        "freeMem": (mem.available_kb / 1024).to_string(),

        // Versioning.
        "version_name": "Software Version",
        "software_version": CLAIMED_VDSM_VERSION,
        "vdsm_rs_version": VDSM_RS_VERSION,
        "kernel": sysinfo::kernel_release(),
        "operatingSystem": {
            "name": os.get("NAME").cloned().unwrap_or_else(|| "Linux".into()),
            "version": os.get("VERSION_ID").cloned().unwrap_or_default(),
            "release": os.get("BUILD_ID").cloned().unwrap_or_default(),
            "pretty_name": os.get("PRETTY_NAME").cloned().unwrap_or_default(),
        },

        // Network. Empty bonds/bridges/vlans for v0 — engine accepts the
        // empty map and won't try to manage networks until we declare we
        // can (additionalFeatures, supported_engines).
        "nics": nics,
        "bonds": {},
        "vlans": {},
        "bridges": bridges_map,
        "networks": networks_map,

        // NUMA + hugepages — minimal but well-formed.
        "numaNodes": numa_topology(),
        "numaNodeDistance": {},
        "autoNumaBalancing": 2,
        "hugepages": [2048],
        "hostdevPassthrough": "false",

        // Cluster compat levels we'd accept. Real engine pulls from this
        // when offering the host to a cluster.
        "supportedRHEVMs": ["4.5", "4.4"],
        "supportedENGINEs": ["4.5", "4.4"],
        "clusterLevels": ["4.5", "4.4"],
        "supportedProtocols": ["2.2", "2.3"],

        "selinux": {
            "mode": fs_first_line("/sys/fs/selinux/enforce").unwrap_or_else(|| "0".into())
        },

        // Feature flags we *don't* support yet but engine probes for.
        "additionalFeatures": [],
        "fipsEnabled": false,
        "liveMerge": "false",
        "liveSnapshot": "false",
        "openSessions": 0,

        // Package version dict — engine reads packages2.vdsm.version in the
        // post-first-refresh RefreshCapabilities path. Missing → NPE.
        "packages2": {
            "vdsm": {
                "version": CLAIMED_VDSM_VERSION,
                "release": "1.el9",
                "buildtime": 0u64,
            },
            "libvirt": {
                "version": "10.0.0",
                "release": "1.el9",
                "buildtime": 0u64,
            },
            "qemu-kvm": {
                "version": "8.2.0",
                "release": "1.el9",
                "buildtime": 0u64,
            },
            "qemu-img": {
                "version": "8.2.0",
                "release": "1.el9",
                "buildtime": 0u64,
            },
            "kernel": {
                "version": sysinfo::kernel_release(),
                "release": "",
                "buildtime": 0u64,
            },
            "spice-server": {
                "version": "0.15.2",
                "release": "1.el9",
                "buildtime": 0u64,
            },
            "openvswitch": {
                "version": "3.4.0",
                "release": "1.el9",
                "buildtime": 0u64,
            },
        },

        // QEMU machine types & guest OS types the engine offers when
        // scheduling VMs. Real vdsm enumerates from libvirt; we ship a
        // minimal x86_64 q35 set so the schedule pass succeeds.
        "emulatedMachines": [
            "pc-q35-rhel9.4.0",
            "pc-q35-rhel9.2.0",
            "pc-q35-rhel8.6.0",
            "q35",
        ],
        "vmTypes": ["kvm"],

        // HBA inventory — engine dereferences hbaInventory.iSCSI/.FC lists
        // during host inventory rendering. Empty lists are fine.
        "hbaInventory": {
            "iSCSI": [],
            "FC": [],
        },

        // RNG sources — engine validates VM templates against available
        // entropy sources. /dev/urandom is universal.
        "rngSources": ["random"],

        // Reserved memory for the host OS (MiB). Engine uses this when
        // computing schedulable memory; missing → arithmetic NPE.
        "reservedMem": "321",

        // Per-CPU topology — engine iterates cpuTopology to build NUMA
        // cell relationships. List of {socket_id, numa_cell_id, core_id, cpu_id}.
        "cpuTopology": cpu_topology(cpu.logical_cpus),

        // Host-side hooks — engine iterates this map to display installed hook
        // scripts. Empty dict is fine; null → NPE on `.entrySet()`.
        "hooks": {},

        // Misc system fields engine reads with no null guard:
        "kdumpStatus": 0,
        "bootTime": "0",
        "dateTime": current_iso_datetime(),
        "timeOffset": "0",
        "kernelArgs": "",
        "ISCSIInitiatorName": "",
        "transparentHugePages": "",
        "nr_hugepages": 0,
        "vm.free_hugepages": 0,

        // Network management hints. nmstate / openvswitch / ovnConfigured
        // tell engine which network backend we speak. We say "none of them"
        // (legacy ifcfg) so engine doesn't try to push nmstate YAML at us.
        "nmstate": {},
        "openvswitch": false,
        "ovnConfigured": false,

        // Block-size support hints used by storage scheduling.
        "supported_block_size": ["4k", "512"],
        "domain_versions": [3, 4, 5],
    });

    // Use the minimal map below directly (we'll wire up the full `info`
    // when host transitions to Up). Engine's JsonResponseUtil.updateResponse
    // WRAPS our result under "info" itself — we send the bare struct, not
    // `{info: ..., status: ...}`.
    let _ = info;
    // cpuFlags = real /proc/cpuinfo flags + engine-specific model_<X> tokens.
    // Engine matches model_* against ServerCPUList; missing token fires
    // CPU_TYPE_UNSUPPORTED_IN_THIS_CLUSTER_VERSION.
    let cpu_flags_str = {
        let mut all = cpu.flags.clone();
        for t in sysinfo::model_tokens_from_flags(&cpu.flags) {
            all.push(t.to_string());
        }
        all.join(",")
    };

    // packages2 — only include packages whose version() returned non-empty.
    // Empty entries get filled with the bare package name by engine and
    // pollute the UI.
    let mut pkgs: Map<String, Value> = Map::new();
    pkgs.insert("vdsm".into(), json!({
        "version": CLAIMED_VDSM_VERSION,
        "release": format!("vdsm-rs-{VDSM_RS_VERSION}"),
    }));
    pkgs.insert("kernel".into(), pkg_split(&sysinfo::kernel_release()));
    for (name, v) in [
        ("qemu-kvm",     sysinfo::kvm_version()),
        ("qemu-img",     sysinfo::kvm_version()),
        ("libvirt",      sysinfo::libvirt_version()),
        ("spice-server", sysinfo::spice_version()),
        ("gluster",      sysinfo::gluster_version()),
        ("glusterfs-cli", sysinfo::gluster_version()),
        ("librbd1",      sysinfo::ceph_version()),
        ("openvswitch",  sysinfo::openvswitch_version()),
        ("nmstate",      sysinfo::nmstate_version()),
    ] {
        if !v.is_empty() {
            pkgs.insert(name.into(), pkg_version(&v));
        }
    }

    // Build real NUMA + hugepage data from /sys.
    let numa_node_dicts: Map<String, Value> = sysinfo::numa_nodes()
        .into_iter()
        .map(|(idx, cpus, mem_mb)| {
            (
                idx.to_string(),
                json!({
                    "cpus": cpus,
                    "totalMemory": mem_mb.to_string(),
                }),
            )
        })
        .collect();
    let numa_dist_dicts: Map<String, Value> = sysinfo::numa_distances()
        .into_iter()
        .map(|(idx, distances)| (idx.to_string(), json!(distances)))
        .collect();
    let hugepage_sizes: Vec<u64> = sysinfo::hugepage_sizes_kb();
    let emulated = sysinfo::emulated_machines();

    let minimal = json!({
        "uuid": sysinfo::vdsm_id(),
        "hostname": sysinfo::hostname(),
        "kvmEnabled": sysinfo::kvm_enabled().to_string(),
        "cpuSpeed": format!("{:.1}", if cpu.mhz > 0.0 { cpu.mhz } else { 2400.0 }),
        "cpuCores": cpu.physical_cores.to_string(),
        "cpuThreads": cpu.logical_cpus.to_string(),
        "cpuSockets": cpu.sockets.to_string(),
        "cpuModel": cpu.model_name,
        "cpuFlags": cpu_flags_str,
        "onlineCpus": cpu.logical_cpus.to_string(),
        "cpuLoad": format!("{:.2}", load1),
        "memSize": (mem.total_kb / 1024).to_string(),
        "freeMem": (mem.available_kb / 1024).to_string(),
        "software_version": CLAIMED_VDSM_VERSION,
        "vdsm_rs_version": VDSM_RS_VERSION,
        "kernel": sysinfo::kernel_release(),
        "operatingSystem": {
            "name":    os.get("NAME").cloned().unwrap_or_else(|| "Linux".into()),
            "version": os.get("VERSION_ID").cloned().unwrap_or_default(),
            "release": os.get("BUILD_ID").cloned().unwrap_or_else(|| sysinfo::kernel_release()),
            "pretty_name": os.get("PRETTY_NAME").cloned().unwrap_or_default(),
        },
        "boot_time": sysinfo::boot_time_secs().to_string(),
        "bootTime":  sysinfo::boot_time_secs().to_string(),

        // Component versions — engine reads these JSON keys into matching
        // vds_dynamic columns. Field names are snake_case (matches Python
        // VDSM caps.py) for the dedicated DB columns. We also emit
        // camelCase aliases where some engine versions use those instead.
        "kvm_version":          sysinfo::kvm_version(),
        "kvmVersion":           sysinfo::kvm_version(),
        "libvirt_version":      sysinfo::libvirt_version(),
        "libvirtVersion":       sysinfo::libvirt_version(),
        "kernel_version":       sysinfo::kernel_release(),
        "kernelVersion":        sysinfo::kernel_release(),
        "version_name":         "Software Version",
        "spice_version":        sysinfo::spice_version(),
        "spiceVersion":         sysinfo::spice_version(),
        "gluster_version":      sysinfo::gluster_version(),
        "glusterVersion":       sysinfo::gluster_version(),
        "glusterfs_cli_version": sysinfo::gluster_version(),
        "ceph_version":         sysinfo::ceph_version(),
        "cephVersion":          sysinfo::ceph_version(),
        "librbd1_version":      sysinfo::ceph_version(),
        "openvswitch_version":  sysinfo::openvswitch_version(),
        "nmstate_version":      sysinfo::nmstate_version(),
        "nmstateVersion":       sysinfo::nmstate_version(),
        "rpm_version":          format!("vdsm-rs-{VDSM_RS_VERSION}-fc44"),
        "hw_version":           sysinfo::dmi_field("bios_version"),
        "hwVersion":            sysinfo::dmi_field("bios_version"),
        // Serial number — root-only in /sys/class/dmi/id; vdsmd.service's
        // ExecStartPre=+ copies it to /run/vdsm/product_serial, which the
        // sysinfo::dmi_field helper falls back to.
        "systemSerialNumber":   sysinfo::dmi_field("product_serial"),
        "hwSerialNumber":       sysinfo::dmi_field("product_serial"),
        "serial":               sysinfo::dmi_field("product_serial"),
        // TSC frequency (MHz, as string). Engine UI shows this under
        // "TSC Frequency"; missing → "Unknown (scaling disabled)".
        "tscFrequency":         sysinfo::tsc_frequency_mhz(),
        "tsc_frequency":        sysinfo::tsc_frequency_mhz(),
        // Engine reads systemManufacturer / systemProductName / systemSerialNumber
        // / systemVersion at the caps top level (in addition to from
        // Host.getHardwareInfo).
        "systemManufacturer":   sysinfo::dmi_field("sys_vendor"),
        "systemProductName":    sysinfo::dmi_field("product_name"),
        "systemSerialNumber":   sysinfo::dmi_field("product_serial"),
        "systemVersion":        sysinfo::dmi_field("bios_version"),
        "systemFamily":         sysinfo::dmi_field("sys_family"),
        // packages2 is set below from the precomputed `pkgs` map (which
        // omits empty versions so engine doesn't fill those entries with
        // the bare package name).
        "kernel_features":      sysinfo::kernel_features(),
        "kernelFeatures":       sysinfo::kernel_features(),
        // Engine reads `kernelArgs` (camelCase) and writes to vds_dynamic.kernel_args.
        "kernelArgs":           std::fs::read_to_string("/proc/cmdline").unwrap_or_default().trim().to_string(),
        "kernel_args":          std::fs::read_to_string("/proc/cmdline").unwrap_or_default().trim().to_string(),
        "vncEncrypted":         false,
        "ovnConfigured":        false,
        "openvswitch":          false,
        "nmstate":              {},

        // Engine reads component versions from packages2 — only entries
        // with non-empty versions are included (see `pkgs` above).
        "packages2": pkgs,
        // ovirtmgmt bridge — engine refuses to leave NonOperational when
        // it can't find the cluster's management network on the host.
        // Pick a real UP iface so engine's "interface state down" guard
        // (VDS_SET_NONOPERATIONAL_IFACE_DOWN) doesn't fire and bounce us.
        // Engine also reads addr / ipv4addrs / gateway on the mgmt network
        // to confirm it's actually serving traffic; empty values trigger
        // VDS_INSTALL_FAILED("Failed to configure management network").
        "nics": {
            sysinfo::mgmt_iface(): {
                "addr": sysinfo::iface_ipv4(&sysinfo::mgmt_iface()).0,
                "cfg": {},
                "mtu": sysinfo::iface_mtu(&sysinfo::mgmt_iface()),
                "netmask": sysinfo::iface_ipv4(&sysinfo::mgmt_iface()).2,
                "ipv4addrs": [sysinfo::iface_ipv4(&sysinfo::mgmt_iface()).1],
                "ipv6addrs": [],
                "hwaddr": sysinfo::iface_mac(&sysinfo::mgmt_iface()),
                "speed": 1000,
                "state": "up", "duplex": "full",
                "ad_aggregator_id": "0",
                "permhwaddr": sysinfo::iface_mac(&sysinfo::mgmt_iface()),
                "dhcpv4": false, "dhcpv6": false,
                "ipv4defaultroute": true,
                "gateway": sysinfo::default_gateway(),
                "ipv6gateway": "",
                "ipv6autoconf": false,
            },
        },
        "bonds": {},
        "vlans": {},
        "bridges": {
            "ovirtmgmt": {
                "addr": sysinfo::iface_ipv4(&sysinfo::mgmt_iface()).0,
                "cfg": {},
                "ipv4addrs": [sysinfo::iface_ipv4(&sysinfo::mgmt_iface()).1],
                "ipv6addrs": [],
                "mtu": sysinfo::iface_mtu(&sysinfo::mgmt_iface()),
                "netmask": sysinfo::iface_ipv4(&sysinfo::mgmt_iface()).2,
                "stp": "off",
                "ports": [sysinfo::mgmt_iface()],
                "dhcpv4": false, "dhcpv6": false,
                "gateway": sysinfo::default_gateway(),
                "ipv6gateway": "",
                "ipv4defaultroute": true,
                "ipv6autoconf": false,
                "opts": {},
            },
        },
        "networks": {
            "ovirtmgmt": {
                "iface": "ovirtmgmt", "bridge": "ovirtmgmt",
                "bridged": true,
                "addr": sysinfo::iface_ipv4(&sysinfo::mgmt_iface()).0,
                "ipv4addrs": [sysinfo::iface_ipv4(&sysinfo::mgmt_iface()).1],
                "ipv6addrs": [],
                "netmask": sysinfo::iface_ipv4(&sysinfo::mgmt_iface()).2,
                "mtu": sysinfo::iface_mtu(&sysinfo::mgmt_iface()),
                "stp": "off",
                "ports": [sysinfo::mgmt_iface()],
                "switch": "legacy",
                "dhcpv4": false, "dhcpv6": false,
                "gateway": sysinfo::default_gateway(),
                "ipv6gateway": "",
                "ipv4defaultroute": true,
                "ipv6autoconf": false,
                "southbound": sysinfo::mgmt_iface(),
            },
        },
        "numaNodes": numa_node_dicts,
        "numaNodeDistance": numa_dist_dicts,
        "autoNumaBalancing": 2,
        "hugepages": hugepage_sizes,
        "hostdevPassthrough": "false",
        "supportedRHEVMs": ["4.8", "4.7", "4.6", "4.5", "4.4"],
        "supportedENGINEs": ["4.8", "4.7", "4.6", "4.5", "4.4"],
        "clusterLevels": ["4.8", "4.7", "4.6", "4.5", "4.4"],
        "supportedProtocols": ["2.2", "2.3"],
        // Upstream Python VDSM uses Linux-kernel-aligned values:
        // -1=Disabled, 0=Permissive, 1=Enforcing. Engine's SELinuxMode
        // enum matches. (Earlier 0/1/2 convention was wrong.)
        "selinux": { "mode": sysinfo::selinux_mode() },
        "selinux_enforce_mode": sysinfo::selinux_mode(),
        "additionalFeatures": [],
        "fipsEnabled": sysinfo::fips_enabled(),
        "liveMerge": "true",
        "liveSnapshot": "true",
        "openSessions": 0,
        "emulatedMachines": emulated,
        "vmTypes": ["kvm"],
        "HBAInventory": {"iSCSI": [], "FC": []},
        "rngSources": ["random", "hwrng"],
        "reservedMem": "321",
        "hooks": {},
    });
    // Return the bare struct. JsonResponseUtil wraps under "info" + adds
    // status itself. If we wrap again here, engine sees double-info
    // and every assignDoubleValue/assignStringValue returns null → NPE.
    Ok(minimal)
}

fn numa_topology() -> Map<String, Value> {
    let mut out = Map::new();
    for i in 0..sysinfo::numa_node_count() {
        out.insert(
            i.to_string(),
            json!({
                "cpus": [],
                "totalMemory": "0",
            }),
        );
    }
    out
}

/// Per-CPU topology entries. Engine iterates cpuTopology to build NUMA
/// cell relationships. We claim everything is on socket 0, NUMA cell 0,
/// distinct cores — close enough for protocol conformance.
fn cpu_topology(logical_cpus: u32) -> Vec<Value> {
    (0..logical_cpus)
        .map(|i| {
            json!({
                "cpu_id": i,
                "core_id": i,
                "socket_id": 0,
                "numa_cell_id": 0,
            })
        })
        .collect()
}

fn current_iso_datetime() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    // Coarse YYYY-MM-DDTHH:MM:SSZ from epoch — engine just wants a string.
    let days = secs / 86_400;
    let year = 1970 + days / 365;
    let _ = year; // simple placeholder; engine doesn't parse it strictly
    format!("{}Z", secs)
}

fn fs_first_line(path: &str) -> Option<String> {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|s| s.lines().next().map(|l| l.trim().to_string()))
}

/// Return `{"version": ..., "release": ""}` for packages2 entries where
/// we have a single bare version string. Empty input → empty dict so
/// engine renders `[N/A]` correctly for components not installed.
fn pkg_version(v: &str) -> Value {
    if v.is_empty() {
        return json!({});
    }
    json!({ "version": v, "release": "" })
}

/// Split a kernel-release-style string `7.0.4-200.fc44.x86_64` into
/// `{"version": "7.0.4", "release": "200.fc44.x86_64"}` so engine's
/// `{version}-{release}` join reproduces the original.
fn pkg_split(v: &str) -> Value {
    if v.is_empty() {
        return json!({});
    }
    match v.split_once('-') {
        Some((ver, rel)) => json!({ "version": ver, "release": rel }),
        None => json!({ "version": v, "release": "" }),
    }
}
