#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-or-later
# Throwaway Windows test VMs (plain QEMU, no libvirt, no root): unattended install from
# the Microsoft evaluation ISOs, random local-admin credentials, RDP forwarded to
# localhost. Uses KVM when /dev/kvm is usable and falls back to (slow, multi-threaded)
# TCG emulation otherwise, so the disk can be installed before SVM/VT-x is enabled.
#
#   scripts/winvm.sh fetch               # download eval ISOs + virtio-win
#   scripts/winvm.sh prep    win11|server # disk, firmware vars, TPM, creds, answer ISO
#   scripts/winvm.sh install win11|server # unattended install; returns when the VM powers off
#   scripts/winvm.sh run     win11|server [--snapshot]  # boot in the background
#   scripts/winvm.sh shot    win11|server FILE.png       # screenshot (progress)
#   scripts/winvm.sh info    win11|server
#   scripts/winvm.sh exec    win11|server 'PowerShell…'  # run in the guest (guest agent)
#   scripts/winvm.sh configure win11|server  # re-apply transom-policy.ps1 (RDP policy)
#   scripts/winvm.sh fingerprint win11|server  # RDP cert SHA-256, read via the guest agent
#   scripts/winvm.sh push NAME LOCAL 'C:\path'   # copy a file into the guest
#   scripts/winvm.sh session-run NAME 'cmdline'  # start in the rdp user's interactive session
#   scripts/winvm.sh stop    win11|server  # ACPI power-off, then kill after 120 s
#   scripts/winvm.sh destroy win11|server  # delete the VM and its credentials
#
# Images and per-VM state live under ${TRANSOM_VM_DIR:-~/.local/share/transom-testvm}
# (durable: an installed VM is reused across sessions). Credentials are 0600 there and
# exist nowhere else; the answer ISO holding them is deleted after the install.
set -euo pipefail
cd "$(dirname "$0")/.."
SRC=$PWD/tests/servers/windows
ROOT=${TRANSOM_VM_DIR:-$HOME/.local/share/transom-testvm}
ISO=$ROOT/iso
OVMF=/usr/share/edk2/ovmf

cmd=${1:-}; name=${2:-}
case "$name" in
win11)  PORT=13392; IMAGE=$ISO/win11-eval.iso;      INDEX=1; ROLE=client; HOST=TRANSOM-W11 ;;
server) PORT=13393; IMAGE=$ISO/server2025-eval.iso; INDEX=4; ROLE=server; HOST=TRANSOM-SRV ;;
"") [[ $cmd == fetch ]] || { sed -n '8,18p' "$0"; exit 2; } ;;
*) echo "unknown VM '$name' (win11 | server)" >&2; exit 2 ;;
esac
# INDEX: install.wim image. Win11 Enterprise eval has one; Server 2025 eval index 4 is
# "Datacenter Evaluation (Desktop Experience)" (RD Gateway + Session Host need the GUI).
VM=$ROOT/$name
USER_NAME=rdp

qmp() { # JSON command → reply (first non-greeting line)
    python3 -I - "$VM/qmp.sock" "$1" <<'PY'
import json, socket, sys
s = socket.socket(socket.AF_UNIX); s.connect(sys.argv[1]); f = s.makefile('rw')
f.readline(); f.write('{"execute":"qmp_capabilities"}\n'); f.flush(); f.readline()
f.write(sys.argv[2] + '\n'); f.flush()
while True:
    line = f.readline()
    if not line or '"event"' not in line:
        print(line.strip()); break
PY
}

