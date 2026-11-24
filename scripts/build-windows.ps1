# SPDX-License-Identifier: GPL-3.0-or-later
# Transom — Windows build and packaging (portable .zip, optional Inno Setup installer).
#
# Builds with MSYS2's CLANG64 / CLANGARM64 GTK stack, like Tributary and Balun:
#   pacman -S mingw-w64-clang-x86_64-{toolchain,gtk4,libadwaita,freerdp,ffmpeg,pkg-config,clang}
#   rustup target add x86_64-pc-windows-gnullvm   (aarch64-pc-windows-gnullvm on ARM)
# Build scripts run on the host: either Visual Studio's C++ tools (MSVC host, as on
# GitHub's runners) or the gnullvm host toolchain
# (rustup toolchain install stable-x86_64-pc-windows-gnullvm; RUSTUP_TOOLCHAIN=…).
#
#   pwsh scripts/build-windows.ps1 [-Msys2Root C:\msys64] [-NoCargoBuild] [-InnoSetup]
#
# Environment: RUST_TARGET (default from the host arch), MSYS_ENV (clang64|clangarm64).
# Output: dist\transom-windows\ (bin\transom.exe + DLLs, share\, lib\, and the licence
# files at the top), dist\transom-windows.zip, and with -InnoSetup dist\transom-setup.exe.
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

# Licences: Transom's, the bundled native libraries' and the Rust crates' (the
# installer copies the whole tree, so they land in the install folder too).
Copy-Item (Join-Path $repo 'COPYING') (Join-Path $dist 'COPYING.txt')
Copy-Item (Join-Path $repo 'build-aux\packaging\THIRD-PARTY-NOTICES.txt') $dist
Copy-Item (Join-Path $repo 'build-aux\packaging\RUST-CRATE-LICENSES.txt') $dist

