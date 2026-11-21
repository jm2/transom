#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-or-later
# Audio playback check against the grd test server, silently: a tone plays into a
# null sink in the remote session; the client (in a container with its own null
# sink) must reproduce it. Needs scripts/test-servers.sh up.
set -euo pipefail
cd "$(dirname "$0")/.."
STATE="${TMPDIR:-/var/tmp}/transom-test"
cargo build -q -p transom-view-freerdp --example spike
fp=$(podman logs transom-grd 2>&1 | sed -n 's/^TLS fingerprint: //p' | tail -1)
# 6 s of 440 Hz in the session, starting once the client is connected.
podman exec -d transom-grd bash -c '
    sleep 6
    python3 -c "
import math, struct, wave
w = wave.open(\"/tmp/tone.wav\", \"wb\"); w.setnchannels(2); w.setsampwidth(2); w.setframerate(48000)
w.writeframes(b\"\".join(struct.pack(\"<hh\", v, v) for v in (int(12000 * math.sin(2 * math.pi * 440 * i / 48000)) for i in range(48000 * 6))))
"
    pid=$(pgrep -f "gnome-shell --headless" | head -1)
    export $(tr "\0" "\n" < /proc/$pid/environ | grep -E "^(XDG_RUNTIME_DIR)=")
    paplay /tmp/tone.wav'
podman run --rm --network host --userns=keep-id \
    -v "$PWD/target/debug:/demo:ro,Z" \
    -v "$PWD/tests/servers/audio/client.sh:/client.sh:ro,Z" \
    -v "$STATE:/state:Z" \
    --entrypoint /client.sh transom-test-grd \
    localhost:13389 transom --password-file /state/grd.pw --trust "$fp" --audio --stay-ms 14000 \
    2>&1 | grep -E "^(connected|stayed|disconnected|recorded)" | tee "$STATE/audio-check.out"
rms=$(sed -n 's/.*RMS \([0-9]*\).*/\1/p' "$STATE/audio-check.out")
if [[ ${rms:-0} -gt 500 ]]; then echo "AUDIO OUT OK (RMS $rms)"; else echo "AUDIO OUT FAILED (RMS ${rms:-none})"; exit 1; fi

# --- Microphone: a tone is the client's microphone; record it inside the session.
python3 -c "
import math, struct, wave
w = wave.open('$STATE/mic-tone.wav', 'wb'); w.setnchannels(2); w.setsampwidth(2); w.setframerate(48000)
w.writeframes(b''.join(struct.pack('<hh', v, v) for v in (int(12000 * math.sin(2 * math.pi * 660 * i / 48000)) for i in range(48000 * 3))))
"
podman exec -d transom-grd bash -c '
    sleep 7
    pid=$(pgrep -f "gnome-shell --headless" | head -1)
    export $(tr "\0" "\n" < /proc/$pid/environ | grep -E "^(XDG_RUNTIME_DIR)=")
    pactl list short sources > /tmp/mic-sources.txt
    src=$(pactl list short sources | grep -v monitor | grep -i -E "remote|grd|rdp" | head -1 | cut -f2)
    echo "source: ${src:-none}" >> /tmp/mic-sources.txt
    timeout 6 parec ${src:+-d "$src"} --raw --format=s16le --rate=48000 --channels=2 > /tmp/mic-rec.raw'
podman run --rm --network host --userns=keep-id -e MIC_TONE=/state/mic-tone.wav \
    -v "$PWD/target/debug:/demo:ro,Z" \
    -v "$PWD/tests/servers/audio/client.sh:/client.sh:ro,Z" \
    -v "$STATE:/state:Z" \
    --entrypoint /client.sh transom-test-grd \
    localhost:13389 transom --password-file /state/grd.pw --trust "$fp" --mic --stay-ms 16000 \
    2>&1 | grep -E "^(connected|stayed|disconnected)" || true
podman exec transom-grd cat /tmp/mic-sources.txt
podman cp transom-grd:/tmp/mic-rec.raw "$STATE/mic-rec.raw"
mrms=$(python3 -c "
import array, math
a = array.array('h', open('$STATE/mic-rec.raw', 'rb').read())
print(int(math.sqrt(sum(x * x for x in a) / max(1, len(a)))))")
if [[ $mrms -gt 500 ]]; then echo "MICROPHONE OK (RMS $mrms)"; else echo "MICROPHONE FAILED (RMS $mrms)"; exit 1; fi
