%global vdsm_user         vdsm
%global vdsm_group        vdsm
# Module *file* / *artifact* name. Must be a C ident (no hyphen) because
# SELinux's checkmodule rejects mismatched module name vs. output base.
%global selinux_modname   vdsm_rs
%global selinuxtype       targeted

Name:           vdsm-rs
# Epoch:1 lets us outrank real upstream vdsm (which ships with no Epoch,
# i.e. Epoch:0) regardless of how low our %{version} is. Anyone with
# `Requires: vdsm >= 4.x` is satisfied by `Provides: vdsm = 1:%{version}`.
Epoch:          1
Version:        4.5.7
Release:        1%{?dist}
Summary:        Rust rewrite of oVirt's VDSM host daemon

License:        GPL-2.0-or-later
URL:            https://ovirt.org
# Tarball top-level directory is `vdsm-rs/` to match the unpack rule below.
Source0:        %{name}.tar
Source1:        %{name}.sysusers

# Pull the selinux subpackage onto every install by default. Users on
# SELinux-disabled hosts can `dnf install vdsm-rs --setopt=install_weak_deps=False`.
Recommends:     %{name}-selinux = %{epoch}:%{version}-%{release}

ExclusiveArch:  x86_64 aarch64

BuildRequires:  cargo
BuildRequires:  rust >= 1.80
BuildRequires:  systemd-rpm-macros
BuildRequires:  pkgconfig(libvirt) >= 9.0
BuildRequires:  gcc
BuildRequires:  make
# For building the SELinux policy module under packaging/selinux/.
BuildRequires:  selinux-policy-devel

%if 0%{?fedora}
BuildRequires:  pkgconfig(libnl-3.0)
BuildRequires:  pkgconfig(libnl-route-3.0)
%endif

%if 0%{?rhel} && 0%{?rhel} >= 10
BuildRequires:  pkgconfig(libnl-3.0)
BuildRequires:  pkgconfig(libnl-route-3.0)
%endif

Requires:       libvirt-daemon
Requires:       libvirt-daemon-driver-qemu
Requires:       qemu-kvm
# ovirt-host-deploy unconditionally runs `tuned-adm profile virtual-host`,
# which requires the tuned daemon to be installed and the dbus interface
# reachable. We don't tune anything ourselves, but tuned has to be there
# for the deploy to advance past the misc stage.
Requires:       tuned
%{?systemd_requires}
# systemd-sysusers handles the vdsm user; the shipped sysusers.d file
# auto-generates Provides: user(vdsm) / group(vdsm) via systemd-rpm-macros.
%{?sysusers_requires_compat}

