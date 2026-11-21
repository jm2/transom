#!/usr/bin/env bash
# Runs inside the test image: a private PipeWire with a null sink, the spike with
# --audio, and a recorder on the null sink's monitor. Prints the recording's RMS.
set -euo pipefail
export XDG_RUNTIME_DIR=/tmp/xdg-runtime
mkdir -p -m 700 "$XDG_RUNTIME_DIR"
if [[ "${1:-}" != "--inner" ]]; then exec dbus-run-session -- "$0" --inner "$@"; fi
shift
pipewire & sleep 0.5; wireplumber & sleep 0.5; pipewire-pulse & sleep 1
pactl load-module module-null-sink sink_name=client_null >/dev/null
pactl set-default-sink client_null
parec -d client_null.monitor --raw --format=s16le --rate=48000 --channels=2 > /state/audio-rec.raw &
rec=$!
if [[ -n "${MIC_TONE:-}" ]]; then
    # Microphone test: the null sink's monitor is the "microphone"; keep a tone
    # playing into it.
    pactl set-default-source client_null.monitor
    ( sleep 3; for _ in 1 2 3 4; do paplay "$MIC_TONE"; done ) &
fi
/demo/examples/spike "$@" 2>&1 | grep -E "^(connected|stayed|disconnected)" || true
kill $rec; sleep 0.3
python3 - <<'PY'
import array, math
a = array.array("h", open("/state/audio-rec.raw", "rb").read())
rms = math.sqrt(sum(x * x for x in a) / max(1, len(a)))
print(f"recorded {len(a) / 96000:.1f} s, RMS {rms:.0f}")
PY