guest_ps() { # SCRIPT → run as SYSTEM in the guest via the QEMU guest agent; prints output
    python3 -I - "$VM/qga.sock" "$1" <<'PY'
import base64, json, socket, sys, time
s = socket.socket(socket.AF_UNIX); s.settimeout(5); s.connect(sys.argv[1])
# Resynchronise first (fails fast while Windows is still booting, instead of holding
# the agent's single connection for minutes), then allow long-running commands.
n = int(time.time() * 1000) % (1 << 30)
s.sendall(b'\xff' + json.dumps({"execute": "guest-sync-delimited", "arguments": {"id": n}}).encode() + b'\n')
buf = b""
try:
    while json.dumps({"return": n}).encode() not in buf:
        buf += s.recv(4096)
except (socket.timeout, OSError):
    sys.exit("guest agent not answering")
s.settimeout(600); f = s.makefile('rw')
def call(cmd, **args):
    f.write(json.dumps({"execute": cmd, "arguments": args}) + "\n"); f.flush()
    while True:
        r = json.loads(f.readline())
        if 'error' in r: sys.exit(f"guest agent: {r['error']}")
        if 'return' in r: return r['return']
enc = base64.b64encode(("$ProgressPreference = 'SilentlyContinue'; [Console]::OutputEncoding = [Text.Encoding]::UTF8\n" + sys.argv[2]).encode('utf-16-le')).decode()
pid = call("guest-exec", path="powershell.exe", arg=["-NoProfile", "-NonInteractive",
           "-ExecutionPolicy", "Bypass", "-EncodedCommand", enc], **{"capture-output": True})["pid"]
while not (st := call("guest-exec-status", pid=pid))["exited"]:
    time.sleep(0.5)
sys.stdout.write(base64.b64decode(st.get("out-data", "")).decode("utf-8", "replace"))
sys.stderr.write(base64.b64decode(st.get("err-data", "")).decode("utf-8", "replace"))
sys.exit(st.get("exitcode", 1))
PY
}

guest_push() { # LOCAL REMOTE → copy through the guest agent's file API
    python3 -I - "$VM/qga.sock" "$1" "$2" <<'PY'
import base64, json, socket, sys
s = socket.socket(socket.AF_UNIX); s.settimeout(5); s.connect(sys.argv[1])
import time
n = int(time.time() * 1000) % (1 << 30)
s.sendall(b'\xff' + json.dumps({"execute": "guest-sync-delimited", "arguments": {"id": n}}).encode() + b'\n')
buf = b""
try:
    while json.dumps({"return": n}).encode() not in buf:
        buf += s.recv(4096)
except (socket.timeout, OSError):
    sys.exit("guest agent not answering")
s.settimeout(60); f = s.makefile('rw')
def call(cmd, **args):
    f.write(json.dumps({"execute": cmd, "arguments": args}) + "\n"); f.flush()
    while True:
        r = json.loads(f.readline())
        if 'error' in r: sys.exit(f"guest agent: {r['error']}")
        if 'return' in r: return r['return']
data = open(sys.argv[2], 'rb').read()
h = call("guest-file-open", path=sys.argv[3], mode="wb")
for i in range(0, len(data), 48 * 1024):
    call("guest-file-write", handle=h, **{"buf-b64": base64.b64encode(data[i:i + 48 * 1024]).decode()})
call("guest-file-close", handle=h)
PY
}

running() { [[ -f $VM/qemu.pid ]] && kill -0 "$(cat "$VM/qemu.pid")" 2>/dev/null; }

kvm_ok() { [[ -r /dev/kvm && -w /dev/kvm ]]; }

start_tpm() {
    mkdir -p -m 700 "$VM/tpm"
    rm -f "$VM/swtpm.sock"
    swtpm socket --tpm2 --tpmstate dir="$VM/tpm" --ctrl type=unixio,path="$VM/swtpm.sock" \
        --terminate --daemon --log file="$VM/swtpm.log"
    for _ in $(seq 50); do [[ -S $VM/swtpm.sock ]] && return; sleep 0.1; done
    echo "swtpm did not start (see $VM/swtpm.log)" >&2; exit 1
}

