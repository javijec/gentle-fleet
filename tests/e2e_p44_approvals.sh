#!/usr/bin/env bash
# P4.4-H: aprobaciones deterministas con FakeDriver, sin permisos nativos.
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
BIN="$ROOT/target/debug"
RUNTIME="$(mktemp -d /tmp/gf-p44-appr-XXXXXX)"
WORKSPACE="$(mktemp -d /tmp/gf-p44-appr-ws-XXXXXX)"
cleanup() {
  [[ -n "${DAEMON_PID:-}" ]] && kill "$DAEMON_PID" 2>/dev/null || true
  if [[ -z "${GF_KEEP:-}" ]]; then rm -rf "$RUNTIME" "$WORKSPACE"; fi
  pkill -f "fleet-runner .* --workspace $WORKSPACE" 2>/dev/null || true
}
trap cleanup EXIT
export FLEET_RUNTIME_DIR="$RUNTIME" FLEET_DB_PATH="$RUNTIME/fleet.sqlite"
export FLEET_FAKE_APPROVAL=1
start_daemon() {
  "$BIN/fleetd" >"$RUNTIME/daemon-$1.out" 2>"$RUNTIME/daemon-$1.err" &
  DAEMON_PID=$!
  for _ in $(seq 1 50); do [[ -f "$RUNTIME/fleetd.addr" ]] && return; sleep 0.1; done
  echo "FAIL: daemon did not start"; exit 1
}
start_daemon 1
WS="$("$BIN/fleetctl" workspace add "$WORKSPACE" | cut -f1)"
SES="$("$BIN/fleetctl" session create --workspace "$WS" --provider fake | cut -f1)"
"$BIN/fleetctl" prompt "$SES" 'approval contract probe' >/dev/null
APPR=""
for _ in $(seq 1 80); do
  "$BIN/fleetctl" approvals --session "$SES" >"$RUNTIME/approvals.out"
  APPR="$(awk -F '\t' '$4 == "pending" {print $1; exit}' "$RUNTIME/approvals.out")"
  [[ -n "$APPR" ]] && break
  sleep 0.1
done
[[ -n "$APPR" ]] || { echo "FAIL: no pending approval"; exit 1; }
grep -q "$APPR" "$RUNTIME/approvals.out"
# FakeDriver anuncia exactamente once/reject.
grep -Eq 'once,reject|reject,once' "$RUNTIME/approvals.out" || { echo "FAIL: announced options missing"; cat "$RUNTIME/approvals.out"; exit 1; }
if "$BIN/fleetctl" answer "$APPR" always >"$RUNTIME/invalid.out" 2>&1; then
  echo "FAIL: unannounced option accepted"; exit 1
fi
grep -qi 'InvalidDecision\|invalid' "$RUNTIME/invalid.out" || { echo "FAIL: wrong rejection error"; cat "$RUNTIME/invalid.out"; exit 1; }
"$BIN/fleetctl" approval "$APPR" >"$RUNTIME/after-invalid.out"
grep -q $'pending' "$RUNTIME/after-invalid.out" || { echo "FAIL: approval auto-accepted after rejected answer"; exit 1; }

# Desconexión del daemon con aprobación pendiente y reanudación.
kill -9 "$DAEMON_PID"; wait "$DAEMON_PID" 2>/dev/null || true
rm -f "$RUNTIME/fleetd.addr" "$RUNTIME/fleetd.sock"
start_daemon 2
"$BIN/fleetctl" approval "$APPR" >"$RUNTIME/after-restart.out"
grep -q "$APPR" "$RUNTIME/after-restart.out" || { echo "FAIL: approval lost after daemon death"; exit 1; }
grep -q $'pending' "$RUNTIME/after-restart.out" || { echo "FAIL: approval was implicitly resolved"; exit 1; }
"$BIN/fleetctl" answer "$APPR" reject >"$RUNTIME/answer.out"
if "$BIN/fleetctl" answer "$APPR" reject >"$RUNTIME/duplicate.out" 2>&1; then
  echo "FAIL: duplicate approval answer accepted"; exit 1
fi
sleep 1
"$BIN/fleetctl" timeline "$SES" --limit 256 >"$RUNTIME/timeline.out"
[[ "$(grep -c 'approval_resolved' "$RUNTIME/timeline.out")" == "1" ]] || { echo "FAIL: approval resolved more than once"; exit 1; }
[[ "$(grep -c 'turn_completed' "$RUNTIME/timeline.out")" == "1" ]] || { echo "FAIL: contradictory terminal events"; exit 1; }
! grep -Eq 'native-|per_[a-f0-9]{8,}' "$RUNTIME/timeline.out" || { echo "FAIL: native approval id leaked"; exit 1; }
echo "PASS: pending, explicit, idempotent and reconnect-safe approval contract"
