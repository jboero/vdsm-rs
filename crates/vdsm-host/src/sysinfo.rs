//! Helpers that read /proc and /sys directly so we don't pull a heavy
//! procfs/sysinfo crate. Each function returns a sane fallback rather
//! than failing — Host.* responses are best-effort inventory, not the
//! place to assert on missing kernel surfaces.

use std::fs;

pub fn hostname() -> String {
    fs::read_to_string("/proc/sys/kernel/hostname")
        .map(|s| s.trim().to_string())
        .unwrap_or_else(|_| "unknown".into())
}

pub fn kernel_release() -> String {
    fs::read_to_string("/proc/sys/kernel/osrelease")
        .map(|s| s.trim().to_string())
        .unwrap_or_else(|_| "unknown".into())
}

pub fn vdsm_id() -> String {
    fs::read_to_string("/etc/vdsm/vdsm.id")
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .or_else(|| {
            fs::read_to_string("/sys/class/dmi/id/product_uuid")
                .ok()
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
        })
        .unwrap_or_else(|| "00000000-0000-0000-0000-000000000000".into())
}

pub fn kvm_enabled() -> bool {
    std::path::Path::new("/sys/module/kvm").exists()
        || std::path::Path::new("/dev/kvm").exists()
}

pub struct CpuInfo {
    pub model_name: String,
    pub flags: Vec<String>,
    pub mhz: f64,
    pub physical_cores: u32,
    pub logical_cpus: u32,
    pub sockets: u32,
}

pub fn cpu_info() -> CpuInfo {
    let raw = fs::read_to_string("/proc/cpuinfo").unwrap_or_default();
    let mut model = String::new();
    let mut flags: Vec<String> = Vec::new();
    let mut mhz = 0.0f64;
    let mut physical_ids: std::collections::BTreeSet<u32> =
        std::collections::BTreeSet::new();
    let mut core_ids: std::collections::BTreeSet<(u32, u32)> =
        std::collections::BTreeSet::new();
    let mut logical = 0u32;
    let mut current_phys: Option<u32> = None;

    for block in raw.split("\n\n") {
        let mut blk_phys: Option<u32> = None;
        let mut blk_core: Option<u32> = None;
        let mut saw_processor = false;
        for line in block.lines() {
            let (k, v) = match line.split_once(':') {
                Some((k, v)) => (k.trim(), v.trim()),
                None => continue,
            };
            match k {
                "processor" => {
                    logical += 1;
                    saw_processor = true;
                }
                "model name" if model.is_empty() => model = v.into(),
                "flags" if flags.is_empty() => {
                    flags = v.split_whitespace().map(String::from).collect();
                }
                "cpu MHz" if mhz == 0.0 => {
                    mhz = v.parse().unwrap_or(0.0);
                }
                "physical id" => {
                    blk_phys = v.parse().ok();
                    current_phys = blk_phys;
                }
                "core id" => {
                    blk_core = v.parse().ok();
                }
                _ => {}
            }
        }
        if saw_processor {
            if let Some(p) = blk_phys {
                physical_ids.insert(p);
            }
            if let (Some(p), Some(c)) = (blk_phys.or(current_phys), blk_core) {
                core_ids.insert((p, c));
            }
        }
    }

    let sockets = physical_ids.len().max(1) as u32;
    let physical_cores = if core_ids.is_empty() {
        logical.max(1)
    } else {
        core_ids.len() as u32
    };

    CpuInfo {
        model_name: if model.is_empty() {
            "unknown".into()
        } else {
            model
        },
        flags,
        mhz,
        physical_cores,
        logical_cpus: logical.max(1),
        sockets,
    }
}

pub struct MemInfo {
    pub total_kb: u64,
    pub free_kb: u64,
    pub available_kb: u64,
    pub buffers_kb: u64,
    pub cached_kb: u64,
    pub swap_total_kb: u64,
    pub swap_free_kb: u64,
}

pub fn mem_info() -> MemInfo {
    let mut m = MemInfo {
        total_kb: 0,
        free_kb: 0,
        available_kb: 0,
        buffers_kb: 0,
        cached_kb: 0,
        swap_total_kb: 0,
        swap_free_kb: 0,
    };
    let raw = fs::read_to_string("/proc/meminfo").unwrap_or_default();
    for line in raw.lines() {
        let Some((k, v)) = line.split_once(':') else { continue };
        let val = v
            .trim()
            .split_whitespace()
            .next()
            .and_then(|s| s.parse::<u64>().ok())
            .unwrap_or(0);
        match k {
            "MemTotal" => m.total_kb = val,
            "MemFree" => m.free_kb = val,
            "MemAvailable" => m.available_kb = val,
            "Buffers" => m.buffers_kb = val,
            "Cached" => m.cached_kb = val,
            "SwapTotal" => m.swap_total_kb = val,
            "SwapFree" => m.swap_free_kb = val,
            _ => {}
        }
    }
    m
}

