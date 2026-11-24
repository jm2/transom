# SPDX-License-Identifier: GPL-3.0-or-later
# Fedora / COPR spec. Sources from scripts/dist.sh (vendored crates: offline build).
# The upstream version in SemVer spelling; Version is its RPM spelling (a prerelease
# such as 0.2.0-alpha.1 becomes 0.2.0~alpha.1). scripts/release_check.py binds both to
# the tag, and Packit rewrites Version the same way.
%global upstream_version 0.0.1

Name:           transom
Version:        0.0.1
Release:        1%{?dist}
Summary:        Sharp remote desktops over RDP
# App GPL-3.0-or-later; the bundled transom-view libraries Apache-2.0; vendored crates
# under various permissive licences (build-aux/packaging/RUST-CRATE-LICENSES.txt).
License:        GPL-3.0-or-later AND Apache-2.0 AND MIT AND (MIT OR Apache-2.0)
URL:            https://github.com/jm2/transom
Source0:        %{name}-%{upstream_version}.tar.gz
Source1:        %{name}-%{upstream_version}-vendor.tar.xz

BuildRequires:  cargo >= 1.98
BuildRequires:  rust >= 1.98
BuildRequires:  clang-devel
BuildRequires:  pkgconfig(freerdp3) >= 3.15
BuildRequires:  pkgconfig(freerdp-client3)
BuildRequires:  pkgconfig(winpr3)
# Zero-copy H.264 (VA-API through libavcodec). Fedora's libavcodec-free has no native
# H.264 decoder, so this engages only with a full libavcodec (e.g. RPM Fusion's
# libavcodec-freeworld, same soname); otherwise FreeRDP decodes in software.
BuildRequires:  pkgconfig(libavcodec)
BuildRequires:  pkgconfig(libavutil)
BuildRequires:  pkgconfig(gtk4) >= 4.16
BuildRequires:  pkgconfig(libadwaita-1) >= 1.6
BuildRequires:  pkgconfig(libsecret-1)
BuildRequires:  desktop-file-utils
BuildRequires:  libappstream-glib

%description
Transom connects to Windows and GNOME computers using the Remote Desktop Protocol,
rendering remote sessions pixel for pixel at 100%%, 200%% and fractional scales.

%prep
%autosetup -n %{name}-%{upstream_version}
tar -xJf %{SOURCE1}
mkdir -p .cargo
cat > .cargo/config.toml <<CFG
[source.crates-io]
replace-with = "vendored-sources"
[source.vendored-sources]
directory = "vendor"
CFG

%build
# Fedora's Rust flags (debuginfo for the -debuginfo package, frame pointers, ...), as
# %%cargo_build would set them; %%check uses the same so nothing is rebuilt.
export RUSTFLAGS="%{?build_rustflags}"
cargo build --release --locked --offline -p transom

%install
install -Dm755 target/release/transom %{buildroot}%{_bindir}/transom
install -Dm644 data/io.github.jm2.Transom.desktop %{buildroot}%{_datadir}/applications/io.github.jm2.Transom.desktop
install -Dm644 data/io.github.jm2.Transom.metainfo.xml %{buildroot}%{_metainfodir}/io.github.jm2.Transom.metainfo.xml
install -Dm644 data/io.github.jm2.Transom.mime.xml %{buildroot}%{_datadir}/mime/packages/io.github.jm2.Transom.xml
for icon in data/icons/hicolor/*/apps/io.github.jm2.Transom.*; do
    install -Dm644 "$icon" "%{buildroot}%{_datadir}/${icon#data/}"
done

%check
desktop-file-validate %{buildroot}%{_datadir}/applications/io.github.jm2.Transom.desktop
appstream-util validate-relax --nonet %{buildroot}%{_metainfodir}/io.github.jm2.Transom.metainfo.xml
export RUSTFLAGS="%{?build_rustflags}"
cargo test --release --locked --offline -p transom-view-core

%files
%license COPYING LICENSE-APACHE build-aux/packaging/RUST-CRATE-LICENSES.txt
%doc README.md
%{_bindir}/transom
%{_datadir}/applications/io.github.jm2.Transom.desktop
%{_metainfodir}/io.github.jm2.Transom.metainfo.xml
%{_datadir}/mime/packages/io.github.jm2.Transom.xml
%{_datadir}/icons/hicolor/*/apps/io.github.jm2.Transom.*

%changelog
* Sat Nov 21 2026 Transom Contributors - 0.0.1-1
- First development snapshot
