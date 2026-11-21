#!/usr/bin/env bash
set -euo pipefail
if [[ "${GF_E2E_REAL:-}" != "1" ]]; then
  echo "SKIP: set GF_E2E_REAL=1 to run real Codex smoke (billable model calls)"
  exit 0
fi
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
BIN="$ROOT/target/debug"
RUNTIME="$(mktemp -d /tmp/p41-codex-rt-XXXXXX)"
WORKSPACE="$(mktemp -d /tmp/p41-codex-ws-XXXXXX)"
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
[[ -f "$RUNTIME/fleetd.addr" ]] || { echo "FAIL: daemon did not start"; cat "$RUNTIME/daemon.err"; exit 1; }
echo '--- agents ---'
"$BIN/fleetctl" agents || true
WS="$("$BIN/fleetctl" workspace add "$WORKSPACE" | cut -f1)"
SES="$("$BIN/fleetctl" session create --workspace "$WS" --provider codex | cut -f1)"
echo "workspace=$WS session=$SES"
timeout 240 "$BIN/fleetctl" watch "$SES" --until-turn-end >"$RUNTIME/watch.out" 2>"$RUNTIME/watch.err" &
WATCH_PID=$!
sleep 1
"$BIN/fleetctl" prompt "$SES" 'Reply with exactly: P4 CODEX OK. Do not use tools.'
wait "$WATCH_PID"
echo 'watch ok'
echo '--- watch stderr ---'; cat "$RUNTIME/watch.err"
echo '--- watch stdout ---'; cat "$RUNTIME/watch.out"
grep -qF $'delta\t' "$RUNTIME/watch.out" || { echo "FAIL: no assistant deltas"; exit 1; }
grep -qF 'turn_completed	Completed' "$RUNTIME/watch.out" || { echo "FAIL: no terminal completion"; exit 1; }
grep -qi 'P4 CODEX OK' "$RUNTIME/watch.out" || { echo "FAIL: assistant response text missing"; exit 1; }
echo '--- timeline ---'; "$BIN/fleetctl" timeline "$SES" --limit 100 || true
echo '--- daemon err tail ---'; tail -20 "$RUNTIME/daemon.err"
echo "RUNTIME=$RUNTIME"
