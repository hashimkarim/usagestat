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

%check
cargo test --locked -p usagestat-cli -p usagestat-daemon

%files
%license LICENSE
%{_bindir}/usagestat
%{_bindir}/usagestatd
%{_datadir}/usagestat/plugins

%changelog
* Sun Sep 06 2026 Hashim-K <Hashim-K@users.noreply.github.com> - 1.0.3-1
- Release 1.0.3 with daemon controls, T3 integration, and dashboard opener

* Mon May 18 2026 Hashim-K <Hashim-K@users.noreply.github.com> - 1.0.2-1
- Add usagestat test https smoke-test command

* Mon May 18 2026 Hashim-K <Hashim-K@users.noreply.github.com> - 1.0.1-1
- Install rustls ring crypto provider before plugin HTTP requests

* Sat May 16 2026 Hashim-K <Hashim-K@users.noreply.github.com> - 1.0.0-1
- Initial RPM package
