# SPDX-License-Identifier: GPL-3.0-or-later
# Fedora / COPR spec. Sources from scripts/dist.sh (vendored crates: offline build).
Name:           transom
Version:        0.0.1
Release:        1%{?dist}
Summary:        Sharp remote desktops over RDP
# App GPL-3.0-or-later; the bundled transom-view libraries Apache-2.0; vendored crates
# under various permissive licences (see vendor/).
License:        GPL-3.0-or-later AND Apache-2.0 AND MIT AND (MIT OR Apache-2.0)
URL:            https://github.com/jm2/transom
Source0:        %{name}-%{version}.tar.gz
Source1:        %{name}-%{version}-vendor.tar.xz

BuildRequires:  cargo >= 1.98
BuildRequires:  rust >= 1.98
BuildRequires:  clang-devel
BuildRequires:  pkgconfig(freerdp3) >= 3.15
BuildRequires:  pkgconfig(freerdp-client3)
BuildRequires:  pkgconfig(winpr3)
BuildRequires:  pkgconfig(gtk4) >= 4.16
BuildRequires:  pkgconfig(libadwaita-1) >= 1.6
BuildRequires:  pkgconfig(libsecret-1)
BuildRequires:  desktop-file-utils
BuildRequires:  libappstream-glib

%description
Transom connects to Windows and GNOME computers using the Remote Desktop Protocol,
rendering remote sessions pixel for pixel at 100%%, 200%% and fractional scales.

%prep
%autosetup -n %{name}-%{version}
tar -xJf %{SOURCE1}
mkdir -p .cargo
cat > .cargo/config.toml <<CFG
[source.crates-io]
replace-with = "vendored-sources"
[source.vendored-sources]
directory = "vendor"
CFG

%build
cargo build --release --locked --offline -p transom

%install
install -Dm755 target/release/transom %{buildroot}%{_bindir}/transom
install -Dm644 data/io.github.jm2.Transom.desktop %{buildroot}%{_datadir}/applications/io.github.jm2.Transom.desktop
install -Dm644 data/io.github.jm2.Transom.metainfo.xml %{buildroot}%{_metainfodir}/io.github.jm2.Transom.metainfo.xml
install -Dm644 data/io.github.jm2.Transom.mime.xml %{buildroot}%{_datadir}/mime/packages/io.github.jm2.Transom.xml
install -Dm644 data/icons/hicolor/scalable/apps/io.github.jm2.Transom.svg %{buildroot}%{_datadir}/icons/hicolor/scalable/apps/io.github.jm2.Transom.svg

%check
desktop-file-validate %{buildroot}%{_datadir}/applications/io.github.jm2.Transom.desktop
appstream-util validate-relax --nonet %{buildroot}%{_metainfodir}/io.github.jm2.Transom.metainfo.xml
cargo test --release --locked --offline -p transom-view-core

%files
%license COPYING LICENSE-APACHE
%doc README.md
%{_bindir}/transom
%{_datadir}/applications/io.github.jm2.Transom.desktop
%{_metainfodir}/io.github.jm2.Transom.metainfo.xml
%{_datadir}/mime/packages/io.github.jm2.Transom.xml
%{_datadir}/icons/hicolor/scalable/apps/io.github.jm2.Transom.svg

%changelog
* Sat Nov 21 2026 Transom Contributors - 0.0.1-1
- First development snapshot
