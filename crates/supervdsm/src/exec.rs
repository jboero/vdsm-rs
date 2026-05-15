//! Server-side execution of [`PrivOp`]s. Each op is turned into an argv
//! *here*, in the root process, from validated semantic fields — the
//! unprivileged caller never supplies a raw command line.

use tokio::process::Command;

use vdsm_common::supervdsm::{PrivOp, PrivResult};

const MOUNT_ROOT: &str = "/rhev/data-center/mnt/";

/// LVM object names: VG and LV. Conservative allowlist — alnum plus the
/// few punctuation chars LVM itself permits. Rejecting everything else
/// blocks option injection (`-`-prefixed) and path traversal.
fn valid_lvm_name(s: &str) -> bool {
    !s.is_empty()
        && !s.starts_with('-')
        && s.len() <= 128
        && s.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | '+'))
}

/// Block device path. Must live under /dev/, no traversal, no option
/// injection.
fn valid_device(s: &str) -> bool {
    s.starts_with("/dev/")
        && !s.contains("..")
        && !s.contains('\0')
        && s.len() <= 4096
}

/// Mount target. Restricted to the storage-domain mount root so this
/// can't be turned into an arbitrary `mount --bind /` style attack.
fn valid_target(s: &str) -> bool {
    s.starts_with(MOUNT_ROOT) && !s.contains("..") && !s.contains('\0')
}

fn valid_fstype(s: &str) -> bool {
    matches!(
        s,
        "nfs" | "nfs4" | "cifs" | "glusterfs" | "none" | "auto"
            | "ext4" | "xfs" | "ext3"
    )
}

fn valid_portal(s: &str) -> bool {
    !s.is_empty()
        && !s.starts_with('-')
        && !s.contains('\0')
        && s.len() <= 256
        && s.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | ':' | '-' | '[' | ']'))
}

fn valid_iqn(s: &str) -> bool {
    !s.is_empty()
        && !s.starts_with('-')
        && !s.contains('\0')
        && s.len() <= 256
        && s.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | ':' | '-'))
}

fn invalid(field: &str) -> PrivResult {
    PrivResult::failure(format!("rejected: invalid {field}"))
}

async fn run(bin: &str, args: &[&str]) -> PrivResult {
    match Command::new(bin).args(args).output().await {
        Ok(o) => PrivResult {
            ok: o.status.success(),
            code: o.status.code().unwrap_or(-1),
            stdout: String::from_utf8_lossy(&o.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&o.stderr).into_owned(),
        },
        Err(e) => PrivResult::failure(format!("spawn {bin}: {e}")),
    }
}

