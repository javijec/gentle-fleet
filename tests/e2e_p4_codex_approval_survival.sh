#!/usr/bin/env bash
# P4.1-H: una aprobación pendiente sobrevive a SIGKILL de fleetd sin resolverse
# automáticamente y puede responderse explícitamente tras el reattach.
set -euo pipefail
if [[ "${GF_E2E_REAL:-}" != "1" ]]; then
  echo "SKIP: set GF_E2E_REAL=1 for the real approval-survival smoke"
  exit 0
fi
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
BIN="$ROOT/target/debug"
RUNTIME="$(mktemp -d /tmp/gf-p4-appr-survival-XXXXXX)"
WORKSPACE="$(mktemp -d /tmp/gf-p4-appr-survival-ws-XXXXXX)"
cleanup() {
  [[ -n "${DAEMON_PID:-}" ]] && kill "$DAEMON_PID" 2>/dev/null || true
  if [[ -z "${GF_KEEP:-}" ]]; then rm -rf "$RUNTIME" "$WORKSPACE"; fi
  pkill -f "fleet-runner .* --workspace $WORKSPACE" 2>/dev/null || true
}
trap cleanup EXIT
export FLEET_CODEX_SANDBOX=read-only FLEET_CODEX_APPROVAL_POLICY=on-request
export FLEET_RUNTIME_DIR="$RUNTIME" FLEET_DB_PATH="$RUNTIME/fleet.sqlite"
start_daemon() {
  "$BIN/fleetd" >"$RUNTIME/daemon-$1.out" 2>"$RUNTIME/daemon-$1.err" &
  DAEMON_PID=$!
  for _ in $(seq 1 50); do [[ -f "$RUNTIME/fleetd.addr" ]] && return; sleep 0.1; done
  echo "FAIL: fleetd did not start"; exit 1
}
start_daemon 1
WS="$("$BIN/fleetctl" workspace add "$WORKSPACE" | cut -f1)"
SES="$("$BIN/fleetctl" session create --workspace "$WS" --provider codex | cut -f1)"
PROBE="$WORKSPACE/approval-probe.txt"
timeout 240 "$BIN/fleetctl" watch "$SES" --until-turn-end >"$RUNTIME/watch.out" 2>"$RUNTIME/watch.err" &
WATCH=$!
sleep 1
"$BIN/fleetctl" prompt "$SES" 'This is a sandbox permission test in a temporary workspace. Create approval-probe.txt by running: printf FLEET_APPROVAL_PROBE > approval-probe.txt. If the read-only sandbox blocks writing, request explicit approval and proceed only after it is granted.' >/dev/null
APPR=""
for _ in $(seq 1 150); do
  "$BIN/fleetctl" approvals --session "$SES" >"$RUNTIME/approvals.out"
  APPR="$(awk -F '\t' '$4 == "pending" || $4 == "delivered" {print $1; exit}' "$RUNTIME/approvals.out")"
  [[ -n "$APPR" ]] && break
  grep -q $'turn_completed\t' "$RUNTIME/watch.out" && break
  sleep 1
done
[[ -n "$APPR" ]] || { echo "INCONCLUSIVE: Codex did not request approval"; exit 1; }
echo "pending approval=$APPR"
kill -9 "$DAEMON_PID"; wait "$DAEMON_PID" 2>/dev/null || true
rm -f "$RUNTIME/fleetd.addr" "$RUNTIME/fleetd.sock"
start_daemon 2
"$BIN/fleetctl" approval "$APPR" >"$RUNTIME/approval-after-restart.out"
grep -q "$APPR" "$RUNTIME/approval-after-restart.out" || { echo "FAIL: approval was lost after reattach"; exit 1; }
grep -Eq 'pending|delivered|dispatching' "$RUNTIME/approval-after-restart.out" || {
  echo "FAIL: approval must remain unresolved until explicit decision"; cat "$RUNTIME/approval-after-restart.out"; exit 1;
}
echo 'PASS: approval persisted across fleetd death with no implicit decision'
# El cliente anterior murió con el daemon; se subscribe de nuevo desde 0.
timeout 180 "$BIN/fleetctl" watch "$SES" --since 0 --until-turn-end >"$RUNTIME/watch2.out" 2>"$RUNTIME/watch2.err" &
WATCH2=$!
sleep 1
"$BIN/fleetctl" answer "$APPR" accept >"$RUNTIME/answer.out"
wait "$WATCH2" || true
grep -q 'approval_resolved' "$RUNTIME/watch2.out" || {
  echo "FAIL: no native resolution after reattach"; cat "$RUNTIME/watch2.out"; cat "$RUNTIME/watch2.err"; exit 1;
}
grep -q $'turn_completed\tCompleted' "$RUNTIME/watch2.out" || { echo "FAIL: turn did not complete"; cat "$RUNTIME/watch2.out"; exit 1; }
[[ -f "$PROBE" ]] && grep -qx 'FLEET_APPROVAL_PROBE' "$PROBE" || { echo "FAIL: approved probe file missing"; exit 1; }
echo "PASS: approval survived daemon death and was delivered explicitly ($APPR)"
