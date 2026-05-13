# Schema patches vs. upstream `lib/vdsm/api/vdsm-api.yml`

The vendored `vdsm-api.yml` is **not byte-identical** to `oVirt/vdsm:master`.
Two minimal structural fixes are applied so the codegen tree-walks
correctly even with our duplicate-tolerant loader; PyYAML hides both bugs
behind silent last-wins, so upstream Python VDSM never noticed them.

When re-vendoring from upstream, re-apply (or, better, push these as
patches to oVirt/vdsm).

---

## 1. `ExternalVmParams.qcow2_compat` is missing its list-item dash

**Upstream** (around line 1044):

```yaml
        -   defaultvalue: null
            description: disk allocation type (sparse, preallocated)
            name: allocation
            type: *VolumeAllocation

            added: '4.2'
            description: set version of QCOW2 images (0.10, 1.1);
                ignored for KVM imports
            name: qcow2_compat
            type: string
```

The `added/description/name/type` block at the bottom is meant to be a
**second list item** (a separate property called `qcow2_compat`) but the
`-` separator is missing. PyYAML reads it as four duplicate keys on the
single `allocation` entry, silently overwriting; the `allocation`
property is effectively erased at load time.

**Patched form**: a `-` is inserted in front of `added: '4.2'`, restoring
both `allocation` and `qcow2_compat` as distinct properties.

## 2. `HostDevices` declares `name: HostDevices` twice

**Upstream** (around line 1897):

```yaml
    HostDevices: &HostDevices
        added: '3.6'
        description: Mapping of device names to device details
        name: HostDevices
        key-type: string
        name: HostDevices
        type: map
        value-type: *HostDevice
```

The second `name: HostDevices` is a copy-paste leftover. Last-wins makes
this a no-op; we drop it for cleanliness.

---

The duplicate-key tolerance is implemented in `crates/vdsm-schema/build.rs`
via a private `LenientLoader` that mirrors `yaml_rust2::yaml::YamlLoader`
but silently overwrites on duplicate insert. That handles any *other*
duplicates the upstream schema may carry without failing the build.