pub async fn execute(op: PrivOp) -> PrivResult {
    match op {
        PrivOp::Mount { fstype, spec, target, options } => {
            if !valid_fstype(&fstype) { return invalid("fstype"); }
            if !valid_target(&target) { return invalid("target"); }
            if spec.is_empty() || spec.starts_with('-') || spec.contains('\0') {
                return invalid("spec");
            }
            let mut args: Vec<&str> = vec!["-t", &fstype];
            if !options.is_empty() {
                if options.starts_with('-') || options.contains('\0') {
                    return invalid("options");
                }
                args.push("-o");
                args.push(&options);
            }
            args.push(&spec);
            args.push(&target);
            run("/usr/bin/mount", &args).await
        }
        PrivOp::Umount { target } => {
            if !valid_target(&target) { return invalid("target"); }
            run("/usr/bin/umount", &[target.as_str()]).await
        }
        PrivOp::IscsiDiscover { portal } => {
            if !valid_portal(&portal) { return invalid("portal"); }
            run("/usr/sbin/iscsiadm",
                &["-m", "discovery", "-t", "st", "-p", &portal]).await
        }
        PrivOp::IscsiLogin { iqn, portal } => {
            if !valid_iqn(&iqn) { return invalid("iqn"); }
            if !valid_portal(&portal) { return invalid("portal"); }
            run("/usr/sbin/iscsiadm",
                &["-m", "node", "-T", &iqn, "-p", &portal, "--login"]).await
        }
        PrivOp::IscsiLogout { iqn, portal } => {
            if !valid_iqn(&iqn) { return invalid("iqn"); }
            if !valid_portal(&portal) { return invalid("portal"); }
            run("/usr/sbin/iscsiadm",
                &["-m", "node", "-T", &iqn, "-p", &portal, "--logout"]).await
        }
        PrivOp::IscsiRescan => {
            run("/usr/sbin/iscsiadm", &["-m", "session", "--rescan"]).await
        }
        PrivOp::Pvcreate { device } => {
            if !valid_device(&device) { return invalid("device"); }
            run("/usr/sbin/pvcreate", &["-ff", "-y", &device]).await
        }
        PrivOp::Vgcreate { vg, devices } => {
            if !valid_lvm_name(&vg) { return invalid("vg"); }
            if devices.is_empty() || !devices.iter().all(|d| valid_device(d)) {
                return invalid("devices");
            }
            let mut args: Vec<&str> = vec!["-s", "128m", "-y", &vg];
            args.extend(devices.iter().map(String::as_str));
            run("/usr/sbin/vgcreate", &args).await
        }
        PrivOp::Vgextend { vg, device } => {
            if !valid_lvm_name(&vg) { return invalid("vg"); }
            if !valid_device(&device) { return invalid("device"); }
            run("/usr/sbin/vgextend", &[&vg, &device]).await
        }
        PrivOp::Vgremove { vg } => {
            if !valid_lvm_name(&vg) { return invalid("vg"); }
            run("/usr/sbin/vgremove", &["-f", "-y", &vg]).await
        }
        PrivOp::Lvcreate { vg, lv, size_bytes } => {
            if !valid_lvm_name(&vg) { return invalid("vg"); }
            if !valid_lvm_name(&lv) { return invalid("lv"); }
            if size_bytes == 0 { return invalid("size_bytes"); }
            let size = format!("{size_bytes}B");
            run("/usr/sbin/lvcreate",
                &["-W", "y", "-Z", "n", "-n", &lv, "-L", &size, &vg]).await
        }
        PrivOp::Lvchange { vg, lv, active } => {
            if !valid_lvm_name(&vg) { return invalid("vg"); }
            if !valid_lvm_name(&lv) { return invalid("lv"); }
            let path = format!("{vg}/{lv}");
            if active {
                run("/usr/sbin/lvchange", &["-a", "y", "-K", &path]).await
            } else {
                run("/usr/sbin/lvchange", &["-a", "n", &path]).await
            }
        }
        PrivOp::Lvremove { vg, lv } => {
            if !valid_lvm_name(&vg) { return invalid("vg"); }
            if !valid_lvm_name(&lv) { return invalid("lv"); }
            run("/usr/sbin/lvremove", &["-f", "-y", &format!("{vg}/{lv}")]).await
        }
        PrivOp::FcScan => {
            // Iterate /sys/class/scsi_host/*/scan and trigger a rescan.
            // Done directly (no subprocess) since it's just sysfs writes.
            let mut errs = Vec::new();
            match std::fs::read_dir("/sys/class/scsi_host") {
                Ok(rd) => {
                    for ent in rd.flatten() {
                        let p = ent.path().join("scan");
                        if let Err(e) = std::fs::write(&p, "- - -\n") {
                            errs.push(format!("{}: {e}", p.display()));
                        }
                    }
                }
                Err(e) => errs.push(format!("read scsi_host: {e}")),
            }
            if errs.is_empty() {
                PrivResult { ok: true, code: 0, stdout: String::new(), stderr: String::new() }
            } else {
                PrivResult::failure(errs.join("; "))
            }
        }
        PrivOp::MultipathList => {
            run("/usr/sbin/multipath", &["-ll", "-j"]).await
        }
    }
}