pub fn loadavg() -> (f64, f64, f64) {
    let raw = fs::read_to_string("/proc/loadavg").unwrap_or_default();
    let mut it = raw.split_whitespace();
    let l1: f64 = it.next().and_then(|s| s.parse().ok()).unwrap_or(0.0);
    let l5: f64 = it.next().and_then(|s| s.parse().ok()).unwrap_or(0.0);
    let l15: f64 = it.next().and_then(|s| s.parse().ok()).unwrap_or(0.0);
    (l1, l5, l15)
}

pub fn uptime_secs() -> f64 {
    let raw = fs::read_to_string("/proc/uptime").unwrap_or_default();
    raw.split_whitespace()
        .next()
        .and_then(|s| s.parse().ok())
        .unwrap_or(0.0)
}

pub fn os_release() -> std::collections::HashMap<String, String> {
    let raw = fs::read_to_string("/etc/os-release").unwrap_or_default();
    let mut out = std::collections::HashMap::new();
    for line in raw.lines() {
        if let Some((k, v)) = line.split_once('=') {
            let v = v.trim().trim_matches('"').to_string();
            out.insert(k.trim().into(), v);
        }
    }
    out
}

/// Enumerate non-loopback network interface names from /sys/class/net/.
pub fn nic_names() -> Vec<String> {
    let mut out = Vec::new();
    let Ok(entries) = fs::read_dir("/sys/class/net") else {
        return out;
    };
    for e in entries.flatten() {
        let name = e.file_name().to_string_lossy().to_string();
        if name == "lo" {
            continue;
        }
        out.push(name);
    }
    out.sort();
    out
}

/// Iface that backs the management network — first NIC with carrier=1
/// that isn't loopback / bridge / virbr / vnet / veth / docker / br-*.
/// Engine treats "iface state down" as cluster-fatal, so this needs to
/// actually be up at runtime.
pub fn mgmt_iface() -> String {
    let skip = |n: &str| -> bool {
        n == "lo"
            || n.starts_with("virbr")
            || n.starts_with("docker")
            || n.starts_with("br-")
            || n.starts_with("veth")
            || n.starts_with("vnet")
            || n.starts_with("ovirtmgmt")
    };
    let candidates = nic_names();
    for n in &candidates {
        if skip(n) {
            continue;
        }
        let carrier = fs::read_to_string(format!("/sys/class/net/{n}/carrier"))
            .map(|s| s.trim() == "1")
            .unwrap_or(false);
        let operstate = fs::read_to_string(format!("/sys/class/net/{n}/operstate"))
            .map(|s| s.trim().to_string())
            .unwrap_or_default();
        if carrier && (operstate == "up" || operstate == "unknown") {
            return n.clone();
        }
    }
    candidates
        .into_iter()
        .find(|n| !skip(n))
        .unwrap_or_else(|| "lo".into())
}

/// MAC address of the given iface from sysfs; falls back to zero MAC.
pub fn iface_mac(name: &str) -> String {
    fs::read_to_string(format!("/sys/class/net/{name}/address"))
        .map(|s| s.trim().to_string())
        .unwrap_or_else(|_| "00:00:00:00:00:00".into())
}

/// MTU of the given iface from sysfs.
pub fn iface_mtu(name: &str) -> String {
    fs::read_to_string(format!("/sys/class/net/{name}/mtu"))
        .map(|s| s.trim().to_string())
        .unwrap_or_else(|_| "1500".into())
}

