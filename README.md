# vdsm-rs

A Rust port of oVirt VDSM — the host-side agent that ovirt-engine drives via
JSON-RPC over TLS to manage virtual machines on a hypervisor.

[![COPR build status](https://copr.fedorainfracloud.org/coprs/boeroboy/vdsm-rs/package/vdsm-rs/status_image/last_build.png)](https://copr.fedorainfracloud.org/coprs/boeroboy/vdsm-rs/)
[![License: GPL-2.0-or-later](https://img.shields.io/badge/license-GPL--2.0--or--later-blue.svg)](LICENSE)

<img width="3483" height="2143" alt="vdsm-rs" src="https://github.com/user-attachments/assets/1c3a15b8-d916-4e40-9966-b5404209a90e" />


## Quick start

```fish
# Fedora 43 / 44 / rawhide; x86_64 and aarch64 builds available
sudo dnf copr enable boeroboy/vdsm-rs
sudo dnf install vdsm-rs supervdsm
```

Then in your ovirt-engine UI: **Compute → Hosts → New**, give the Fedora node's
address and root password, click OK. Standard `ovirt-host-deploy` runs against
`vdsm-rs` unchanged — the package satisfies all the role's `package_facts`
checks via RPM `Provides:` aliases.

COPR project: <https://copr.fedorainfracloud.org/coprs/boeroboy/vdsm-rs/>

## Status

Early PoC. Validated against **ovirt-engine 4.5.7** on a CentOS Stream 9 engine
VM with a Fedora 44 node (x86_64). Cross-built and packaged for aarch64 too —
the original test target was a SolidRun HoneyComb LX2K.

**Working today**

- Full *Add Host* flow via the engine UI — ovirt-host-deploy ansible playbook
  passes (137 ok, 50 changed, 0 failed, 1 ignored), host transitions to **Up**.
- Engine reads complete capability inventory: real DMI, real CPU model and
  flags with engine-recognized `model_<X>` tokens, NUMA topology, kernel,
  hugepages, SELinux mode, package versions, etc.
- Host monitoring polls cleanly: `Host.ping2`, `getCapabilities`, `getStats`,
  `getAllVmStats`, `getHardwareInfo`, `dumpxmls`, etc.
- VM lifecycle (`VM.create` / `VM.destroy` / `VM.getStats`) via libvirt.
- External VM ingestion — engine auto-discovers libvirt VMs running on the
  host and imports them as `external-*` managed entities.
- **SELinux enforcing** clean: zero AVCs in steady state, custom policy
  module ships in the `vdsm-rs-selinux` subpackage.

**Out of scope for v0**

- Block / FC / iSCSI storage subsystem
- Hosted-engine deployment
- GlusterFS storage domains
- OVS / OVN networking
- Live migration between hosts
- Sanlock-backed locking

These can be added incrementally; the current focus is making a Fedora node
work as a *compute* member of an existing cluster.

## Why a rewrite?

oVirt has been in active development since 2008 and has grown into a substantial
multi-component virtualization platform. VDSM specifically is the host-side
daemon — roughly 100,000 lines of Python that translate the engine's JSON-RPC
verbs into libvirt and storage operations.

Three things motivated this Rust port:

1. **Python ecosystem drift makes legacy host maintenance painful.** Each
   Fedora release tightens Python compatibility expectations: the move from
   3.12 to 3.14 between Fedora 41 and 44 alone broke several long-standing
   VDSM dependencies (notably tuned's d-bus integration and a handful of
   asyncio APIs). The VDSM codebase carries a decade and a half of accumulated
   workarounds for evolving runtime constraints — keeping it portable to
   current distributions is a serious ongoing maintenance burden, and not
   one the upstream team should be expected to shoulder alone forever.

2. **Component sunsets in the broader ecosystem.** Red Hat Gluster Storage
   left the product portfolio in 2024 with end-of-life through 2026, and the
   role of OVN in virt-only deployments has narrowed. Several VDSM subsystems
   exist primarily to integrate with components that are themselves winding
   down. A clean baseline lets newer Fedora-based deployments skip what they
   no longer need without inheriting the maintenance cost of code that's
   only there for legacy paths.

3. **A memory-safe systems language is a good fit for this layer.** VDSM's
   job — managing privileged host resources for a remote orchestrator over
   a long-lived TLS connection — is exactly the kind of work where Rust's
   strong type discipline and lack of a GIL pay off. The Rust implementation
   so far is roughly 10% the size of the Python equivalent, starts in
   under 100 ms (versus ~3 s), and confines neatly under SELinux.

**This is not an attempt to replace ovirt-engine.** The engine is the heart
of the project — a substantial Java/WildFly application that the maintainers
have refined for fifteen years and which this work has no quarrel with.
`vdsm-rs` is purely a host-side drop-in: it speaks the engine's wire
protocol verbatim, satisfies ovirt-host-deploy's package checks, and
registers with the engine as an unmodified host. An admin running
ovirt-engine 4.5 today can install vdsm-rs on a Fedora node and add it to
an existing cluster with no engine-side changes.

The oVirt maintainers — many of whom I'm privileged to call friends from
Red Hat days — have built and continue to maintain a remarkable platform.
This project is an *offer of an alternative* for the corner of the platform
where the language choice creates friction with modern Linux distributions,
not a critique of their work or a fork of the upstream effort.

## Architecture

```
crates/
├── vdsm-rpc/      JSON-RPC over STOMP over TLS (tokio + rustls)
├── vdsm-host/     Host capabilities, stats, hardware inventory
├── vdsm-virt/     libvirt wrapper, VM lifecycle (virsh shell-out for v0)
├── vdsm-storage/  NFS-only file SD (placeholder)
├── vdsm-network/  Read-only network discovery
├── vdsm-schema/   Codegen from upstream VDSM YAML schema
├── vdsm-common/   Shared types, logging, config
├── vdsmd/         Main daemon binary
├── supervdsm/     Tiny privileged helper for ops vdsm can't do as user vdsm
├── vdsm-tool/     Compat shim — host-deploy probes
└── vdsm-client/   CLI for direct RPC calls
```

Wire-compatible with the upstream VDSM JSON-RPC schema — 205 verbs and 372
types are code-generated from `schema/vdsm-api.yml` at build time. Adding a
new verb is a few lines of Rust plus a `register("Namespace.method", fn)`
call.

The SELinux policy module (`packaging/selinux/`) confines the daemon to its
own `vdsmd_t` domain, with narrow allows for the things VDSM legitimately
needs to do (bind port 54321, exec `virsh` and `ip`, read SELinux enforce
mode, etc.).

## Building from source

```fish
git clone https://github.com/jboero/vdsm-rs
cd vdsm-rs
cargo build --release
# Or, the full RPM family:
tar -cf vdsm-rs.tar --transform 's,^.,vdsm-rs,' .
mkdir -p rpmbuild/{SOURCES,SPECS}
cp vdsm-rs.tar packaging/vdsm-rs.sysusers rpmbuild/SOURCES/
cp packaging/vdsm-rs.spec rpmbuild/SPECS/
rpmbuild --define "_topdir $PWD/rpmbuild" -bb rpmbuild/SPECS/vdsm-rs.spec
```

Requires Rust ≥ 1.80, libvirt-devel, selinux-policy-devel.

## License

GPL-2.0-or-later, matching upstream VDSM.

## Acknowledgements

To the upstream oVirt project (<https://ovirt.org>) and the VDSM and engine
maintainer teams — past and present — for fifteen years of work on the
platform this rests on. Particularly to those who designed the JSON-RPC
schema and engine→host wire protocol that vdsm-rs implements verbatim.