# qemu_args MODE: MODE=install (CDs attached, guest network restricted) or run.
qemu_args() {
    local mode=$1
    if kvm_ok; then
        A=(-accel kvm -cpu host,hv-passthrough,topoext=on -smp 8,sockets=1,cores=4,threads=2)
    else
        A=(-accel tcg,thread=multi,tb-size=2048 -cpu max -smp 8,sockets=1,cores=8)
    fi
    A+=(-name "transom-$name" -machine q35,smm=on -m 8G -rtc base=utc
        -global driver=cfi.pflash01,property=secure,value=on
        -drive "if=pflash,format=qcow2,unit=0,readonly=on,file=$OVMF/OVMF_CODE_4M.secboot.qcow2"
        -drive "if=pflash,format=qcow2,unit=1,file=$VM/vars.qcow2"
        -chardev "socket,id=tpm,path=$VM/swtpm.sock" -tpmdev emulator,id=tpm0,chardev=tpm
        -device tpm-crb,tpmdev=tpm0
        -drive "if=none,id=disk,file=$VM/disk.qcow2,format=qcow2,discard=unmap,cache=writeback"
        -device ide-hd,drive=disk,bus=ide.0,bootindex=0
        -device e1000e,netdev=n0
        -device virtio-serial-pci
        -chardev "socket,id=qga,path=$VM/qga.sock,server=on,wait=off"
        -device virtserialport,chardev=qga,name=org.qemu.guest_agent.0
        -device qemu-xhci -device usb-tablet -vga std
        -chardev "file,id=com1,path=$VM/serial.log" -serial chardev:com1
        -qmp "unix:$VM/qmp.sock,server=on,wait=off"
        -display none -vnc "unix:$VM/vnc.sock"
        -pidfile "$VM/qemu.pid")
    local net="user,id=n0,hostfwd=tcp:127.0.0.1:$PORT-:3389"
    # Server: RD Gateway (HTTPS) on localhost:13443, and its KDC (once it is the test
    # domain's controller, scripts/ad-setup.sh) on localhost:13088 for client kinit.
    [[ $name == server ]] && net+=",hostfwd=tcp:127.0.0.1:13443-:443,hostfwd=tcp:127.0.0.1:13088-:88"
    # Lab network between the VMs (test AD domain): a point-to-point Ethernet link over
    # a unix socket, no root or bridge. The server listens; win11 (re)connects to it.
    # 10.77.0.1 = server (DC, DNS), 10.77.0.2 = win11; set statically by ad-setup.sh.
    if [[ $mode == run ]]; then
        local lab="stream,id=lab,addr.type=unix,addr.path=$ROOT/lab.sock"
        if [[ $name == server ]]; then lab+=",server=on"; rm -f "$ROOT/lab.sock"
        else lab+=",server=off,reconnect-ms=2000"; fi
        A+=(-netdev "$lab" -device "e1000e,netdev=lab,mac=52:54:00:77:00:0$([[ $name == server ]] && echo 1 || echo 2)")
    fi
    if [[ $mode == install ]]; then
        # No internet during setup: no updates or online-account detours, faster.
        net+=",restrict=on"
        A+=(-drive "if=none,id=cd0,media=cdrom,readonly=on,file=$VM/boot.iso"
            -device ide-cd,drive=cd0,bus=ide.1,bootindex=1
            -drive "if=none,id=cd1,media=cdrom,readonly=on,file=$VM/answer.iso"
            -device ide-cd,drive=cd1,bus=ide.2
            -drive "if=none,id=cd2,media=cdrom,readonly=on,file=$ISO/virtio-win.iso"
            -device ide-cd,drive=cd2,bus=ide.3)
    fi
    A+=(-netdev "$net")
}

# Windows' UEFI CD loader waits for "press any key"; with the disk first in the boot
# order and an empty disk, nobody presses it and setup never starts. Patch a reflinked
# copy of the ISO so its El Torito UEFI image is efisys_noprompt.bin (same size, shipped
# on the ISO for exactly this purpose). After setup has written the ESP the disk boots
# first, so the CD never loops.
patch_boot_iso() {
    local tmp lba sectors
    tmp=$(mktemp -d -p "${TMPDIR:-/var/tmp}")
    # The ISO9660 tree on Microsoft media is a stub; the files are in UDF (7z reads it).
    7z e -y -bso0 -bsp0 -o"$tmp" "$IMAGE" efi/microsoft/boot/efisys.bin \
        efi/microsoft/boot/efisys_noprompt.bin
    # "El Torito boot img :   2  UEFI  y   none  0x0000  0x00      1         548"
    # (the catalog's load size is a token 1 sector; the real size is efisys.bin's).
    lba=$(xorriso -indev "$IMAGE" -report_el_torito plain 2>/dev/null |
        awk '/El Torito boot img/ && /UEFI/ {print $NF}')
    sectors=$(( $(stat -c %s "$tmp/efisys.bin") / 2048 ))
    if [[ -z $lba ]] || ! dd if="$IMAGE" bs=2048 skip="$lba" count="$sectors" status=none |
            cmp -s - "$tmp/efisys.bin"; then
        echo "UEFI boot image of $IMAGE is not efisys.bin at LBA ${lba:-?}; not patching" >&2
        exit 1
    fi
    cmp -s <(stat -c %s "$tmp/efisys.bin") <(stat -c %s "$tmp/efisys_noprompt.bin") ||
        { echo "efisys_noprompt.bin size differs" >&2; exit 1; }
    cp --reflink=auto "$IMAGE" "$VM/boot.iso.part"
    dd if="$tmp/efisys_noprompt.bin" of="$VM/boot.iso.part" bs=2048 seek="$lba" conv=notrunc status=none
    mv "$VM/boot.iso.part" "$VM/boot.iso"
    rm -rf "$tmp"
}