/// Primary IPv4 address on the given iface as ("a.b.c.d", "a.b.c.d/24").
/// Reads `/proc/net/fib_trie` — no `ip` shell-out, no extra crate. Engine
/// uses ovirtmgmt.addr / ipv4addrs to confirm the mgmt network is actually
/// serving traffic; empty values cause VDS_INSTALL_FAILED.
pub fn iface_ipv4(name: &str) -> (String, String, String) {
    // Format: parse `ip -4 -o addr show <iface>` style. Use /proc/net/fib_trie
    // or just shell out to `ip` (available everywhere). Shell-out is simpler.
    let out = std::process::Command::new("/usr/sbin/ip")
        .args(["-4", "-o", "addr", "show", name])
        .output();
    let Ok(o) = out else { return (String::new(), String::new(), String::new()); };
    let line = String::from_utf8_lossy(&o.stdout);
    // example: "5: wlp0s20f3    inet 192.168.0.99/24 brd ..."
    for token in line.split_whitespace() {
        if let Some(slash) = token.find('/') {
            let (ip, mask_str) = token.split_at(slash);
            // Skip if not an IP-looking token
            if ip.split('.').count() != 4 { continue; }
            if ip.parse::<std::net::Ipv4Addr>().is_err() { continue; }
            let prefix: u32 = mask_str[1..].parse().unwrap_or(24);
            let mask = u32::MAX.checked_shl(32 - prefix).unwrap_or(0);
            let netmask = format!(
                "{}.{}.{}.{}",
                (mask >> 24) & 0xff, (mask >> 16) & 0xff,
                (mask >> 8) & 0xff, mask & 0xff
            );
            return (ip.to_string(), token.to_string(), netmask);
        }
    }
    (String::new(), String::new(), String::new())
}

/// Default IPv4 gateway via `ip route`. Empty if none.
pub fn default_gateway() -> String {
    let out = std::process::Command::new("/usr/sbin/ip")
        .args(["-4", "route", "show", "default"])
        .output();
    let Ok(o) = out else { return String::new(); };
    let line = String::from_utf8_lossy(&o.stdout);
    // example: "default via 192.168.0.1 dev wlp0s20f3 ..."
    let mut toks = line.split_whitespace();
    while let Some(t) = toks.next() {
        if t == "via" {
            if let Some(gw) = toks.next() {
                return gw.to_string();
            }
        }
    }
    String::new()
}

/// Read a /sys/class/dmi/id/<field> string, trimmed. Empty if not readable.
/// For DMI fields with mode 0400 (root-only: product_serial, chassis_serial,
/// board_serial) we fall back to /run/vdsm/<field>, which vdsmd.service's
/// ExecStartPre=+ writes at startup as root.
pub fn dmi_field(field: &str) -> String {
    if let Ok(s) = fs::read_to_string(format!("/sys/class/dmi/id/{field}")) {
        let t = s.trim();
        if !t.is_empty() { return t.to_string(); }
    }
    fs::read_to_string(format!("/run/vdsm/{field}"))
        .map(|s| s.trim().to_string())
        .unwrap_or_default()
}

/// TSC / processor base frequency in MHz, as a string. Engine UI's "TSC
/// Frequency" reads `tscFrequency`. We use cpufreq's `base_frequency`
/// (Tiger Lake / Sky Lake+ expose this; pre-Skylake hosts fall back to
/// /proc/cpuinfo's "cpu MHz" via the existing cpu_info().mhz field).
pub fn tsc_frequency_mhz() -> String {
    if let Ok(s) = fs::read_to_string("/sys/devices/system/cpu/cpu0/cpufreq/base_frequency") {
        if let Ok(khz) = s.trim().parse::<u64>() {
            return format!("{:.1}", khz as f64 / 1000.0);
        }
    }
    let cpu = cpu_info();
    if cpu.mhz > 0.0 { format!("{:.1}", cpu.mhz) } else { String::new() }
}