# Pose as the upstream Python vdsm package family. ovirt-engine's
# host-deploy ansible role + the `ovirt-host` meta-package both probe
# package_facts for these names; we satisfy them at the RPM dep layer
# without inheriting the actual Python codebase.
#
# The Conflicts forces dnf to remove real vdsm in the same transaction
# rather than fail on overlapping /usr/sbin/vdsmd, /etc/vdsm, etc.
# (We genuinely cannot coexist on disk.)
# NOTE: `vdsm` itself is shipped as a separate (empty) subpackage below
# so ovirt-engine's host-deploy ansible role can find a package literally
# named `vdsm` via ansible.builtin.package_facts (which queries the RPM
# DB by exact name, not Provides). The other vdsm-* names are fine as
# pure Provides since nothing inspects them via package_facts.
Provides:       vdsm-common = %{epoch}:%{version}-%{release}
Provides:       vdsm-python = %{epoch}:%{version}-%{release}
Provides:       vdsm-network = %{epoch}:%{version}-%{release}
Provides:       vdsm-http = %{epoch}:%{version}-%{release}
Provides:       vdsm-jsonrpc = %{epoch}:%{version}-%{release}
Provides:       vdsm-yajsonrpc = %{epoch}:%{version}-%{release}
Provides:       vdsm-api = %{epoch}:%{version}-%{release}
Provides:       vdsm-client = %{epoch}:%{version}-%{release}
# ovirt-engine's host-deploy ansible installs these unconditionally
# regardless of "Hosted Engine = None" — they're the host baseline.
# We don't ship the corresponding code (hosted-engine, imageio, vmconsole
# host bits are all out of v0 scope), but Provides satisfies dnf so
# host-deploy can advance to the parts we DO care about (vdsm JSON-RPC).
Provides:       ovirt-host = 4.5.0
Provides:       ovirt-host-deploy = 1.9.0
Provides:       ovirt-hosted-engine-setup = 2.7.0
Provides:       ovirt-hosted-engine-ha = 2.5.0
Provides:       ovirt-imageio-daemon = 2.5.0
Provides:       ovirt-imageio-client = 2.5.0
Provides:       ovirt-imageio-common = 2.5.0
Provides:       ovirt-vmconsole-host = 1.0.9
Provides:       safelease = 1.0.0
Provides:       mom = 0.6.0
Provides:       cockpit-ovirt-dashboard = 0.16.0
# OVN host-side packages. Host-deploy installs these even with virt-only
# clusters because the role unconditionally adds them. We don't run OVN
# in vdsm-rs v0; the Provides exist purely so dnf considers them satisfied.
Provides:       ovirt-provider-ovn-driver = 1.2.36
Provides:       openvswitch-ovn-host = 3.4
Provides:       openvswitch = 3.4
Provides:       ovn-host = 24.09.0
Provides:       ovn = 24.09.0
Provides:       openvswitch-ipsec = 3.4
Obsoletes:      vdsm-common < 5.0
Obsoletes:      vdsm-python < 5.0
Obsoletes:      vdsm-network < 5.0
Obsoletes:      vdsm-http < 5.0
Obsoletes:      vdsm-jsonrpc < 5.0
Obsoletes:      vdsm-yajsonrpc < 5.0
Obsoletes:      vdsm-api < 5.0
Obsoletes:      vdsm-client < 5.0
# (Conflicts: vdsm intentionally removed — we now ship our own `vdsm`
# subpackage. Upstream Python vdsm is excluded via the `vdsm` subpackage
# Obsoletes line below.)
# Pull the vdsm shim subpackage by default so `dnf install vdsm-rs`
# results in package_facts['vdsm'] resolving without a second dnf line.
Requires:       vdsm = %{epoch}:%{version}-%{release}

%description
vdsm-rs is a Rust rewrite of the oVirt VDSM host-side daemon, targeting
modern Fedora and EL distributions. v0 is intentionally limited:
file-based (NFS) storage domains only; no Gluster, hosted-engine, OVS, or
LVM/SAN. Wire-compatible with current ovirt-engine.

%package -n supervdsm
Summary:        Privileged helper for vdsm-rs
# Epoch must match — main package is Epoch:1; without an explicit epoch
# prefix this Requires resolves to epoch 0 and never matches.
Requires:       %{name} = %{epoch}:%{version}-%{release}

%description -n supervdsm
Small, polkit-mediated privileged helper for vdsm-rs. The only
component of vdsm-rs that runs as root.

%package -n vdsm
Summary:        Compatibility shim — registers vdsm-rs as `vdsm` in the RPM DB
BuildArch:      noarch
# Inherit Epoch from %{name}; ansible package_facts compares the Version
# field as a string (via `| float` after a strip), so 0.1.0 is fine —
# the filter ValueError fallback returns 0.0 and the conditional is benign.
Requires:       %{name} = %{epoch}:%{version}-%{release}
Provides:       vdsm = %{epoch}:%{version}-%{release}
Obsoletes:      vdsm < 5.0

%description -n vdsm
ovirt-engine's host-deploy ansible role queries `package_facts` for a
package literally named `vdsm`. RPM Provides do not satisfy that lookup
(package_facts compares names, not capabilities). This subpackage ships
no files — its purpose is to exist in the RPM DB so that
`ansible_facts.packages['vdsm']` resolves on a vdsm-rs host. All actual
daemon functionality lives in the vdsm-rs package this Requires.

%package selinux
Summary:        SELinux policy module for vdsm-rs
BuildArch:      noarch
Requires:       %{name} = %{epoch}:%{version}-%{release}
Requires(post): selinux-policy-base >= %{_selinux_policy_version}
Requires(post): policycoreutils
# semanage(8) lives here — needed to label port 54321 as vdsmd_port_t.
Requires(post): policycoreutils-python-utils
Requires(postun): policycoreutils-python-utils
%{?selinux_requires}
# Replace upstream Python vdsm-selinux entirely.
Provides:       vdsm-selinux = %{epoch}:%{version}-%{release}
Obsoletes:      vdsm-selinux < 5.0