# ---- bundled MSYS2 packages ------------------------------------------------------
# Record the MSYS2 package and exact version behind every bundled file, from pacman's
# local database, so each release names the sources THIRD-PARTY-NOTICES.txt points to
# (BUNDLED-PACKAGES.txt), and copy the licence files those packages ship (licenses\).
# A DLL or EXE that no package owns fails the build: its source can't be identified.
function Write-BundledPackages([string]$Root, [string]$Msys2Root, [string]$MsysEnv, [string]$Title) {
    $db = Join-Path $Msys2Root 'var\lib\pacman\local'
    if (-not (Test-Path $db -PathType Container)) { Fail "MSYS2 package database not found: $db" }
    $rootFull = (Get-Item -LiteralPath $Root).FullName.TrimEnd('\', '/')
    # Bundled files by the path pacman records (clang64/bin/x.dll), since the bundle
    # keeps the prefix layout. Transom's own exe and the top-level licence files aren't
    # MSYS2's; the app icons in share\icons\hicolor simply match no package.
    $wanted = @{}
    foreach ($f in Get-ChildItem -LiteralPath $rootFull -Recurse -File) {
        $rel = $f.FullName.Substring($rootFull.Length + 1).Replace('\', '/')
        if ($rel -notlike '*/*' -or $rel -ieq 'bin/transom.exe' -or $rel -like 'licenses/*') { continue }
        $wanted["$MsysEnv/$rel".ToLowerInvariant()] = $rel
    }
    $owned = [System.Collections.Generic.HashSet[string]]::new()
    $licensePrefix = "$MsysEnv/share/licenses/"
    $packages = foreach ($pkg in Get-ChildItem -LiteralPath $db -Directory | Sort-Object Name) {
        $filesPath = Join-Path $pkg.FullName 'files'
        $descPath = Join-Path $pkg.FullName 'desc'
        if (-not (Test-Path $filesPath) -or -not (Test-Path $descPath)) { continue }
        $bundled = [System.Collections.Generic.List[string]]::new()
        $data = 0
        $licenseFiles = [System.Collections.Generic.List[string]]::new()
        foreach ($line in [System.IO.File]::ReadLines($filesPath)) {
            $key = $line.ToLowerInvariant()
            if ($wanted.ContainsKey($key)) {
                $null = $owned.Add($key)
                if ($key -match '\.(dll|exe)$') { $bundled.Add($wanted[$key]) } else { $data++ }
            }
            elseif ($line.StartsWith($licensePrefix, [StringComparison]::OrdinalIgnoreCase) -and
                    -not $line.EndsWith('/')) {
                $licenseFiles.Add($line)
            }
        }
        if ($bundled.Count -eq 0 -and $data -eq 0) { continue }

        $desc = @{}
        $field = $null
        foreach ($line in [System.IO.File]::ReadLines($descPath)) {
            if ($line -match '^%([A-Z0-9]+)%$') { $field = $Matches[1]; $desc[$field] = @() }
            elseif ($line -and $field) { $desc[$field] += $line }
        }
        $name = $desc['NAME'][0]
        $pkgVersion = $desc['VERSION'][0]
        $base = if ($desc['BASE']) { $desc['BASE'][0] } else { $name }
        $license = if ($desc['LICENSE']) {
            @($desc['LICENSE'] | ForEach-Object { $_ -replace '^spdx:\s*', '' }) -join '; '
        } else { 'not declared by the package' }

        $copied = foreach ($licenseFile in $licenseFiles) {
            $relative = $licenseFile.Substring($licensePrefix.Length)
            if ($relative -match '(^|/)\.\.?(/|$)') { Fail "MSYS2 package $name lists licence file $licenseFile" }
            $dest = Join-Path (Join-Path $rootFull 'licenses') $relative
            New-Item -ItemType Directory -Force (Split-Path $dest) | Out-Null
            Copy-Item -LiteralPath (Join-Path $Msys2Root $licenseFile) $dest
            "licenses/$relative"
        }
        if ($data -gt 0) { $bundled.Add("$data data file(s)") }
        [pscustomobject]@{
            Name = $name
            Version = $pkgVersion
            License = $license
            Source = "https://repo.msys2.org/mingw/sources/$base-$pkgVersion.src.tar.zst"
            LicenseFiles = @($copied)
            Bundled = @($bundled)
        }
    }
    $unowned = @($wanted.Keys | Where-Object { $_ -match '\.(dll|exe)$' -and -not $owned.Contains($_) } |
        ForEach-Object { $wanted[$_] } | Sort-Object)
    if ($unowned.Count -gt 0) { Fail "no MSYS2 package owns bundled $($unowned -join ', ')" }

    $lines = [System.Collections.Generic.List[string]]::new()
    $lines.Add($Title)
    $lines.Add('=' * $Title.Length)
    $lines.Add('')
    $lines.Add('Every DLL, program, loader, icon and schema in this folder that Transom does not')
    $lines.Add('build itself comes from one of the MSYS2 packages below, as installed when this')
    $lines.Add('package was built. Each entry gives the exact package version, the licence the')
    $lines.Add('package declares, its source package (the upstream source with MSYS2''s build')
    $lines.Add('recipe and patches; https://github.com/msys2/MINGW-packages has the recipes),')
    $lines.Add('the licence files copied into licenses\, and what it provided. See')
    $lines.Add('THIRD-PARTY-NOTICES.txt.')
    foreach ($p in $packages) {
        $lines.Add('')
        $lines.Add("$($p.Name) $($p.Version)")
        $lines.Add("  License: $($p.License)")
        $lines.Add("  Source: $($p.Source)")
        if ($p.LicenseFiles.Count -gt 0) { $lines.Add("  License files: $($p.LicenseFiles -join ', ')") }
        else { $lines.Add('  License files: none shipped by the package; see its source') }
        $lines.Add("  Bundled: $($p.Bundled -join ', ')")
    }
    [System.IO.File]::WriteAllLines((Join-Path $rootFull 'BUNDLED-PACKAGES.txt'), $lines,
        [System.Text.UTF8Encoding]::new($false))
    return @($packages).Count
}
$version = (Select-String -Path (Join-Path $repo 'Cargo.toml') -Pattern '^version\s*=\s*"([^"]+)"' |
    Select-Object -First 1).Matches[0].Groups[1].Value
$count = Write-BundledPackages $dist $Msys2Root $msysEnv "Transom $version for Windows ($msysEnv): bundled MSYS2 packages"
Info "recorded $count MSYS2 packages in BUNDLED-PACKAGES.txt"

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
    # Four-part numeric form for the version resource: "1.2.3-rc1" -> "1.2.3.0".
    $parts = @(($version -split '[-+]')[0] -split '\.')
    while ($parts.Count -lt 4) { $parts += '0' }
    $numeric = ($parts[0..3]) -join '.'
    & $iscc "/DAppVersion=$version" "/DAppNumericVersion=$numeric" "/DSourceDir=$dist" `
        "/DOutputDir=$(Join-Path $repo 'dist')" "/DTargetArch=$innoArch" `
        (Join-Path $repo 'build-aux\inno\transom.iss')
    if ($LASTEXITCODE -ne 0) { Fail 'Inno Setup failed' }
    Info "wrote dist\transom-setup.exe"
}