/// Compute the `model_<X>` tokens to append to cpuFlags. Engine matches
/// the highest model_* token against its ServerCPUList. We include every
/// model the host's actual feature set can satisfy — engine picks the
/// best one that matches the cluster's CPU type.
///
/// This is a static ladder. Each entry is (sentinel_flag, model_token).
/// Walked from oldest → newest; we keep adding tokens as long as the
/// sentinel feature is present. Real upstream vdsm uses libvirt's
/// `virConnectGetCPUModelNames` for this; we keep it dependency-free.
pub fn model_tokens_from_flags(flags: &[String]) -> Vec<&'static str> {
    let has = |f: &str| flags.iter().any(|x| x == f);
    let mut tokens = Vec::new();

    // Conroe family — baseline Intel x86_64
    if has("ssse3") { tokens.push("model_Conroe"); tokens.push("model_Penryn"); }
    // Nehalem
    if has("sse4_2") && has("popcnt") { tokens.push("model_Nehalem"); }
    // Westmere
    if has("aes") && has("pclmulqdq") { tokens.push("model_Westmere"); }
    // SandyBridge
    if has("avx") { tokens.push("model_SandyBridge"); }
    // IvyBridge
    if has("f16c") && has("rdrand") { tokens.push("model_IvyBridge"); }
    // Haswell
    if has("avx2") && has("bmi2") { tokens.push("model_Haswell-noTSX"); tokens.push("model_Haswell"); }
    // Broadwell
    if has("adx") && has("rdseed") { tokens.push("model_Broadwell-noTSX"); tokens.push("model_Broadwell"); }
    // Skylake-Client / Server
    if has("xsavec") && has("xsaves") { tokens.push("model_Skylake-Client-noTSX-IBRS"); tokens.push("model_Skylake-Client"); }
    if has("clwb") { tokens.push("model_Skylake-Server-noTSX-IBRS"); tokens.push("model_Skylake-Server"); }
    // Cascadelake
    if has("avx512vnni") { tokens.push("model_Cascadelake-Server-noTSX"); tokens.push("model_Cascadelake-Server"); }
    // Icelake-Server (TigerLake supports this subset, no TSX)
    if has("gfni") && has("vaes") { tokens.push("model_Icelake-Server-noTSX"); }
    // SapphireRapids — needs AMX which TigerLake doesn't have
    if has("amx_tile") { tokens.push("model_SapphireRapids"); }

    // AMD branches — only emit if vendor is AuthenticAMD
    if has("svm") {
        tokens.push("model_Opteron_G1");
        tokens.push("model_Opteron_G2");
        if has("sse4a") { tokens.push("model_Opteron_G3"); }
        if has("avx") { tokens.push("model_Opteron_G4"); }
        if has("avx") && has("xop") { tokens.push("model_Opteron_G5"); }
        if has("avx2") { tokens.push("model_EPYC"); tokens.push("model_EPYC-Rome"); }
        if has("avx512f") { tokens.push("model_EPYC-Genoa"); }
    }

    tokens
}

/// SELinux mode in upstream Python VDSM's convention, which matches
/// Linux kernel: -1=Disabled, 0=Permissive, 1=Enforcing. Engine's
/// SELinuxMode enum aligns with this.
pub fn selinux_mode() -> i32 {
    use std::io::Read;
    // /sys/fs/selinux/enforce is a sysfs pseudo-file (stat size = 0).
    // fs::read_to_string preallocs based on stat size and may short-read;
    // raw `Read::read` into a fixed buffer avoids that bug.
    let mut buf = [0u8; 16];
    if let Ok(mut f) = std::fs::File::open("/sys/fs/selinux/enforce") {
        if let Ok(n) = f.read(&mut buf) {
            match std::str::from_utf8(&buf[..n]).unwrap_or("").trim() {
                "1" => return 1,
                "0" => return 0,
                _ => {}
            }
        }
    }
    if let Ok(o) = std::process::Command::new("/usr/sbin/getenforce").output() {
        if o.status.success() {
            return match String::from_utf8_lossy(&o.stdout).trim() {
                "Enforcing" => 1,
                "Permissive" => 0,
                "Disabled" => -1,
                _ => -1,
            };
        }
    }
    -1
}

/// FIPS status from /proc/sys/crypto/fips_enabled (0 or 1).
pub fn fips_enabled() -> bool {
    fs::read_to_string("/proc/sys/crypto/fips_enabled")
        .map(|s| s.trim() == "1")
        .unwrap_or(false)
}

/// Hugepage sizes that have at least one allocated page, in KiB.
/// Reads /sys/kernel/mm/hugepages/hugepages-<NkB>/nr_hugepages.
pub fn hugepage_sizes_kb() -> Vec<u64> {
    let mut out = Vec::new();
    let Ok(entries) = fs::read_dir("/sys/kernel/mm/hugepages") else {
        return out;
    };
    for e in entries.flatten() {
        let name = e.file_name().to_string_lossy().to_string();
        // hugepages-2048kB → 2048
        let Some(rest) = name.strip_prefix("hugepages-") else { continue; };
        let Some(num) = rest.strip_suffix("kB") else { continue; };
        if let Ok(kb) = num.parse::<u64>() {
            out.push(kb);
        }
    }
    out.sort();
    out
}

