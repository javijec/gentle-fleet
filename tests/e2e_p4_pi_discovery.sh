#!/usr/bin/env bash
# P4.2-F: descubrimiento e importación de historial Pi sobre un archivo
# sintético aislado. No toca sesiones personales ni usa modelos.
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
BIN="$ROOT/target/debug"
RUNTIME="$(mktemp -d /tmp/gf-p4-pi-disc-XXXXXX)"
WORKSPACE="$(mktemp -d /tmp/gf-p4-pi-disc-ws-XXXXXX)"
SESSIONS="$RUNTIME/pi-sessions"
mkdir -p "$SESSIONS/--tmp--fixture"
cat >"$SESSIONS/--tmp--fixture/2026-01-01_fixture.jsonl" <<'EOF'
{"type":"session","version":3,"id":"fixture-session","timestamp":"2026-01-01T00:00:00.000Z","cwd":"/tmp/fixture"}
{"type":"message","id":"e1","parentId":null,"timestamp":"2026-01-01T00:00:01.000Z","message":{"role":"user","content":"historical question"}}
{"type":"message","id":"e2","parentId":"e1","timestamp":"2026-01-01T00:00:02.000Z","message":{"role":"assistant","content":[{"type":"text","text":"historical answer"}]}}
{"type":"message","id":"e3","parentId":"e2","timestamp":"2026-01-01T00:00:03.000Z","message":{"role":"toolResult","toolName":"bash","content":"fixture output"}}
EOF
cleanup() {
  [[ -n "${DAEMON_PID:-}" ]] && kill "$DAEMON_PID" 2>/dev/null || true
  if [[ -z "${GF_KEEP:-}" ]]; then rm -rf "$RUNTIME" "$WORKSPACE"; fi
  pkill -f "fleet-runner .* --workspace $WORKSPACE" 2>/dev/null || true
}
trap cleanup EXIT
export FLEET_RUNTIME_DIR="$RUNTIME" FLEET_DB_PATH="$RUNTIME/fleet.sqlite" PI_SESSION_DIR="$SESSIONS"
"$BIN/fleetd" >"$RUNTIME/daemon.out" 2>"$RUNTIME/daemon.err" &
DAEMON_PID=$!
for _ in $(seq 1 50); do [[ -f "$RUNTIME/fleetd.addr" ]] && break; sleep 0.1; done
WS="$("$BIN/fleetctl" workspace add "$WORKSPACE" | cut -f1)"
SES="$("$BIN/fleetctl" session create --workspace "$WS" --provider pi | cut -f1)"
"$BIN/fleetctl" native-sessions pi >"$RUNTIME/native.out"
REF="$(awk -F '\t' '$2 == "/tmp/fixture" {print $1; exit}' "$RUNTIME/native.out")"
[[ "$REF" == pi-* ]] || { echo "FAIL: native session not discovered"; cat "$RUNTIME/native.out"; exit 1; }
! grep -q 'fixture-session' "$RUNTIME/native.out" || { echo "FAIL: native ID leaked"; exit 1; }
"$BIN/fleetctl" import-history "$SES" pi "$REF" op-key-1 >"$RUNTIME/import.out"
grep -q $'^imported\t3$' "$RUNTIME/import.out" || { echo "FAIL: history import count"; cat "$RUNTIME/import.out"; exit 1; }
"$BIN/fleetctl" timeline "$SES" --limit 100 >"$RUNTIME/timeline.out"
grep -q $'history\tuser\timported_from_provider\thistorical question' "$RUNTIME/timeline.out" || { echo "FAIL: user history missing"; exit 1; }
grep -q $'history\ttool\timported_from_provider' "$RUNTIME/timeline.out" || { echo "FAIL: tool history missing"; exit 1; }
BEFORE="$(grep -c $'history\t' "$RUNTIME/timeline.out")"
"$BIN/fleetctl" import-history "$SES" pi "$REF" op-key-1 >"$RUNTIME/import2.out"
"$BIN/fleetctl" import-history "$SES" pi "$REF" op-key-2 >"$RUNTIME/import3.out"
"$BIN/fleetctl" timeline "$SES" --limit 100 >"$RUNTIME/timeline2.out"
[[ "$(grep -c $'history\t' "$RUNTIME/timeline2.out")" == "$BEFORE" ]] || { echo "FAIL: duplicate history"; exit 1; }
echo "PASS: Pi read-only discovery, typed history import and cross-key dedup"
