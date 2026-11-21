#!/usr/bin/env bash
# P4.2-G: Gentle Shell y Codex conviven; fallar Gentle Shell no detiene Codex.
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
BIN="$ROOT/target/debug"
RUNTIME="$(mktemp -d /tmp/gf-p4-gs-conc-XXXXXX)"
WORKSPACE="$(mktemp -d /tmp/gf-p4-gs-conc-ws-XXXXXX)"
cleanup() {
  [[ -n "${DAEMON_PID:-}" ]] && kill "$DAEMON_PID" 2>/dev/null || true
  if [[ -z "${GF_KEEP:-}" ]]; then rm -rf "$RUNTIME" "$WORKSPACE"; fi
  pkill -f "fleet-runner .* --workspace $WORKSPACE" 2>/dev/null || true
}
trap cleanup EXIT
export FLEET_RUNTIME_DIR="$RUNTIME" FLEET_DB_PATH="$RUNTIME/fleet.sqlite"
"$BIN/fleetd" >"$RUNTIME/daemon.out" 2>"$RUNTIME/daemon.err" &
DAEMON_PID=$!
for _ in $(seq 1 50); do [[ -f "$RUNTIME/fleetd.addr" ]] && break; sleep 0.1; done
WS="$("$BIN/fleetctl" workspace add "$WORKSPACE" | cut -f1)"
GS="$("$BIN/fleetctl" session create --workspace "$WS" --provider gentle-shell | cut -f1)"
CX="$("$BIN/fleetctl" session create --workspace "$WS" --provider codex | cut -f1)"
"$BIN/fleetctl" session list >"$RUNTIME/sessions.out"
grep -q "$GS" "$RUNTIME/sessions.out" || { echo "FAIL: Gentle Shell session missing"; exit 1; }
grep -q "$CX" "$RUNTIME/sessions.out" || { echo "FAIL: Codex session missing"; exit 1; }
GS_RUNNER="$(pgrep -f "fleet-runner --runner-id runner-$GS" | head -1)"
[[ -n "$GS_RUNNER" ]] || { echo "FAIL: no Gentle Shell runner"; exit 1; }
kill -9 "$GS_RUNNER"
sleep 1
"$BIN/fleetctl" session list >"$RUNTIME/after.out"
grep -q "$CX" "$RUNTIME/after.out" || { echo "FAIL: Codex session vanished after Gentle Shell failure"; exit 1; }
CX_RUNNER="$(pgrep -f "fleet-runner --runner-id runner-$CX" | head -1)"
[[ -n "$CX_RUNNER" ]] && kill -0 "$CX_RUNNER" || { echo "FAIL: Codex runner died with Gentle Shell"; exit 1; }
echo "PASS: Gentle Shell and Codex isolated; Gentle Shell runner failure did not stop Codex"