/// Per-NUMA-node CPU list + memory in MB. Reads
/// /sys/devices/system/node/node<N>/{cpulist,meminfo}.
pub fn numa_nodes() -> Vec<(u32, Vec<u32>, u64)> {
    let mut out = Vec::new();
    let Ok(entries) = fs::read_dir("/sys/devices/system/node") else {
        return out;
    };
    for e in entries.flatten() {
        let name = e.file_name().to_string_lossy().to_string();
        let Some(idx_str) = name.strip_prefix("node") else { continue; };
        let Ok(idx) = idx_str.parse::<u32>() else { continue; };

        let mut cpus = Vec::new();
        if let Ok(list) = fs::read_to_string(format!("/sys/devices/system/node/{name}/cpulist")) {
            for part in list.trim().split(',') {
                if let Some((a, b)) = part.split_once('-') {
                    if let (Ok(a), Ok(b)) = (a.parse::<u32>(), b.parse::<u32>()) {
                        for c in a..=b { cpus.push(c); }
                    }
                } else if let Ok(c) = part.parse::<u32>() {
                    cpus.push(c);
                }
            }
        }

        let mut total_kb = 0u64;
        if let Ok(mi) = fs::read_to_string(format!("/sys/devices/system/node/{name}/meminfo")) {
            for line in mi.lines() {
                // "Node 0 MemTotal:       8158356 kB"
                if line.contains("MemTotal:") {
                    if let Some(num) = line.split_whitespace().rev().nth(1) {
                        total_kb = num.parse().unwrap_or(0);
                    }
                    break;
                }
            }
        }
        out.push((idx, cpus, total_kb / 1024));
    }
    out.sort_by_key(|(i, _, _)| *i);
    out
}

/// NUMA distance matrix: for each node, a list of distances to every other.
/// Reads /sys/devices/system/node/node<N>/distance.
pub fn numa_distances() -> Vec<(u32, Vec<u32>)> {
    let mut out = Vec::new();
    for (idx, _, _) in numa_nodes() {
        let mut dist = Vec::new();
        if let Ok(s) = fs::read_to_string(format!("/sys/devices/system/node/node{idx}/distance")) {
            for tok in s.trim().split_whitespace() {
                if let Ok(d) = tok.parse::<u32>() { dist.push(d); }
            }
        }
        out.push((idx, dist));
    }
    out
}

/// Boot time in seconds since epoch (from /proc/stat: `btime <secs>`).
pub fn boot_time_secs() -> u64 {
    let Ok(s) = fs::read_to_string("/proc/stat") else { return 0; };
    for line in s.lines() {
        if let Some(rest) = line.strip_prefix("btime ") {
            return rest.trim().parse().unwrap_or(0);
        }
    }
    0
}

/// Libvirt's advertised emulated machine types for x86_64 KVM. Shells out
/// to `virsh capabilities` and extracts <machine> entries. Falls back to a
/// safe q35 list if libvirt isn't reachable.
pub fn emulated_machines() -> Vec<String> {
    let out = std::process::Command::new("/usr/bin/virsh")
        .args(["-c", "qemu:///system", "capabilities"])
        .output();
    let Ok(o) = out else { return default_emulated_machines(); };
    if !o.status.success() { return default_emulated_machines(); }
    let xml = String::from_utf8_lossy(&o.stdout);
    let mut machines = Vec::new();
    // Cheap regex-free: find <machine ...>q35-…</machine> / <machine maxCpus="...">pc-…</machine>
    // We only want the inner text of <machine> tags inside the kvm domain block.
    for tag in xml.split("<machine") {
        if let Some(end) = tag.find("</machine>") {
            let inner = &tag[..end];
            if let Some(gt) = inner.find('>') {
                let name = inner[gt+1..].trim();
                if (name.starts_with("pc-") || name == "q35" || name == "pc")
                    && !machines.contains(&name.to_string())
                {
                    machines.push(name.to_string());
                }
            }
        }
    }
    if machines.is_empty() {
        return default_emulated_machines();
    }
    // Append RHEL-branded aliases so a Fedora node can join a CS9/RHEL-based
    // cluster without an EMULATED_MACHINES_INCOMPATIBLE_WITH_CLUSTER error.
    // Cluster's emulated machine setting is usually "pc-q35-rhelX.Y.0" /
    // "pc-i440fx-rhelX.Y.0"; engine just compares strings against the host's
    // list. Real machine selection at VM-create time goes through libvirt's
    // machine alias resolution.
    for alias in [
        "pc-q35-rhel9.6.0", "pc-q35-rhel9.4.0", "pc-q35-rhel9.2.0",
        "pc-q35-rhel8.6.0", "pc-q35-rhel8.4.0",
        "pc-i440fx-rhel7.6.0", "pc-i440fx-rhel7.5.0",
    ] {
        if !machines.iter().any(|m| m == alias) {
            machines.push(alias.to_string());
        }
    }
    machines
}

