<#
.SYNOPSIS
    Transom - Windows build and packaging helper (portable .zip, optional Inno Setup
    installer), with the switches of Tributary's and Balun's build-windows.ps1.

.DESCRIPTION
    Builds with MSYS2's CLANG64 / CLANGARM64 GTK stack, like Tributary and Balun. With
    no mode, it checks the build dependencies, builds the release transom.exe with
    Cargo's locked dependency graph, stages dist\transom-windows\ (bin\transom.exe and
    its DLLs, share\, lib\, and the licence files at the top) and writes
    dist\transom-windows.zip.

    Dependencies, in an MSYS2 shell (on arm64: CLANGARM64 and mingw-w64-clang-aarch64-...):
      pacman -S --needed mingw-w64-clang-x86_64-{toolchain,gtk4,libadwaita,freerdp,ffmpeg,adwaita-icon-theme,pkg-config,clang}
    and Rust from rustup (https://rustup.rs) with the gnullvm target:
      rustup target add x86_64-pc-windows-gnullvm   (aarch64-pc-windows-gnullvm on arm64)
    Build scripts run on the host: either Visual Studio's C++ build tools (the default
    MSVC host, as on GitHub's runners) or the gnullvm host toolchain
    (rustup toolchain install stable-x86_64-pc-windows-gnullvm, then
    $env:RUSTUP_TOOLCHAIN = 'stable-x86_64-pc-windows-gnullvm').

    The helper installs nothing: a missing package or tool stops it before any build
    work, with the command that installs it. Cargo may fetch the locked crates.

    Environment: RUST_TARGET (default from the host architecture), MSYS_ENV (clang64 or
    clangarm64), MSYS2_ROOT (default C:\msys64), TRANSOM_ISCC (the full path of
    ISCC.exe, when Inno Setup 6 isn't in a standard place).

    The bundle keeps MSYS2's prefix layout, so GLib/GTK find share\ and lib\ from their
    own DLL location; nothing sets environment variables at run time. DLLs are found
    from PE import tables with llvm-readobj (not ldd, which runs the binaries).

.PARAMETER Msys2Root
    The MSYS2 installation. Defaults to MSYS2_ROOT, else C:\msys64.

.PARAMETER Fmt
    Run cargo fmt on the workspace and exit. Needs only Cargo, not MSYS2.

.PARAMETER Check
    Check every workspace target for the Windows target and exit.

.PARAMETER Clippy
    Clippy with warnings denied on every workspace target, in the debug and then the
    release profile. Exits afterwards.

.PARAMETER Test
    Run the workspace tests for the Windows target and exit.

.PARAMETER Coverage
    Print a workspace coverage summary and exit. Needs cargo-llvm-cov
    (cargo install --locked cargo-llvm-cov --version 0.8.7).

.PARAMETER Run
    Build the release transom.exe, then run it from target\ with MSYS2's DLLs, showing
    its log in this terminal until it exits. Cannot be combined with another mode.

.PARAMETER InnoSetup
    After the zip, compile dist\transom-setup.exe with Inno Setup 6 (choco install
    innosetup, or TRANSOM_ISCC). With SkipBundle, compile it from the existing
    dist\transom-windows\ instead.

.PARAMETER SkipBundle
    Alone: build transom.exe without staging, zipping or an installer. With InnoSetup:
    skip the build and staging and compile the installer from the existing
    dist\transom-windows\ (from an earlier full run).

.PARAMETER NoCargoBuild
    Bundle the transom.exe already built for the target (CI builds it in an earlier
    step).

.PARAMETER Help
    Show this help and exit. --help and -h work too.

.EXAMPLE
    pwsh scripts/build-windows.ps1 -InnoSetup

.EXAMPLE
    pwsh scripts/build-windows.ps1 -Clippy
#>
# SPDX-License-Identifier: GPL-3.0-or-later
# (Below the help: Get-Help takes the script's first comment block as its help.)
[CmdletBinding(PositionalBinding = $false)]
param(
    [string]$Msys2Root = $(if ($env:MSYS2_ROOT) { $env:MSYS2_ROOT } else { 'C:\msys64' }),
    [switch]$Fmt,
    [switch]$Check,
    [switch]$Clippy,
    [switch]$Test,
    [switch]$Coverage,
    [switch]$Run,
    [switch]$InnoSetup,
    [switch]$SkipBundle,
    [switch]$NoCargoBuild,
    [switch]$Help,
    [Parameter(ValueFromRemainingArguments = $true)]
    [string[]]$Rest
)
$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
$repo = Split-Path -Parent $PSScriptRoot
Set-Location $repo

function Info($m) { Write-Host "==> $m" }
function Fail($m) { $host.UI.WriteErrorLine("build-windows: $m"); exit 1 }
function Usage-Error($m) {
    $host.UI.WriteErrorLine("build-windows: $m (-Help lists the modes)")
    exit 2
}

# ---- modes -----------------------------------------------------------------------
if ($Help -or ($Rest -contains '--help') -or ($Rest -contains '-h')) {
    Get-Help -Full $PSCommandPath
    exit 0
}
if ($Rest) { Usage-Error "unknown argument: $($Rest -join ' ')" }
$quick = [int][bool]$Fmt + [int][bool]$Check + [int][bool]$Clippy + [int][bool]$Test + [int][bool]$Coverage
$packaging = [int][bool]$InnoSetup + [int][bool]$SkipBundle + [int][bool]$NoCargoBuild
if ($quick -gt 1) { Usage-Error 'choose at most one of -Fmt, -Check, -Clippy, -Test and -Coverage' }
if ($quick -and $packaging) {
    Usage-Error 'a quick-exit mode can''t be combined with -InnoSetup, -SkipBundle or -NoCargoBuild'
}
if ($Run -and ($quick + $packaging)) { Usage-Error '-Run can''t be combined with other modes' }
if ($SkipBundle -and $NoCargoBuild -and -not $InnoSetup) {
    Usage-Error '-SkipBundle with -NoCargoBuild leaves nothing to do'
}

if ($Fmt) {
    if (-not (Get-Command cargo -ErrorAction SilentlyContinue)) {
        Fail 'cargo not found: install Rust with rustup, https://rustup.rs'
    }
    $ErrorActionPreference = 'Continue'   # PowerShell 5.1: redirected stderr is an error
    cargo fmt --version *> $null
    if ($LASTEXITCODE -ne 0) { Fail 'cargo fmt not found: rustup component add rustfmt' }
    Info 'cargo fmt --all'
    cargo fmt --all
    exit $LASTEXITCODE
}

# What this run does: compile (Rust, the libraries, a host linker), stage the bundle
# (MSYS2's tools and data), compile the installer.
$installerOnly = $SkipBundle -and $InnoSetup
$compile = -not $NoCargoBuild -and -not $installerOnly
$bundle = -not $SkipBundle -and -not $Run -and -not $quick

# ---- toolchain -------------------------------------------------------------------
$arm = [System.Runtime.InteropServices.RuntimeInformation]::OSArchitecture -eq 'Arm64'
$msysEnv = if ($env:MSYS_ENV) { $env:MSYS_ENV } elseif ($arm) { 'clangarm64' } else { 'clang64' }
$pkgPrefix = switch ($msysEnv) {
    'clang64' { 'mingw-w64-clang-x86_64' }
    'clangarm64' { 'mingw-w64-clang-aarch64' }
    default { Fail "MSYS_ENV=${msysEnv}: Transom builds with MSYS2's clang64 or clangarm64" }
}
$target = if ($env:RUST_TARGET) { $env:RUST_TARGET } elseif ($msysEnv -eq 'clangarm64') {
    'aarch64-pc-windows-gnullvm' } else { 'x86_64-pc-windows-gnullvm' }
$innoArch = if ($target -like 'aarch64-*') { 'arm64' } else { 'x64' }
$prefix = Join-Path $Msys2Root $msysEnv
$prefixBin = Join-Path $prefix 'bin'
$pkgConfig = Join-Path $prefixBin 'pkg-config.exe'
$dist = Join-Path $repo 'dist\transom-windows'
$exe = Join-Path $repo "target\$target\release\transom.exe"
$version = (Select-String -Path (Join-Path $repo 'Cargo.toml') -Pattern '^version\s*=\s*"([^"]+)"' |
    Select-Object -First 1).Matches[0].Groups[1].Value
$msrv = (Select-String -Path (Join-Path $repo 'Cargo.toml') -Pattern '^rust-version\s*=\s*"([^"]+)"' |
    Select-Object -First 1).Matches[0].Groups[1].Value

# ---- dependency checks (everything first, then one report) ------------------------
# Windows PowerShell 5.1 turns a native command's redirected stderr into errors, which
# 'Stop' makes fatal: the probes below write to stderr when something is missing.
$ErrorActionPreference = 'Continue'
$problems = [System.Collections.Generic.List[string]]::new()
$packages = [System.Collections.Generic.SortedSet[string]]::new()   # MSYS2 packages to install
function Need-Package([string]$what, [string]$package) {
    $problems.Add("$what; from $pkgPrefix-$package")
    $null = $packages.Add("$pkgPrefix-$package")
}
$compiles = $compile -or $quick -or $Run

if ($compiles) {
    if (-not (Get-Command cargo -ErrorAction SilentlyContinue) -or
        -not (Get-Command rustc -ErrorAction SilentlyContinue)) {
        $problems.Add('Rust (cargo and rustc): install rustup, https://rustup.rs (winget install Rustlang.Rustup)')
    } else {
        $rustc = (rustc --version) -split ' '
        if ([version]($rustc[1] -replace '-.*', '') -lt [version]$msrv) {
            $problems.Add("Rust $msrv or newer (rustc is $($rustc[1])): rustup update")
        }
        $libdir = rustc --print target-libdir --target $target 2>$null
        if ($LASTEXITCODE -ne 0 -or -not $libdir -or -not (Test-Path $libdir)) {
            $problems.Add("the Rust standard library for ${target}: rustup target add $target")
        }
        # Build scripts and proc macros are linked for the host. An MSVC host needs
        # Visual Studio's linker (a gnullvm host uses MSYS2's clang, checked below).
        $hostTriple = ((rustc -vV) | Select-String '^host: (\S+)').Matches[0].Groups[1].Value
        if ($hostTriple -like '*-msvc' -and -not $env:VCINSTALLDIR) {
            $vswhere = Join-Path ${env:ProgramFiles(x86)} 'Microsoft Visual Studio\Installer\vswhere.exe'
            $vs = if (Test-Path $vswhere) {
                & $vswhere -latest -products * -requiresAny -property installationPath -requires `
                    Microsoft.VisualStudio.Component.VC.Tools.x86.x64 Microsoft.VisualStudio.Component.VC.Tools.ARM64
            }
            if (-not $vs) {
                $hostArch = $hostTriple.Split('-')[0]
                $problems.Add("a linker for Rust's $hostTriple host: Visual Studio's C++ build tools " +
                    "(the 'Desktop development with C++' workload), or the gnullvm host toolchain: " +
                    "rustup toolchain install stable-$hostArch-pc-windows-gnullvm, then " +
                    "`$env:RUSTUP_TOOLCHAIN = 'stable-$hostArch-pc-windows-gnullvm'")
            }
        }
    }
    if ($Clippy) {
        cargo clippy --version *> $null
        if ($LASTEXITCODE -ne 0) { $problems.Add('cargo clippy: rustup component add clippy') }
    }
    if ($Coverage) {
        if (-not (Get-Command cargo-llvm-cov -ErrorAction SilentlyContinue)) {
            $problems.Add('cargo-llvm-cov: cargo install --locked cargo-llvm-cov --version 0.8.7')
        }
        # The LLVM tools it runs: the llvm-tools rustup component (where cargo-llvm-cov
        # looks) unless LLVM_COV and LLVM_PROFDATA name them. CARGO_LLVM_COV_SETUP=no
        # below keeps it from installing the component itself.
        if (-not ($env:LLVM_COV -and $env:LLVM_PROFDATA) -and (Get-Command rustc -ErrorAction SilentlyContinue)) {
            $sysroot = rustc --print sysroot
            $hostTriple = ((rustc -vV) | Select-String '^host: (\S+)').Matches[0].Groups[1].Value
            if (-not (Test-Path (Join-Path $sysroot "lib\rustlib\$hostTriple\bin\llvm-cov.exe"))) {
                $problems.Add('LLVM''s coverage tools: rustup component add llvm-tools-preview (or set LLVM_COV and LLVM_PROFDATA)')
            }
        }
    }
}

if ($installerOnly) {
    # Only Inno Setup and the existing bundle are needed.
} elseif (-not (Test-Path (Join-Path $Msys2Root 'usr\bin\bash.exe'))) {
    $problems.Add("MSYS2 at ${Msys2Root}: install it from https://www.msys2.org, or pass -Msys2Root (or set MSYS2_ROOT)")
} elseif (-not (Test-Path $prefix)) {
    $problems.Add("MSYS2's $msysEnv environment ($prefix): reinstall or update MSYS2")
} else {
    if (-not (Test-Path $pkgConfig)) {
        Need-Package "pkg-config in $prefixBin" 'pkg-config'
    } else {
        # The libraries at Transom's minimum versions, each with its MSYS2 package.
        $env:PKG_CONFIG_PATH = Join-Path $prefix 'lib\pkgconfig'
        foreach ($m in @(
                @{ mod = 'gtk4'; min = '4.16'; pkg = 'gtk4' },
                @{ mod = 'libadwaita-1'; min = '1.6'; pkg = 'libadwaita' },
                @{ mod = 'freerdp3'; min = '3.15'; pkg = 'freerdp' },
                @{ mod = 'freerdp-client3'; min = '3.15'; pkg = 'freerdp' },
                @{ mod = 'winpr3'; min = '3.15'; pkg = 'freerdp' })) {
            $have = & $pkgConfig --modversion $m.mod 2>$null
            if ($LASTEXITCODE -ne 0) {
                Need-Package "$($m.mod) >= $($m.min) (no $($m.mod).pc in $prefix\lib\pkgconfig)" $m.pkg
            } else {
                & $pkgConfig "--atleast-version=$($m.min)" $m.mod
                if ($LASTEXITCODE -ne 0) {
                    $problems.Add("$($m.mod) >= $($m.min) (found $have): update MSYS2, pacman -Syu")
                }
            }
        }
    }
    if ($compiles) {
        # bindgen loads libclang; the gnullvm targets link with clang.
        if (-not (Test-Path (Join-Path $prefixBin 'libclang.dll'))) {
            Need-Package "libclang.dll in $prefixBin, for bindgen" 'clang'
        }
        if (-not (Test-Path (Join-Path $prefixBin 'clang.exe'))) { Need-Package "clang.exe in $prefixBin" 'clang' }
    }
    if ($bundle) {
        if (-not (Test-Path (Join-Path $prefixBin 'llvm-readobj.exe'))) {
            Need-Package "llvm-readobj.exe in $prefixBin" 'toolchain'
        }
        foreach ($tool in 'glib-compile-schemas.exe', 'gdbus.exe') {
            if (-not (Test-Path (Join-Path $prefixBin $tool))) { Need-Package "$tool in $prefixBin" 'glib2' }
        }
        if (-not (Test-Path (Join-Path $prefix 'share\icons\Adwaita\index.theme'))) {
            Need-Package 'the Adwaita icon theme' 'adwaita-icon-theme'
        }
    }
}

$iscc = $null
if ($InnoSetup) {
    $iscc = @($env:TRANSOM_ISCC, "${env:ProgramFiles(x86)}\Inno Setup 6\ISCC.exe",
        "${env:ProgramFiles}\Inno Setup 6\ISCC.exe",
        "${env:LOCALAPPDATA}\Programs\Inno Setup 6\ISCC.exe") |
        Where-Object { $_ -and (Test-Path $_) } | Select-Object -First 1
    if (-not $iscc) {
        $problems.Add('Inno Setup 6 (ISCC.exe): choco install innosetup, or set TRANSOM_ISCC to its full path')
    }
}

$ErrorActionPreference = 'Stop'
if ($problems.Count -gt 0) {
    $host.UI.WriteErrorLine('Missing build dependencies:')
    foreach ($p in $problems) { $host.UI.WriteErrorLine("  - $p") }
    if ($packages.Count -gt 0) {
        $host.UI.WriteErrorLine('')
        $host.UI.WriteErrorLine("In an MSYS2 shell, install the $msysEnv packages:")
        $host.UI.WriteErrorLine("  pacman -S --needed $($packages -join ' ')")
    }
    exit 1
}

$env:PATH = "$prefixBin;$env:PATH"
# MSYS2's own pkg-config (pkgconf), whatever else is on PATH.
$env:PKG_CONFIG = $pkgConfig
$env:PKG_CONFIG_ALLOW_CROSS = '1'
$env:PKG_CONFIG_PATH = Join-Path $prefix 'lib\pkgconfig'
$env:LIBCLANG_PATH = $prefixBin   # bindgen (FreeRDP bindings)

function Invoke-InnoSetup {
    # Four-part numeric form for the version resource: "1.2.3-rc1" -> "1.2.3.0".
    $parts = @(($version -split '[-+]')[0] -split '\.')
    while ($parts.Count -lt 4) { $parts += '0' }
    $numeric = ($parts[0..3]) -join '.'
    Info 'Inno Setup -> dist\transom-setup.exe'
    & $iscc "/DAppVersion=$version" "/DAppNumericVersion=$numeric" "/DSourceDir=$dist" `
        "/DOutputDir=$(Join-Path $repo 'dist')" "/DTargetArch=$innoArch" `
        (Join-Path $repo 'build-aux\inno\transom.iss')
    if ($LASTEXITCODE -ne 0) { Fail 'Inno Setup failed' }
    Info 'wrote dist\transom-setup.exe'
}

# ---- quick-exit modes ------------------------------------------------------------
if ($Check) {
    Info "cargo check --workspace --all-targets --target $target"
    cargo check --workspace --all-targets --locked --target $target
    exit $LASTEXITCODE
}
if ($Clippy) {
    Info "cargo clippy --workspace --all-targets --target $target (debug)"
    cargo clippy --workspace --all-targets --locked --target $target -- -D warnings
    if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
    Info "cargo clippy --workspace --all-targets --release --target $target"
    cargo clippy --workspace --all-targets --release --locked --target $target -- -D warnings
    exit $LASTEXITCODE
}
if ($Test) {
    Info "cargo test --workspace --target $target"
    cargo test --workspace --locked --target $target
    exit $LASTEXITCODE
}
if ($Coverage) {
    Info "cargo llvm-cov --workspace --all-targets --target $target"
    $env:CARGO_LLVM_COV_SETUP = 'no'
    cargo llvm-cov --workspace --all-targets --locked --target $target --summary-only
    exit $LASTEXITCODE
}

# ---- installer from an existing bundle -------------------------------------------
if ($installerOnly) {
    $staged = Join-Path $dist 'bin\transom.exe'
    if (-not (Test-Path $staged) -or -not (Test-Path (Join-Path $dist 'BUNDLED-PACKAGES.txt'))) {
        Fail "no complete $dist to package: run without -SkipBundle first"
    }
    if ((Test-Path $exe) -and (Get-FileHash $exe).Hash -ne (Get-FileHash $staged).Hash) {
        Write-Warning "$staged isn't the last build ($exe); run without -SkipBundle to bundle that one"
    }
    Invoke-InnoSetup
    exit 0
}

# ---- build -----------------------------------------------------------------------
if ($compile) {
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

if ($Run) {
    Info "running $exe"
    # A GUI-subsystem program: PowerShell waits for it, and shows its log, only when
    # its output is piped. Windows PowerShell 5.1 would turn each stderr line into an
    # error under 'Stop'.
    $ErrorActionPreference = 'Continue'
    # (5.1 wraps each stderr line in an error record, and shows a blank one by its type.)
    & $exe 2>&1 | ForEach-Object {
        if ($_ -is [System.Management.Automation.ErrorRecord]) { $_.Exception.Message } else { "$_" }
    }
    exit $LASTEXITCODE
}
if ($SkipBundle) {
    Info "built $exe (bundle skipped)"
    exit 0
}

# ---- stage ---------------------------------------------------------------------
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
$count = Write-BundledPackages $dist $Msys2Root $msysEnv "Transom $version for Windows ($msysEnv): bundled MSYS2 packages"
Info "recorded $count MSYS2 packages in BUNDLED-PACKAGES.txt"

# ---- zip / installer -------------------------------------------------------------
$zip = Join-Path $repo 'dist\transom-windows.zip'
if (Test-Path $zip) { Remove-Item -Force $zip }
Compress-Archive -Path (Join-Path $dist '*') -DestinationPath $zip
Info "wrote $zip"

if ($InnoSetup) { Invoke-InnoSetup }
