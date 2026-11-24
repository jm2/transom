# SPDX-License-Identifier: GPL-3.0-or-later
# Windows half of scripts/clipboard-check.sh (sourced; scripts/lib/target.sh has set
# T_*). clip-probe.ps1 runs one action per call in the rdp user's session.
win() { scripts/winvm.sh "$1" "$TARGET" "${@:2}"; }
clip() { # ACTION → prints the probe's JSON result
    win exec "Remove-Item -Force -ErrorAction SilentlyContinue C:\\transom\\clip-out.json; exit 0"
    win session-run "powershell -NoProfile -ExecutionPolicy Bypass -WindowStyle Hidden -File C:\\transom\\clip-probe.ps1 -Action $1" >/dev/null
    local r
    for _ in $(seq 120); do
        r=$(win exec "Get-Content -Raw -Encoding UTF8 -ErrorAction SilentlyContinue C:\\transom\\clip-out.json" 2>/dev/null) || true
        [[ -n $r ]] && { tr -d '\r' <<<"$r" | sed '/^$/d'; return; }
        sleep 0.5
    done
    echo '{"t":"timeout"}'
}
set_remote_text() { printf '%s' "$1" > "$STATE/clip-in.txt"; win push "$STATE/clip-in.txt" 'C:\transom\clip-in.txt'; }
ok=1
pass() { echo "$1: OK"; }
fail() { echo "$1: FAILED${2:+ ($2)}"; ok=0; }
jget() { # KEY < JSON (CRLF normalised: CF_UNICODETEXT uses CRLF on Windows)
    python3 -I -c 'import json,sys; v=json.loads(sys.stdin.read()).get(sys.argv[1], ""); print(str(v).replace("\r\n", "\n"))' "$1"
}

win push tests/servers/windows/clip-probe.ps1 'C:\transom\clip-probe.ps1'

echo "--- engine path ($TARGET)"
cargo build -q -p transom-view-freerdp --example cliptest
target/debug/examples/cliptest "$T_ADDR" "$T_USER" --password-file "$T_PW" --trust "$T_FP" \
    --seconds 30 > "$STATE/cliptest.out" 2>&1 &
client=$!
sleep 8
r=$(clip paste-text); echo "probe: $r"
[[ $(jget text <<<"$r") == $'hello from transom ✓\nline two' ]] && pass "local -> remote" || fail "local -> remote"
set_remote_text "hello from Windows ✓"
clip copy-text >/dev/null
wait "$client" || true
grep -q 'REMOTE TEXT: "hello from Windows ✓"' "$STATE/cliptest.out" && pass "remote -> local" || fail "remote -> local"

echo "--- widget path ($TARGET: GDK clipboard <-> TransomView bridge <-> RDP)"
cargo build -q -p transom-view --example transom-view-demo
demo_run() { # extra demo args… (background; sets $demo)
    podman run --rm --network host --userns=keep-id \
        -e SCALE=1 -e MONITOR=1600x1000 -e BIN=/demo/examples/transom-view-demo \
        -e RUST_LOG="${RUST_LOG:-}" \
        -e INJECT=/checks/inject-focus.py -e INJECT_AFTER=8 \
        -v "$PWD/tests/servers/view-check:/checks:ro,Z" \
        -v "$PWD/target/debug:/demo:ro,Z" \
        -v "$PWD/tests/servers/view-check/inner.sh:/inner.sh:ro,Z" \
        -v "$STATE:/state:Z" \
        --entrypoint /inner.sh transom-test-grd \
        "$T_ADDR" "$T_USER" --password-file /state/target.pw --trust "$T_FP" \
        --size 1200x800 "$@" &
    demo=$!
}
demo_run --clip-test --quit-after-ms 40000 >"$STATE/widget-clip.log" 2>&1
set_remote_text "remote says hi"
sleep 12   # the demo sets its local clipboard 6 s after connecting (~4 s to connect)
r=$(clip paste-text); echo "probe: $r"
[[ $(jget text <<<"$r") == "from the local side ✓" ]] && pass "widget local -> remote" || fail "widget local -> remote"
clip copy-text >/dev/null
# The demo puts a 40x30 image on its clipboard 18 s after connecting: wait for it, and
# only then copy the probe's image (whichever copies last owns the clipboard).
for _ in $(seq 10); do
    r=$(clip paste-image)
    [[ $(jget w <<<"$r")x$(jget h <<<"$r") == 40x30 ]] && break
    sleep 1
done
echo "probe: $r"
[[ $(jget w <<<"$r")x$(jget h <<<"$r") == 40x30 ]] && pass "widget local -> remote image" || fail "widget local -> remote image"
clip copy-image >/dev/null
wait "$demo" || true
grep -E "^(set local|LOCAL CLIPBOARD)" "$STATE/widget-clip.log" || true
grep -q 'LOCAL CLIPBOARD TEXT: "remote says hi"' "$STATE/widget-clip.log" \
    && pass "widget remote -> local text" || fail "widget remote -> local text"
grep -q 'LOCAL CLIPBOARD IMAGE: 64x48' "$STATE/widget-clip.log" \
    && pass "widget remote -> local image" || fail "widget remote -> local image"

echo "--- files ($TARGET: both directions, contents compared by SHA-256)"
# Client -> Windows: Ctrl+V into an Explorer window, typed through the widget (the
# only way Windows consumes virtual files). Windows -> client: the probe copies files.
clip open-paste-target >/dev/null
podman run --rm --network host --userns=keep-id \
    -e SCALE=1 -e MONITOR=1600x1000 -e BIN=/demo/examples/transom-view-demo \
    -e INJECT=/inject-paste.py -e INJECT_AFTER=14 -e RUST_LOG="${RUST_LOG:-}" \
    -v "$PWD/target/debug:/demo:ro,Z" \
    -v "$PWD/tests/servers/view-check/inner.sh:/inner.sh:ro,Z" \
    -v "$PWD/tests/servers/windows/inject-paste.py:/inject-paste.py:ro,Z" \
    -v "$STATE:/state:Z" \
    --entrypoint /inner.sh transom-test-grd \
    "$T_ADDR" "$T_USER" --password-file /state/target.pw --trust "$T_FP" \
    --size 1600x1000 --file-clip-test --quit-after-ms 45000 >"$STATE/widget-files.log" 2>&1 &
demo=$!
sleep 30   # focus and files offered at ~14 s, Ctrl+V at ~21 s, then the copy
clip hash-pasted > "$STATE/files-pasted.json"
clip copy-files > "$STATE/files-copied.json"
wait "$demo" || true
python3 -I - "$STATE/widget-files.log" "$STATE/files-pasted.json" "$STATE/files-copied.json" <<'PY' || ok=0
import ast, json, sys
demo = open(sys.argv[1]).read().splitlines()
pasted = json.load(open(sys.argv[2])); copied = json.load(open(sys.argv[3]))
def demo_map(prefix):
    found = None
    for l in demo:
        if l.startswith(prefix):
            try:
                found = ast.literal_eval(l[len(prefix):].strip())
            except (ValueError, SyntaxError):
                pass
    return found
ok = True
for name, a, b in [("local -> remote files", demo_map("LOCAL FILES SET:"), pasted.get("sha")),
                   ("remote -> local files", copied.get("sha"), demo_map("LOCAL CLIPBOARD FILES:"))]:
    good = bool(a) and a == b
    ok &= good
    print(f"{name}: {'OK' if good else 'FAILED'} ({len(a or {})} files)")
    if not good:
        print("  want", a, "\n  got ", b)
sys.exit(0 if ok else 1)
PY
[[ $ok == 1 ]]