%description selinux
SELinux policy module that confines vdsmd to its own domain
(vdsmd_t), labels the daemon's config / state / log / runtime trees,
and grants only the syscalls and port bindings the daemon currently
needs.

%prep
%autosetup -n %{name}

%build
cargo build --release --locked || cargo build --release
# SELinux policy module — produces packaging/selinux/vdsm-rs.pp.
make -C packaging/selinux

%install
install -D -m 0755 target/release/vdsmd      %{buildroot}%{_sbindir}/vdsmd
install -D -m 0755 target/release/supervdsmd %{buildroot}%{_sbindir}/supervdsmd
install -D -m 0644 packaging/systemd/vdsmd.service \
    %{buildroot}%{_unitdir}/vdsmd.service
install -D -m 0644 packaging/systemd/supervdsmd.service \
    %{buildroot}%{_unitdir}/supervdsmd.service
install -D -m 0755 target/release/vdsm-tool   %{buildroot}%{_bindir}/vdsm-tool
install -D -m 0755 target/release/vdsm-client %{buildroot}%{_bindir}/vdsm-client
install -D -m 0640 config/vdsm.toml.example \
    %{buildroot}%{_sysconfdir}/vdsm/vdsm.toml
install -D -m 0644 %{SOURCE1} \
    %{buildroot}%{_sysusersdir}/%{name}.conf

# SDDM / display-manager drop-in: hide the daemon users from the login
# screen. Needed because sysusers may land vdsm / openvswitch in the
# UID_MIN range on systems where the system UID pool is fragmented.
install -d -m 0755 %{buildroot}%{_sysconfdir}/sddm.conf.d
cat > %{buildroot}%{_sysconfdir}/sddm.conf.d/00-vdsm-rs.conf <<'EOF'
# Installed by vdsm-rs — hide service users from the SDDM user list.
[Users]
HideUsers=vdsm,openvswitch
EOF
chmod 0644 %{buildroot}%{_sysconfdir}/sddm.conf.d/00-vdsm-rs.conf

# Runtime / state / log dirs (also re-asserted via systemd's
# RuntimeDirectory/StateDirectory in the unit, but dnf wants them
# present on initial install).
install -d -m 0755 %{buildroot}%{_sharedstatedir}/vdsm
install -d -m 0755 %{buildroot}%{_localstatedir}/log/vdsm

# /etc/pki/vdsm tree expected by ovirt-engine's vdsm-certificates
# ansible role (it pushes certs/keys to fixed paths and chmods them).
install -d -m 0755 %{buildroot}%{_sysconfdir}/pki/vdsm
install -d -m 0755 %{buildroot}%{_sysconfdir}/pki/vdsm/certs
install -d -m 0755 %{buildroot}%{_sysconfdir}/pki/vdsm/keys
install -d -m 0755 %{buildroot}%{_sysconfdir}/pki/vdsm/libvirt-spice
install -d -m 0755 %{buildroot}%{_sysconfdir}/pki/vdsm/libvirt-migrate
install -d -m 0755 %{buildroot}%{_sysconfdir}/pki/vdsm/libvirt-vnc
install -d -m 0755 %{buildroot}%{_sysconfdir}/pki/vdsm/requests
install -d -m 0755 %{buildroot}%{_sysconfdir}/pki/vdsm/requests-qemu
# host-deploy's ovirt-vmconsole-certificates role drops ca/cert/key files
# here; missing dir = aborted deploy. We don't run the vmconsole proxy, but
# the path must exist owned by the shim user.
install -d -m 0755 %{buildroot}%{_sysconfdir}/pki/ovirt-vmconsole

# Stub doc for the `vdsm` shim subpackage — RPM needs at least one
# file owned by a subpackage; this is the only one.
install -d -m 0755 %{buildroot}%{_docdir}/vdsm-shim
echo 'This package exists so ansible package_facts sees an RPM literally named vdsm.
All real functionality lives in the vdsm-rs package.' \
    > %{buildroot}%{_docdir}/vdsm-shim/README

# SELinux policy module + interface header.
install -D -m 0644 packaging/selinux/%{selinux_modname}.pp \
    %{buildroot}%{_datadir}/selinux/packages/%{selinuxtype}/%{selinux_modname}.pp
install -D -m 0644 packaging/selinux/%{selinux_modname}.if \
    %{buildroot}%{_datadir}/selinux/devel/include/distributed/%{selinux_modname}.if

%pre
%sysusers_create_compat %{SOURCE1}