new_password() { # 20 alphanumerics with upper, lower and digit (Windows complexity rules)
    local p
    while :; do
        p=$(openssl rand -base64 48 | tr -dc 'A-Za-z0-9' | head -c 20)
        [[ $p =~ [A-Z] && $p =~ [a-z] && $p =~ [0-9] ]] && { echo "$p"; return; }
    done
}

info() {
    local fp=""
    [[ -f $VM/fingerprint ]] && fp=$(cat "$VM/fingerprint")
    cat <<INFO
Windows test VM '$name' ($(running && echo running || echo stopped); $(kvm_ok && echo KVM || echo "TCG, slow"))
  host:          localhost:$PORT$([[ $name == server ]] && echo "   (RD Gateway: localhost:13443)")
  user:          $USER_NAME
  password file: $VM/password
  SHA-256:       ${fp:-(unknown until first connect)}
  screen:        vncviewer $VM/vnc.sock   |   scripts/winvm.sh shot $name FILE.png
INFO
}

case "$cmd" in
fetch)
    mkdir -p "$ISO"
    get() { [[ -f $ISO/$1 ]] || { curl -fL -C - --retry 5 -o "$ISO/$1.part" "$2" && mv "$ISO/$1.part" "$ISO/$1"; }; }
    get win11-eval.iso 'https://go.microsoft.com/fwlink/?linkid=2334167&clcid=0x409&culture=en-us&country=us' &
    get server2025-eval.iso 'https://go.microsoft.com/fwlink/?linkid=2293312&clcid=0x409&culture=en-us&country=us' &
    # Pinned: the "stable" link redirects to plain http.
    get virtio-win-0.1.302.iso 'https://fedorapeople.org/groups/virt/virtio-win/direct-downloads/archive-virtio/virtio-win-0.1.302-1/virtio-win-0.1.302.iso' &
    wait
    ln -sf virtio-win-0.1.302.iso "$ISO/virtio-win.iso"
    ;;
prep)
    [[ -f $IMAGE && -f $ISO/virtio-win.iso ]] || { echo "missing ISOs: run '$0 fetch'" >&2; exit 1; }
    [[ ! -e $VM/disk.qcow2 ]] || { echo "$VM already exists ('$0 destroy $name' first)" >&2; exit 1; }
    mkdir -p -m 700 "$VM"
    (umask 077
     new_password > "$VM/password"
     printf '%s\n%s\n' "$USER_NAME" "$(cat "$VM/password")" > "$VM/creds"
     a=$(mktemp -d -p "${TMPDIR:-/var/tmp}")
     # Python reads the password from its file (sed would need it on its command line).
     python3 -I - "$SRC/autounattend.xml.in" "$VM/password" "$USER_NAME" "$HOST" "$INDEX" "$ROLE" \
         > "$a/autounattend.xml" <<'PY'
import sys
from xml.sax.saxutils import escape
template, password_file, user, host, index, role = sys.argv[1:]
with open(password_file) as f:
    password = f.read().rstrip("\n")
with open(template) as f:
    xml = f.read()
for key, value in (("PASSWORD", password), ("USER", user), ("COMPUTER_NAME", host),
                   ("IMAGE_INDEX", index), ("ROLE", role)):
    xml = xml.replace(f"@{key}@", escape(value))
sys.stdout.write(xml)
PY
     cp "$SRC/transom-setup.ps1" "$SRC/transom-policy.ps1" "$a/"
     xorriso -as mkisofs -quiet -J -r -V TRANSOM_ANSWER -o "$VM/answer.iso" "$a"
     rm -rf "$a")
    patch_boot_iso
    qemu-img create -q -f qcow2 "$VM/disk.qcow2" 80G
    cp "$OVMF/OVMF_VARS_4M.secboot.qcow2" "$VM/vars.qcow2"
    echo "prepared $VM"
    ;;
