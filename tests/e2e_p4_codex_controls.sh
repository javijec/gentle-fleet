#!/usr/bin/env bash
set -euo pipefail
if [[ "${GF_E2E_REAL:-}" != "1" ]]; then
  echo "SKIP: set GF_E2E_REAL=1 to run real Codex smoke (billable model calls)"
  exit 0
fi
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
BIN="$ROOT/target/debug"
RUNTIME="$(mktemp -d /tmp/p41-codex-ctl-XXXXXX)"
WORKSPACE="$(mktemp -d /tmp/p41-codex-ctlws-XXXXXX)"
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
echo '--- agents controls ---'
"$BIN/fleetctl" agents | cut -c1-200
WS="$("$BIN/fleetctl" workspace add "$WORKSPACE" | cut -f1)"
SES="$("$BIN/fleetctl" session create --workspace "$WS" --provider codex | cut -f1)"
echo "session=$SES"
echo '--- controls ---'
"$BIN/fleetctl" controls "$SES" | head -5
echo '--- set-control effort high ---'
"$BIN/fleetctl" set-control "$SES" effort high turn >"$RUNTIME/set-control.out"
grep -qF 
echo '--- invalid value rejected ---'
if "$BIN/fleetctl" set-control "$SES" effort nonsensical turn >"$RUNTIME/invalid.out" 2>&1; then
  echo "FAIL: invalid effort was accepted"; exit 1
fi
timeout 120 "$BIN/fleetctl" watch "$SES" --until-turn-end >"$RUNTIME/watch.out" 2>"$RUNTIME/watch.err" &
WATCH_PID=$!
sleep 1
"$BIN/fleetctl" prompt "$SES" 'Reply with exactly: CONTROL OK.' >/dev/null
wait "$WATCH_PID"
grep -qF 'turn_completed' "$RUNTIME/watch.out" || { echo "FAIL: no completed turn"; exit 1; }
echo '--- watch stdout ---'; cat "$RUNTIME/watch.out"
echo '--- daemon err tail ---'; tail -10 "$RUNTIME/daemon.err"
effort\\thigh\\tdeferred' "$RUNTIME/set-control.out" || { cat "$RUNTIME/set-control.out"; exit 1; }
"$BIN/fleetctl" controls "$SES" >"$RUNTIME/controls-after.out"
grep -qF 
echo '--- invalid value rejected ---'
"$BIN/fleetctl" set-control "$SES" effort nonsensical turn 2>&1 || echo REJECTED_OK
timeout 120 "$BIN/fleetctl" watch "$SES" --until-turn-end >"$RUNTIME/watch.out" 2>"$RUNTIME/watch.err" &
WATCH_PID=$!
sleep 1
"$BIN/fleetctl" prompt "$SES" 'Reply with exactly: CONTROL OK.' >/dev/null || true
wait "$WATCH_PID" || echo "watch exit=$?"
echo '--- watch stdout ---'; cat "$RUNTIME/watch.out"
echo '--- daemon err tail ---'; tail -10 "$RUNTIME/daemon.err"
effort\\thigh\\t' "$RUNTIME/controls-after.out" || { echo "FAIL: selected control not visible"; exit 1; }
echo '--- invalid value rejected ---'
"$BIN/fleetctl" set-control "$SES" effort nonsensical turn 2>&1 || echo REJECTED_OK
timeout 120 "$BIN/fleetctl" watch "$SES" --until-turn-end >"$RUNTIME/watch.out" 2>"$RUNTIME/watch.err" &
WATCH_PID=$!
sleep 1
"$BIN/fleetctl" prompt "$SES" 'Reply with exactly: CONTROL OK.' >/dev/null || true
wait "$WATCH_PID" || echo "watch exit=$?"
echo '--- watch stdout ---'; cat "$RUNTIME/watch.out"
echo '--- daemon err tail ---'; tail -10 "$RUNTIME/daemon.err"
