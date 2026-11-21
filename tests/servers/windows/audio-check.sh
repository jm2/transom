# SPDX-License-Identifier: GPL-3.0-or-later
# Windows half of scripts/audio-check.sh (sourced; scripts/lib/target.sh has set T_*).
# audio-probe.ps1 plays a tone / records the microphone in the rdp user's session.
win() { scripts/winvm.sh "$1" "$TARGET" "${@:2}"; }
probe() { # ACTION SECONDS (starts it in the session; returns at once)
    win exec "Remove-Item -Force -ErrorAction SilentlyContinue C:\\transom\\audio-out.json; exit 0"
    win session-run "powershell -NoProfile -ExecutionPolicy Bypass -WindowStyle Hidden -File C:\\transom\\audio-probe.ps1 -Action $1 -Seconds $2" >/dev/null
}
result() { # waits for the probe's JSON
    local r
    for _ in $(seq 60); do
        r=$(win exec "Get-Content -Raw -Encoding UTF8 -ErrorAction SilentlyContinue C:\\transom\\audio-out.json" 2>/dev/null) || true
        [[ -n $r ]] && { tr -d '\r' <<<"$r" | sed '/^$/d'; return; }
        sleep 0.5
    done
    echo '{"t":"timeout"}'
}
win push tests/servers/windows/audio-probe.ps1 'C:\transom\audio-probe.ps1'
win configure >/dev/null   # Windows Audio service on (Server)
cargo build -q -p transom-view-freerdp --example spike
client() { # extra spike args… (the audio client container, see tests/servers/audio)
    podman run --rm --network host --userns=keep-id ${MIC_TONE:+-e MIC_TONE="$MIC_TONE"} \
        -v "$PWD/target/debug:/demo:ro,Z" \
        -v "$PWD/tests/servers/audio/client.sh:/client.sh:ro,Z" \
        -v "$STATE:/state:Z" \
        --entrypoint /client.sh transom-test-grd \
        "$T_ADDR" "$T_USER" --password-file /state/target.pw --trust "$T_FP" "$@" 2>&1 |
        grep -E "^(connected|stayed|disconnected|recorded)"
}

echo "--- audio out ($TARGET)"
( sleep 7; probe play 6 ) &
client --audio --stay-ms 16000 | tee "$STATE/audio-check.out"
wait
echo "probe: $(result)"
rms=$(sed -n 's/.*RMS \([0-9]*\).*/\1/p' "$STATE/audio-check.out")
if [[ ${rms:-0} -gt 500 ]]; then echo "AUDIO OUT OK (RMS $rms)"; else echo "AUDIO OUT FAILED (RMS ${rms:-none})"; exit 1; fi

echo "--- microphone ($TARGET)"
python3 -c "
import math, struct, wave
w = wave.open('$STATE/mic-tone.wav', 'wb'); w.setnchannels(2); w.setsampwidth(2); w.setframerate(48000)
w.writeframes(b''.join(struct.pack('<hh', v, v) for v in (int(12000 * math.sin(2 * math.pi * 660 * i / 48000)) for i in range(48000 * 3))))
"
( sleep 8; probe record 5 ) &
MIC_TONE=/state/mic-tone.wav client --mic --stay-ms 18000 || true
wait
r=$(result); echo "probe: $r"
mrms=$(python3 -I -c 'import json,sys; print(json.loads(sys.argv[1]).get("rms", 0))' "$r")
if [[ $mrms -gt 500 ]]; then echo "MICROPHONE OK (RMS $mrms)"; else echo "MICROPHONE FAILED (RMS $mrms)"; exit 1; fi