%post
%systemd_post vdsmd.service

%preun
%systemd_preun vdsmd.service

%postun
%systemd_postun_with_restart vdsmd.service

%post -n supervdsm
%systemd_post supervdsmd.service

%preun -n supervdsm
%systemd_preun supervdsmd.service

%postun -n supervdsm
%systemd_postun_with_restart supervdsmd.service

%pre selinux
%selinux_relabel_pre -s %{selinuxtype}

%post selinux
%selinux_modules_install -s %{selinuxtype} %{_datadir}/selinux/packages/%{selinuxtype}/%{selinux_modname}.pp
# Label port 54321 with vdsmd_port_t so vdsmd_t can name_bind it.
# `-a` adds; `-m` modifies if a label is already present (e.g. on
# upgrade or if base policy already claims the port). Either succeeding
# is fine; if both fail (rare), the daemon falls back to unconfined
# bind via base policy and SELinux logs an AVC we can chase.
semanage port -a -t vdsmd_port_t -p tcp 54321 2>/dev/null \
    || semanage port -m -t vdsmd_port_t -p tcp 54321 2>/dev/null \
    || :

%postun selinux
if [ $1 -eq 0 ]; then
    semanage port -d -p tcp 54321 2>/dev/null || :
    %selinux_modules_uninstall -s %{selinuxtype} %{selinux_modname}
fi

%posttrans selinux
%selinux_relabel_post -s %{selinuxtype}

%files
%{_bindir}/vdsmd
%{_bindir}/vdsm-tool
%{_bindir}/vdsm-client
%{_unitdir}/vdsmd.service
%{_sysusersdir}/%{name}.conf
%config(noreplace) %{_sysconfdir}/sddm.conf.d/00-vdsm-rs.conf
# /etc/vdsm + vdsm.toml owned root:vdsm so the daemon (running as user
# `vdsm`) can read its own config. We deliberately keep root as the
# owner (the daemon shouldn't rewrite its own config) and grant read
# via group membership.
%attr(0750, root, %{vdsm_group}) %dir %{_sysconfdir}/vdsm
%attr(0640, root, %{vdsm_group}) %config(noreplace) %{_sysconfdir}/vdsm/vdsm.toml
%attr(0750, %{vdsm_user}, %{vdsm_group}) %dir %{_sharedstatedir}/vdsm
%attr(0750, %{vdsm_user}, %{vdsm_group}) %dir %{_localstatedir}/log/vdsm
# PKI tree owned vdsm:kvm 0750 so engine's ansible role can drop
# certs/keys here and the daemon (vdsm user, kvm group) can read them.
%attr(0750, %{vdsm_user}, kvm) %dir %{_sysconfdir}/pki/vdsm
%attr(0750, %{vdsm_user}, kvm) %dir %{_sysconfdir}/pki/vdsm/certs
%attr(0750, %{vdsm_user}, kvm) %dir %{_sysconfdir}/pki/vdsm/keys
%attr(0750, %{vdsm_user}, kvm) %dir %{_sysconfdir}/pki/vdsm/libvirt-spice
%attr(0750, %{vdsm_user}, kvm) %dir %{_sysconfdir}/pki/vdsm/libvirt-migrate
%attr(0750, %{vdsm_user}, kvm) %dir %{_sysconfdir}/pki/vdsm/libvirt-vnc
%attr(0750, %{vdsm_user}, kvm) %dir %{_sysconfdir}/pki/vdsm/requests
%attr(0750, %{vdsm_user}, kvm) %dir %{_sysconfdir}/pki/vdsm/requests-qemu
%attr(0755, root, root) %dir %{_sysconfdir}/pki/ovirt-vmconsole

%files -n supervdsm
%{_bindir}/supervdsmd
%{_unitdir}/supervdsmd.service

%files -n vdsm
%doc %{_docdir}/vdsm-shim/README

%files selinux
%{_datadir}/selinux/packages/%{selinuxtype}/%{selinux_modname}.pp
%{_datadir}/selinux/devel/include/distributed/%{selinux_modname}.if

%changelog
* Thu Apr 30 2026 vdsm-rs contributors <vdsm-rs@ovirt.org> - 0.1.0-1
- Initial vdsm-rs scaffold: cargo workspace, vdsm-schema codegen from
  vendored vdsm-api.yml, vdsmd / supervdsmd binary stubs, systemd units.
