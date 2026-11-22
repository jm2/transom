# SPDX-License-Identifier: GPL-3.0-or-later
# Transom — Windows build and packaging (portable .zip, optional Inno Setup installer).
#
# Builds with MSYS2's CLANG64 / CLANGARM64 GTK stack, like Tributary and Balun:
#   pacman -S mingw-w64-clang-x86_64-{toolchain,gtk4,libadwaita,freerdp,ffmpeg,pkg-config,clang}
#   rustup target add x86_64-pc-windows-gnullvm   (aarch64-pc-windows-gnullvm on ARM)
#
#   pwsh scripts/build-windows.ps1 [-Msys2Root C:\msys64] [-NoCargoBuild] [-InnoSetup]
#
# Environment: RUST_TARGET (default from the host arch), MSYS_ENV (clang64|clangarm64).
# Output: dist\transom-windows\ (bin\transom.exe + DLLs, share\, lib\),
# dist\transom-windows.zip, and with -InnoSetup dist\transom-setup.exe.
#
# The layout keeps MSYS2's prefix shape, so GLib/GTK find share\ and lib\ from their
# own DLL location; nothing sets environment variables at run time. DLLs are found
# from PE import tables with llvm-readobj (not ldd, which runs the binaries).
[CmdletBinding()]
param(
    [string]$Msys2Root = $(if ($env:MSYS2_ROOT) { $env:MSYS2_ROOT } else { 'C:\msys64' }),
    [switch]$NoCargoBuild,
    [switch]$InnoSetup
)
$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
$repo = Split-Path -Parent $PSScriptRoot
Set-Location $repo

function Info($m) { Write-Host "==> $m" }
function Fail($m) { Write-Error "build-windows: $m"; exit 1 }

# ---- toolchain -----------------------------------------------------------------
$arm = [System.Runtime.InteropServices.RuntimeInformation]::OSArchitecture -eq 'Arm64'
$msysEnv = if ($env:MSYS_ENV) { $env:MSYS_ENV } elseif ($arm) { 'clangarm64' } else { 'clang64' }
$target = if ($env:RUST_TARGET) { $env:RUST_TARGET } elseif ($msysEnv -eq 'clangarm64') {
    'aarch64-pc-windows-gnullvm' } else { 'x86_64-pc-windows-gnullvm' }
$innoArch = if ($target -like 'aarch64-*') { 'arm64' } else { 'x64' }
$prefix = Join-Path $Msys2Root $msysEnv
$prefixBin = Join-Path $prefix 'bin'
foreach ($tool in 'llvm-readobj.exe', 'glib-compile-schemas.exe', 'gdbus.exe') {
    if (-not (Test-Path (Join-Path $prefixBin $tool))) {
        Fail "$tool not found in $prefixBin (install the MSYS2 $msysEnv packages listed above)"
    }
}
$env:PATH = "$prefixBin;$env:PATH"
$env:PKG_CONFIG_ALLOW_CROSS = '1'
$env:PKG_CONFIG_PATH = Join-Path $prefix 'lib\pkgconfig'
$env:LIBCLANG_PATH = $prefixBin   # bindgen (FreeRDP and libavcodec bindings)

# ---- build ---------------------------------------------------------------------
$exe = Join-Path $repo "target\$target\release\transom.exe"
if (-not $NoCargoBuild) {
    Info "cargo build --release --target $target -p transom"
    cargo build --release --locked --target $target -p transom
    if ($LASTEXITCODE -ne 0) { Fail 'cargo build failed' }
}
if (-not (Test-Path $exe)) { Fail "missing $exe" }

# A release build must be a GUI-subsystem executable (no console window).
$bytes = [System.IO.File]::ReadAllBytes($exe)
$pe = [BitConverter]::ToInt32($bytes, 0x3c)
$subsystem = [BitConverter]::ToUInt16($bytes, $pe + 24 + 68)
if ($subsystem -ne 2) { Fail "transom.exe is not a GUI-subsystem executable (subsystem $subsystem)" }

# ---- stage ---------------------------------------------------------------------
$dist = Join-Path $repo 'dist\transom-windows'
if (Test-Path $dist) { Remove-Item -Recurse -Force $dist }
$bin = Join-Path $dist 'bin'
New-Item -ItemType Directory -Force $bin | Out-Null
Copy-Item $exe $bin
Copy-Item (Join-Path $prefixBin 'gdbus.exe') $bin   # GApplication's D-Bus helper

# GDK-Pixbuf loaders (PNG/SVG icons); the cache uses paths relative to the prefix,
# which GDK-Pixbuf resolves from its own DLL on Windows.
$loadersSrc = Join-Path $prefix 'lib\gdk-pixbuf-2.0'
$loadersDst = Join-Path $dist 'lib\gdk-pixbuf-2.0'
Copy-Item -Recurse $loadersSrc $loadersDst

