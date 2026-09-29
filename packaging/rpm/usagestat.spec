Name:           usagestat
Version:        1.0.3
Release:        1%{?dist}
Summary:        Scriptable CLI for local agent usage data

License:        MIT
URL:            https://github.com/hashimkarim/usagestat
Source0:        %{url}/archive/refs/tags/v%{version}.tar.gz

BuildRequires:  cargo
BuildRequires:  gcc
BuildRequires:  openssl-devel
BuildRequires:  pkgconfig
BuildRequires:  rust
BuildRequires:  systemd-rpm-macros
Requires:       python3
%{?systemd_ordering}

%description
usagestat is a scriptable CLI for probing and exporting local agent usage data.

%prep
%autosetup -n usagestat-%{version}

%build
cargo build --release --locked -p usagestat-cli -p usagestat-daemon

%install
install -Dm0755 target/release/usagestat %{buildroot}%{_bindir}/usagestat
install -Dm0755 target/release/usagestatd %{buildroot}%{_bindir}/usagestatd
mkdir -p %{buildroot}%{_datadir}/usagestat/plugins
cp -a plugins/. %{buildroot}%{_datadir}/usagestat/plugins/
install -Dm0644 tools/updates/service-sync.py %{buildroot}%{_libexecdir}/usagestat/service-sync.py
install -Dm0644 tools/updates/rpm-service-sync.py %{buildroot}%{_libexecdir}/usagestat/rpm-service-sync.py
install -Dm0644 tools/updates/usagestat-package-sync.service %{buildroot}%{_userunitdir}/usagestat-package-sync.service
%if ! 0%{?usagestat_alpha}
install -Dm0644 tools/updates/usagestat-rpm-update.service %{buildroot}%{_unitdir}/usagestat-rpm-update.service
install -Dm0644 tools/updates/usagestat-rpm-update.timer %{buildroot}%{_unitdir}/usagestat-rpm-update.timer
install -Dm0644 packaging/rpm/usagestat-copr.repo %{buildroot}%{_sysconfdir}/yum.repos.d/_copr:copr.fedorainfracloud.org:hashimkarim:usagestat.repo
%endif

%check
cargo test --locked -p usagestat-cli -p usagestat-daemon

%post
%if ! 0%{?usagestat_alpha}
%systemd_post usagestat-rpm-update.timer
%endif

%posttrans
# Probe only RPM-owned running readback services, after all replacement files
# are installed. This helper never changes daemon intent or SDK installations.
/usr/bin/python3 %{_libexecdir}/usagestat/rpm-service-sync.py || :

%preun
%if ! 0%{?usagestat_alpha}
%systemd_preun usagestat-rpm-update.timer
%endif

%files
%license LICENSE
%{_bindir}/usagestat
%{_bindir}/usagestatd
%{_datadir}/usagestat/plugins
%{_libexecdir}/usagestat
%{_userunitdir}/usagestat-package-sync.service
%if ! 0%{?usagestat_alpha}
%{_unitdir}/usagestat-rpm-update.service
%{_unitdir}/usagestat-rpm-update.timer
%config(noreplace) %{_sysconfdir}/yum.repos.d/_copr:copr.fedorainfracloud.org:hashimkarim:usagestat.repo
%endif

%changelog
* Sun Sep 06 2026 Hashim-K <Hashim-K@users.noreply.github.com> - 1.0.3-1
- Release 1.0.3 with daemon controls, T3 integration, and dashboard opener

* Mon May 18 2026 Hashim-K <Hashim-K@users.noreply.github.com> - 1.0.2-1
- Add usagestat test https smoke-test command

* Mon May 18 2026 Hashim-K <Hashim-K@users.noreply.github.com> - 1.0.1-1
- Install rustls ring crypto provider before plugin HTTP requests

* Sat May 16 2026 Hashim-K <Hashim-K@users.noreply.github.com> - 1.0.0-1
- Initial RPM package
