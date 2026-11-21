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
grep -q $'^effort\thigh\tdeferred$' "$RUNTIME/set-control.out" || { cat "$RUNTIME/set-control.out"; exit 1; }
"$BIN/fleetctl" controls "$SES" >"$RUNTIME/controls-after.out"
grep -q $'^effort\thigh\t' "$RUNTIME/controls-after.out" || { echo "FAIL: desired effort missing"; exit 1; }
echo '--- invalid value rejected ---'
if "$BIN/fleetctl" set-control "$SES" effort nonsensical turn >"$RUNTIME/invalid.out" 2>&1; then
  echo "FAIL: invalid effort accepted"; exit 1
fi
echo '--- restart fleetd: V4 preference must survive ---'
kill "$DAEMON_PID"; wait "$DAEMON_PID" 2>/dev/null || true
rm -f "$RUNTIME/fleetd.addr" "$RUNTIME/fleetd.sock"
"$BIN/fleetd" >"$RUNTIME/daemon-restart.out" 2>"$RUNTIME/daemon-restart.err" &
DAEMON_PID=$!
for _ in $(seq 1 50); do [[ -f "$RUNTIME/fleetd.addr" ]] && break; sleep 0.1; done
[[ -f "$RUNTIME/fleetd.addr" ]] || { echo "FAIL: restarted daemon did not start"; exit 1; }
"$BIN/fleetctl" controls "$SES" >"$RUNTIME/controls-restarted.out"
grep -q $'^effort\thigh\t' "$RUNTIME/controls-restarted.out" || {
  echo "FAIL: deferred effort did not survive fleetd restart"; cat "$RUNTIME/controls-restarted.out"; exit 1;
}
echo 'PASS: V4 control preference survived fleetd restart'

timeout 120 "$BIN/fleetctl" watch "$SES" --until-turn-end >"$RUNTIME/watch.out" 2>"$RUNTIME/watch.err" &
WATCH_PID=$!
sleep 1
"$BIN/fleetctl" prompt "$SES" 'Reply with exactly: CONTROL OK.' >/dev/null
wait "$WATCH_PID"
grep -q $'turn_completed\tCompleted' "$RUNTIME/watch.out" || { echo "FAIL: incomplete turn"; exit 1; }
echo '--- effective native control after the turn ---'
"$BIN/fleetctl" native-sessions codex >"$RUNTIME/native.out" 2>"$RUNTIME/native.err" || true
EFFECTIVE="$(awk -F '\t' -v ws="$WORKSPACE" '$2 == ws {print $6; exit}' "$RUNTIME/native.out")"
echo "native_effort=$EFFECTIVE"
[[ "$EFFECTIVE" == "high" ]] || {
  echo "FAIL: Codex did not report the configured effort on the native thread"; cat "$RUNTIME/native.out"; exit 1;
}
echo 'PASS: configured effort is the effective native value on the next turn'
echo '--- watch stdout ---'; cat "$RUNTIME/watch.out"
echo '--- daemon err tail ---'; tail -10 "$RUNTIME/daemon.err"