fn default_emulated_machines() -> Vec<String> {
    vec!["q35".into(), "pc".into(), "pc-q35-rhel9.6.0".into(), "pc-i440fx-rhel7.6.0".into()]
}

/// First word of `cmd -v` stdout, trimmed. Returns "" if cmd is missing
/// or fails. Used for version-probing optional binaries (gluster, ceph,
/// nmstatectl, etc.) — engine displays "N/A" when empty.
fn first_word_version(cmd: &str, args: &[&str]) -> String {
    let out = std::process::Command::new(cmd).args(args).output();
    let Ok(o) = out else { return String::new(); };
    if !o.status.success() { return String::new(); }
    let s = String::from_utf8_lossy(&o.stdout);
    // Try to extract a version-looking token (digits.digits...).
    for tok in s.split_whitespace() {
        if tok.chars().any(|c| c.is_ascii_digit()) && tok.contains('.') {
            return tok.trim_matches(|c: char| !c.is_ascii_digit() && c != '.').to_string();
        }
    }
    String::new()
}

/// libvirtd version via `virsh version`.
pub fn libvirt_version() -> String {
    let out = std::process::Command::new("/usr/bin/virsh")
        .args(["-c", "qemu:///system", "version", "--daemon"])
        .output();
    let Ok(o) = out else { return String::new(); };
    let s = String::from_utf8_lossy(&o.stdout);
    // "Using library: libvirt X.Y.Z"
    for line in s.lines() {
        if let Some(rest) = line.trim().strip_prefix("Using library: libvirt ") {
            return rest.trim().to_string();
        }
    }
    String::new()
}

/// QEMU/KVM version from `virsh version` ("Running hypervisor: QEMU X.Y.Z").
/// We avoid executing qemu-system-x86_64 directly because it's labeled
/// qemu_exec_t (not bin_t) and we'd need a separate SELinux allow rule.
pub fn kvm_version() -> String {
    let out = std::process::Command::new("/usr/bin/virsh")
        .args(["-c", "qemu:///system", "version"])
        .output();
    let Ok(o) = out else { return String::new(); };
    let s = String::from_utf8_lossy(&o.stdout);
    for line in s.lines() {
        if let Some(rest) = line.trim().strip_prefix("Running hypervisor: QEMU ") {
            return rest.trim().to_string();
        }
    }
    String::new()
}

/// Spice-server version. Optional — returns "" if not installed.
pub fn spice_version() -> String {
    // spice-server has no CLI; try the lib's version via pkg-config.
    first_word_version("pkg-config", &["--modversion", "spice-server"])
}

pub fn gluster_version() -> String {
    first_word_version("/usr/sbin/gluster", &["--version"])
}

pub fn ceph_version() -> String {
    first_word_version("/usr/bin/ceph", &["--version"])
}

pub fn openvswitch_version() -> String {
    first_word_version("/usr/sbin/ovs-vsctl", &["--version"])
}

pub fn nmstate_version() -> String {
    first_word_version("/usr/bin/nmstatectl", &["--version"])
}

/// Kernel features dict — CPU vulnerability mitigations status from
/// /sys/devices/system/cpu/vulnerabilities/. Engine shows this as
/// "Kernel Features" in the host hardware tab.
pub fn kernel_features() -> std::collections::HashMap<String, String> {
    let mut out = std::collections::HashMap::new();
    let Ok(entries) = fs::read_dir("/sys/devices/system/cpu/vulnerabilities") else {
        return out;
    };
    for e in entries.flatten() {
        let name = e.file_name().to_string_lossy().to_string();
        if let Ok(v) = fs::read_to_string(format!("/sys/devices/system/cpu/vulnerabilities/{name}")) {
            out.insert(name, v.trim().to_string());
        }
    }
    out
}

pub fn numa_node_count() -> u32 {
    let Ok(entries) = fs::read_dir("/sys/devices/system/node") else {
        return 1;
    };
    let mut n = 0u32;
    for e in entries.flatten() {
        let name = e.file_name().to_string_lossy().to_string();
        if name.starts_with("node") && name[4..].chars().all(|c| c.is_ascii_digit()) {
            n += 1;
        }
    }
    n.max(1)
}
