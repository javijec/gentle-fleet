#!/usr/bin/env bash
# P4.2-B: herramientas reales de Gentle Shell estándar (bash), sin permisos interactivos.
set -euo pipefail
if [[ "${GF_E2E_REAL:-}" != "1" ]]; then
  echo "SKIP: set GF_E2E_REAL=1 for real Gentle Shell tool smoke"
  exit 0
fi
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
BIN="$ROOT/target/debug"
RUNTIME="$(mktemp -d /tmp/gf-p4-gs-tool-XXXXXX)"
WORKSPACE="$(mktemp -d /tmp/gf-p4-gs-tool-ws-XXXXXX)"
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
SES="$("$BIN/fleetctl" session create --workspace "$WS" --provider gentle-shell | cut -f1)"
timeout 180 "$BIN/fleetctl" watch "$SES" --until-turn-end >"$RUNTIME/watch.out" 2>"$RUNTIME/watch.err" &
WATCH=$!
sleep 1
"$BIN/fleetctl" prompt "$SES" 'Run the shell command `pwd` with the bash tool in the temporary workspace and report its output. Do not only describe it.' >/dev/null
wait "$WATCH"
grep -q 'tool_started' "$RUNTIME/watch.out" || { echo "FAIL: no tool execution"; cat "$RUNTIME/watch.out"; exit 1; }
grep -q 'tool_completed' "$RUNTIME/watch.out" || { echo "FAIL: tool did not complete"; exit 1; }
grep -q $'turn_completed\tCompleted' "$RUNTIME/watch.out" || { echo "FAIL: turn did not settle"; exit 1; }
echo "PASS: real Gentle Shell tool execution streamed through Fleet"