install)
    [[ -f $VM/answer.iso ]] || { echo "not prepared (or already installed): '$0 prep $name'" >&2; exit 1; }
    running && { echo "already running" >&2; exit 1; }
    start_tpm
    qemu_args install
    echo "installing $name ($(kvm_ok && echo KVM || echo TCG: expect hours)); screenshots: $0 shot $name FILE.png"
    start=$(date +%s)
    qemu-system-x86_64 "${A[@]}"
    echo "VM powered off after $(( ($(date +%s) - start) / 60 )) min"
    # transom-setup.ps1 reports on COM1 just before powering off; anything else (a crash,
    # a setup error) keeps the answer ISO so the install can be inspected and retried.
    if ! grep -q TRANSOM-READY "$VM/serial.log" 2>/dev/null; then
        echo "install did not complete (no TRANSOM-READY on COM1); see '$0 shot'" >&2
        exit 1
    fi
    rm -f "$VM/answer.iso" "$VM/boot.iso"
    touch "$VM/installed"
    echo "answer ISO and patched boot ISO removed"
    ;;
run)
    [[ -f $VM/installed ]] || { echo "not installed: '$0 install $name'" >&2; exit 1; }
    running && { info; exit 0; }
    start_tpm
    qemu_args run
    [[ ${3:-} == --snapshot ]] && A+=(-snapshot)
    qemu-system-x86_64 "${A[@]}" -daemonize
    info
    ;;
shot)
    out=$(realpath -m "${3:?FILE.png}")
    qmp "{\"execute\":\"screendump\",\"arguments\":{\"filename\":\"$out\",\"format\":\"png\"}}" >/dev/null
    echo "$out"
    ;;
info) info ;;
exec) running || { echo "not running" >&2; exit 1; }; guest_ps "${3:?PowerShell command}" ;;
push)
    running || { echo "not running" >&2; exit 1; }
    guest_ps "New-Item -ItemType Directory -Force -Path (Split-Path '${4:?REMOTE}') | Out-Null"
    guest_push "${3:?LOCAL}" "$4"
    ;;
session-run)
    # A scheduled task with an interactive token runs in the user's (RDP) session; the
    # guest agent itself runs as SYSTEM in session 0, where hooks and clipboards differ.
    running || { echo "not running" >&2; exit 1; }
    tr=${3:?cmdline}
    guest_ps "schtasks /Create /F /TN TransomSession /SC ONCE /ST 23:59 /RU $USER_NAME /IT /RL HIGHEST /TR '$tr' | Out-Null
schtasks /Run /TN TransomSession"
    ;;
fingerprint)
    # Read out of band (guest agent, not the RDP connection), so trusting it is not TOFU.
    running || { echo "not running" >&2; exit 1; }
    fp=$(guest_ps '$c = Get-ChildItem "Cert:\LocalMachine\Remote Desktop" | Select-Object -First 1
[BitConverter]::ToString([Security.Cryptography.SHA256]::Create().ComputeHash($c.RawData)).Replace("-", ":")' | tr -d '\r\n')
    [[ $fp =~ ^([0-9A-F]{2}:){31}[0-9A-F]{2}$ ]] || { echo "no RDP certificate yet: '$fp'" >&2; exit 1; }
    echo "$fp" > "$VM/fingerprint"
    echo "$fp"
    ;;
configure)
    running || { echo "not running: '$0 run $name'" >&2; exit 1; }
    guest_ps "$(cat "$SRC/transom-policy.ps1")"
    ;;
stop)
    running || exit 0
    qmp '{"execute":"system_powerdown"}' >/dev/null
    for _ in $(seq 120); do running || exit 0; sleep 1; done
    kill "$(cat "$VM/qemu.pid")"
    ;;
destroy)
    running && { echo "running: '$0 stop $name' first" >&2; exit 1; }
    rm -rf "$VM"
    echo "removed $VM"
    ;;
*) sed -n '8,18p' "$0"; exit 2 ;;
esac
