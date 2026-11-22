# SPDX-License-Identifier: GPL-3.0-or-later
# Build Transom for Windows inside the Windows test VM, the way CI does (MSYS2 CLANG64
# + rustup gnullvm + scripts/build-windows.ps1). Run as SYSTEM via the guest agent
# (scripts/windows-build-check.sh); progress in C:\transom\build\log.txt, result in
# C:\transom\build\result.txt. Idempotent: reuses an installed MSYS2/rustup.
$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'
$root = 'C:\transom\build'
New-Item -ItemType Directory -Force $root | Out-Null
$log = "$root\log.txt"
function Log($m) { "$(Get-Date -Format s) $m" | Add-Content $log }
# Native tools write progress to stderr, which Windows PowerShell 5.1 turns into
# terminating errors under 'Stop': run them with 'Continue' and check the exit code.
function Native([scriptblock]$cmd, [string]$what) {
    $ErrorActionPreference = 'Continue'
    & $cmd *>> $log
    if ($LASTEXITCODE -ne 0) { throw "$what failed ($LASTEXITCODE)" }
}
Remove-Item -Force -ErrorAction SilentlyContinue "$root\result.txt"
try {
    $msys = 'C:\msys64'
    if (-not (Test-Path "$msys\usr\bin\bash.exe")) {
        Log 'installing MSYS2'
        $inst = "$root\msys2.exe"
        Invoke-WebRequest 'https://github.com/msys2/msys2-installer/releases/download/nightly-x86_64/msys2-base-x86_64-latest.sfx.exe' -OutFile $inst
        & $inst -y -oC:\ | Out-Null
    }
    $bash = "$msys\usr\bin\bash.exe"
    Log 'pacman update + packages'
    # A fresh MSYS2 updates its core first and may need a second -Syu.
    Native { & $bash -lc 'pacman -Syu --noconfirm --noprogressbar' } 'pacman -Syu'
    Native { & $bash -lc 'pacman -Syu --noconfirm --noprogressbar' } 'pacman -Syu (2)'
    Native { & $bash -lc 'pacman -S --needed --noconfirm --noprogressbar mingw-w64-clang-x86_64-{toolchain,gtk4,libadwaita,freerdp,ffmpeg,adwaita-icon-theme,pkg-config,clang}' } 'pacman -S'

    $cargo = "$env:USERPROFILE\.cargo\bin\cargo.exe"
    if (-not (Test-Path $cargo)) {
        Log 'installing rustup'
        Invoke-WebRequest 'https://static.rust-lang.org/rustup/dist/x86_64-pc-windows-msvc/rustup-init.exe' -OutFile "$root\rustup-init.exe"
        Native { & "$root\rustup-init.exe" -y --profile minimal --default-host x86_64-pc-windows-msvc } 'rustup-init'
    }
    $env:PATH = "$env:USERPROFILE\.cargo\bin;$env:PATH"
    Native { & rustup target add x86_64-pc-windows-gnullvm } 'rustup target'

    Log 'unpacking source'
    $src = "$root\src"
    Remove-Item -Recurse -Force -ErrorAction SilentlyContinue $src
    New-Item -ItemType Directory -Force $src | Out-Null
    Native { tar -xzf "$root\transom.tar.gz" -C $src --strip-components 1 } 'tar'
    Set-Location $src
    Log 'build-windows.ps1'
    $env:RUST_TARGET = 'x86_64-pc-windows-gnullvm'
    $env:MSYS_ENV = 'clang64'
    Native { & powershell -NoProfile -ExecutionPolicy Bypass -File scripts\build-windows.ps1 -Msys2Root $msys } 'build-windows.ps1'
    $zip = Get-Item "$src\dist\transom-windows.zip"
    "OK $($zip.Length) $((Get-ChildItem "$src\dist\transom-windows\bin" -Filter *.dll).Count)" | Set-Content "$root\result.txt"
    Log 'done'
} catch {
    Log "FAILED: $_"
    "FAILED: $_" | Set-Content "$root\result.txt"
}
