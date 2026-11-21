#!/usr/bin/env bash
set -uo pipefail
if [[ "${GF_E2E_REAL:-}" != "1" ]]; then
  echo "SKIP: set GF_E2E_REAL=1 to run real Codex smoke (billable model calls)"
  exit 0
fi
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
BIN="$ROOT/target/debug"
RUNTIME="$(mktemp -d /tmp/p41-codex-int-XXXXXX)"
WORKSPACE="$(mktemp -d /tmp/p41-codex-intws-XXXXXX)"
cleanup() {
  [[ -n "${DAEMON_PID:-}" ]] && kill "$DAEMON_PID" 2>/dev/null || true
  pkill -f "fleet-runner .* --workspace $WORKSPACE" 2>/dev/null || true
  if [[ -z "${GF_KEEP:-}" ]]; then rm -rf "$RUNTIME" "$WORKSPACE"; fi
}
trap cleanup EXIT
export FLEET_RUNTIME_DIR="$RUNTIME" FLEET_DB_PATH="$RUNTIME/fleet.sqlite"
"$BIN/fleetd" >"$RUNTIME/daemon.out" 2>"$RUNTIME/daemon.err" &
DAEMON_PID=$!
for _ in $(seq 1 50); do [[ -f "$RUNTIME/fleetd.addr" ]] && break; sleep 0.1; done
WS="$("$BIN/fleetctl" workspace add "$WORKSPACE" | cut -f1)"
SES="$("$BIN/fleetctl" session create --workspace "$WS" --provider codex | cut -f1)"
echo "session=$SES"
timeout 180 "$BIN/fleetctl" watch "$SES" --until-turn-end >"$RUNTIME/watch.out" 2>"$RUNTIME/watch.err" &
WATCH_PID=$!
sleep 1
OUT="$("$BIN/fleetctl" prompt "$SES" 'Run `sleep 30` in a shell and then report that you are done.')"
echo "prompt_out=$OUT"
TURN="$(echo "$OUT" | cut -f3)"
sleep 4
echo '--- interrupt ---'
"$BIN/fleetctl" interrupt "$SES" "$TURN" || echo "interrupt command failed"
wait "$WATCH_PID" || echo "watch exit=$?"
echo '--- watch stdout ---'; cat "$RUNTIME/watch.out"
grep -qF 'turn_completed	Interrupted' "$RUNTIME/watch.out" || { echo "FAIL: interrupt did not produce Interrupted"; exit 1; }
echo '--- watch stderr ---'; cat "$RUNTIME/watch.err"
echo '--- daemon err tail ---'; tail -20 "$RUNTIME/daemon.err"
