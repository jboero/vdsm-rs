//! `Host.getStats` — sampled host stats engine consumes ~every 15 sec
//! to keep the dashboard live.

use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::{json, Value};

use vdsm_rpc::JsonRpcError;

use crate::sysinfo;

/// `yyyy-MM-dd'T'HH:mm:ss'Z'` — engine's SimpleDateFormat for Time Drift.
fn iso_utc_now() -> String {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    // Plain conversion without an extra crate: civil from seconds.
    let days = secs.div_euclid(86_400);
    let sec_of_day = secs.rem_euclid(86_400);
    let hour = (sec_of_day / 3600) as u32;
    let minute = ((sec_of_day % 3600) / 60) as u32;
    let second = (sec_of_day % 60) as u32;
    // Howard Hinnant's days_from_civil inverse.
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = (if mp < 10 { mp + 3 } else { mp - 9 }) as u32;
    let year = (y + if m <= 2 { 1 } else { 0 }) as i64;
    // Engine pattern: `yyyy-MM-dd HH:mm:ss z`. Java's lowercase `z` parser
    // is buggy on 3-letter TZ codes (`UTC` → `UT`+`C`, `GMT` → `GM`+`T`).
    // The reliable workaround is RFC 822 numeric offset, which Java's `z`
    // also accepts: `-0000` for UTC. Confirmed by SimpleDateFormat docs.
    format!(
        "{year:04}-{m:02}-{d:02} {hour:02}:{minute:02}:{second:02} -0000"
    )
}

pub async fn get_stats(_params: Value) -> Result<Value, JsonRpcError> {
    let mem = sysinfo::mem_info();
    let (load1, load5, load15) = sysinfo::loadavg();

    let total_kb = mem.total_kb.max(1);
    let used_kb = total_kb.saturating_sub(mem.available_kb);
    let used_pct = (used_kb as f64) * 100.0 / (total_kb as f64);

    // Per-NIC network stats. Engine dereferences info.network[<iface>].*
    // during HostMonitoring.GetStats; missing keys → NullPointerException.
    let network: serde_json::Map<String, Value> = sysinfo::nic_names()
        .into_iter()
        .chain(std::iter::once("ovirtmgmt".to_string()))
        .map(|n| (n, json!({
            "rx": "0",
            "tx": "0",
            "rxDropped": "0",
            "txDropped": "0",
            "rxErrors": "0",
            "txErrors": "0",
            "speed": "1000",
            "state": "up",
            "name": "",
            "sampleTime": 0,
            "rxRate": "0.0",
            "txRate": "0.0",
        })))
        .collect();

    // Bare struct — engine's JsonResponseUtil wraps under "info" and adds
    // its own status. If we wrap here, every field reads through info.info
    // (double-wrap → all nulls → NPEs).
    Ok(json!({
            "cpuLoad": format!("{:.2}", load1),
            "cpuIdle": format!("{:.2}", 100.0 - used_pct.min(100.0)),
            "cpuSys":  "0.00",
            "cpuUser": "0.00",
            "cpuSysVdsmd":  "0.00",
            "cpuUserVdsmd": "0.00",
            "loadAvg": {
                "1min":  format!("{:.2}", load1),
                "5min":  format!("{:.2}", load5),
                "15min": format!("{:.2}", load15),
            },

            "memUsed":     used_kb.to_string(),
            "memFree":     (mem.free_kb / 1024).to_string(),
            "memAvailable": (mem.available_kb / 1024).to_string(),
            "memCommitted": "0",
            "swapTotal":   (mem.swap_total_kb / 1024).to_string(),
            "swapFree":    (mem.swap_free_kb / 1024).to_string(),

            "ksmCpu":   "0",
            "ksmPages": "0",
            "ksmState": false,

            "elapsedTime": format!("{:.0}", sysinfo::uptime_secs()),
            "network":     network,

            // Per-CPU stats — engine iterates cpuStatistics to build the
            // per-cpu charts. Empty dict for now; engine handles missing
            // entries but NPEs on null parent.
            "cpuStatistics": {},

            // Per-NUMA-node free memory. Engine reads .entrySet() on this;
            // null → NPE.
            "numaNodeMemFree": {"0": "0"},
            "memShared":       "0",
            "guestOverhead":   "0",
            "statusTime":      "0",
            "netConfigDirty":  "false",
            "status":          "Up",
            "imagesLastCheck": 0.0,
            "imagesLastDelay": 0.0,
            "transparentHugePages": 0,
            "kdumpStatus":     0,
            "bootTime":        "0",
            "dateTime":        iso_utc_now(),
            "timeOffset":      "0",
            "haStats":     { "active": false },
            "vmCount":     0,
            "vmActive":    0,
            "vmMigrating": 0,
            "incomingVmMigrations": 0,
            "outgoingVmMigrations": 0,

            "anonHugePages": "0",
            "diskStats":      {},
            "thpState":       "always",
            "boot_time":      "0",
    }))
}