# DLL closure from PE import tables (exe, gdbus, pixbuf loaders), breadth first.
$queue = [System.Collections.Generic.Queue[string]]::new()
$seen = [System.Collections.Generic.HashSet[string]]::new([StringComparer]::OrdinalIgnoreCase)
Get-ChildItem $bin -Filter *.exe | ForEach-Object { $queue.Enqueue($_.FullName) }
Get-ChildItem $loadersDst -Recurse -Filter *.dll | ForEach-Object { $queue.Enqueue($_.FullName) }
while ($queue.Count -gt 0) {
    $file = $queue.Dequeue()
    $imports = & (Join-Path $prefixBin 'llvm-readobj.exe') --coff-imports $file |
        Select-String -Pattern '^\s*Name:\s*(\S+\.dll)\s*$' |
        ForEach-Object { $_.Matches[0].Groups[1].Value }
    foreach ($dll in $imports) {
        if (-not $seen.Add($dll)) { continue }
        $src = Join-Path $prefixBin $dll
        if (Test-Path $src) {          # from MSYS2: bundle it; system DLLs aren't there
            Copy-Item $src $bin
            $queue.Enqueue((Join-Path $bin $dll))
        }
    }
}
Info "bundled $((Get-ChildItem $bin -Filter *.dll).Count) DLLs"
# MSYS2's loaders.cache (copied with the loaders) uses prefix-relative paths, which
# GDK-Pixbuf resolves from its own DLL location: valid wherever the bundle is.
if (-not (Test-Path (Join-Path $loadersDst '2.10.0\loaders.cache'))) { Fail 'loaders.cache missing' }

# Icons (Adwaita, hicolor, the app's own) and GLib schemas.
$share = Join-Path $dist 'share'
New-Item -ItemType Directory -Force (Join-Path $share 'icons'), (Join-Path $share 'glib-2.0\schemas') | Out-Null
foreach ($theme in 'Adwaita', 'hicolor') {
    $src = Join-Path $prefix "share\icons\$theme"
    if (Test-Path $src) { Copy-Item -Recurse $src (Join-Path $share 'icons') }
}
Copy-Item -Recurse -Force (Join-Path $repo 'data\icons\hicolor\*') (Join-Path $share 'icons\hicolor')
Copy-Item (Join-Path $prefix 'share\glib-2.0\schemas\*.xml') (Join-Path $share 'glib-2.0\schemas')
& (Join-Path $prefixBin 'glib-compile-schemas.exe') (Join-Path $share 'glib-2.0\schemas')
if ($LASTEXITCODE -ne 0) { Fail 'glib-compile-schemas failed' }

Copy-Item (Join-Path $repo 'COPYING') (Join-Path $dist 'COPYING.txt')
Copy-Item (Join-Path $repo 'build-aux\packaging\THIRD-PARTY-NOTICES.txt') $dist

# ---- zip / installer -------------------------------------------------------------
$zip = Join-Path $repo 'dist\transom-windows.zip'
if (Test-Path $zip) { Remove-Item -Force $zip }
Compress-Archive -Path (Join-Path $dist '*') -DestinationPath $zip
Info "wrote $zip"

if ($InnoSetup) {
    $iscc = @($env:TRANSOM_ISCC, "${env:ProgramFiles(x86)}\Inno Setup 6\ISCC.exe",
        "${env:ProgramFiles}\Inno Setup 6\ISCC.exe",
        "${env:LOCALAPPDATA}\Programs\Inno Setup 6\ISCC.exe") |
        Where-Object { $_ -and (Test-Path $_) } | Select-Object -First 1
    if (-not $iscc) { Fail 'Inno Setup 6 (ISCC.exe) not found; choco install innosetup' }
    $version = (Select-String -Path (Join-Path $repo 'Cargo.toml') -Pattern '^version\s*=\s*"([^"]+)"' |
        Select-Object -First 1).Matches[0].Groups[1].Value
    $numeric = (($version -split '[-+]')[0] -split '\.' + @('0', '0', '0', '0'))[0..3] -join '.'
    & $iscc "/DAppVersion=$version" "/DAppNumericVersion=$numeric" "/DSourceDir=$dist" `
        "/DOutputDir=$(Join-Path $repo 'dist')" "/DTargetArch=$innoArch" `
        (Join-Path $repo 'build-aux\inno\transom.iss')
    if ($LASTEXITCODE -ne 0) { Fail 'Inno Setup failed' }
    Info "wrote dist\transom-setup.exe"
}
