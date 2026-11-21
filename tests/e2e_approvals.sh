#!/usr/bin/env bash
# P2: aprobaciones con un solo ganador, decisión rechazada y evento observable.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
BIN="$ROOT/target/debug"
RUNTIME="$(mktemp -d /tmp/gf-p2-appr-XXXXXX)"
WORKSPACE="$(mktemp -d /tmp/gf-p2-appr-ws-XXXXXX)"

cleanup() {
  [[ -n "${DAEMON_PID:-}" ]] && kill "$DAEMON_PID" 2>/dev/null || true
  if [[ -z "${GF_KEEP:-}" ]]; then rm -rf "$RUNTIME" "$WORKSPACE"; fi
    pkill -f "fleet-runner .* --workspace $WORKSPACE" 2>/dev/null || true
}
trap cleanup EXIT

export FLEET_RUNTIME_DIR="$RUNTIME"
export FLEET_DB_PATH="$RUNTIME/fleet.sqlite"
export FLEET_FAKE_APPROVAL=1

"$BIN/fleetd" >"$RUNTIME/daemon.out" 2>"$RUNTIME/daemon.err" &
DAEMON_PID=$!
for _ in $(seq 1 50); do [[ -f "$RUNTIME/fleetd.addr" ]] && break; sleep 0.1; done
[[ -f "$RUNTIME/fleetd.addr" ]] || { echo "FAIL: fleetd did not start"; exit 1; }

WS_ID="$("$BIN/fleetctl" workspace add "$WORKSPACE" | cut -f1)"
SID="$("$BIN/fleetctl" session create --workspace "$WS_ID" --provider fake | cut -f1)"

"$BIN/fleetctl" watch "$SID" --until-turn-end >"$RUNTIME/watch.out" 2>"$RUNTIME/watch.err" &
WATCH=$!
for _ in $(seq 1 50); do grep -q subscribed "$RUNTIME/watch.err" 2>/dev/null && break; sleep 0.1; done
"$BIN/fleetctl" prompt "$SID" "allow this command" >/dev/null
APPROVAL_ID=""
for _ in $(seq 1 100); do
  APPROVAL_ID="$(awk -F '\t' '$2=="approval_requested" {print $3; exit}' "$RUNTIME/watch.out" 2>/dev/null || true)"
  [[ -n "$APPROVAL_ID" ]] && break
  sleep 0.05
done
[[ -n "$APPROVAL_ID" ]] || { echo "FAIL: no approval request observed"; exit 1; }

set +e
FLEET_OPERATION_KEY=answer-once "$BIN/fleetctl" answer "$APPROVAL_ID" once >"$RUNTIME/answer1" 2>"$RUNTIME/answer1.err" &
A1=$!
sleep 0.05
FLEET_OPERATION_KEY=answer-reject "$BIN/fleetctl" answer "$APPROVAL_ID" reject >"$RUNTIME/answer2" 2>"$RUNTIME/answer2.err"
A2=$?
wait "$A1"
A1=$?
set -e

WINS=0
for f in answer1 answer2; do
  if grep -qi 'dispatching' "$RUNTIME/$f"; then WINS=$((WINS+1)); fi
done
[[ "$WINS" == "1" ]] || { echo "FAIL: expected exactly one winning decision"; cat "$RUNTIME/answer1" "$RUNTIME/answer2" "$RUNTIME/answer1.err" "$RUNTIME/answer2.err"; exit 1; }
echo "PASS: exactly one decision winner"

wait "$WATCH"
"$BIN/fleetctl" approval "$APPROVAL_ID" | grep -qiE 'accepted|rejected'
grep -q approval_resolved "$RUNTIME/watch.out" || "$BIN/fleetctl" timeline "$SID" --since 0 --limit 100 | grep -q approval_resolved
echo "PASS: approval result observable"

# Un rechazo explícito en otra sesión.
SID2="$("$BIN/fleetctl" session create --workspace "$WS_ID" --provider fake | cut -f1)"
"$BIN/fleetctl" watch "$SID2" --until-turn-end >"$RUNTIME/watch2.out" 2>"$RUNTIME/watch2.err" &
WATCH2=$!
for _ in $(seq 1 50); do grep -q subscribed "$RUNTIME/watch2.err" 2>/dev/null && break; sleep 0.1; done
"$BIN/fleetctl" prompt "$SID2" "reject this command" >/dev/null
APPROVAL2=""
for _ in $(seq 1 100); do
  APPROVAL2="$(awk -F '\t' '$2=="approval_requested" {print $3; exit}' "$RUNTIME/watch2.out" 2>/dev/null || true)"
  [[ -n "$APPROVAL2" ]] && break
  sleep 0.05
done
[[ -n "$APPROVAL2" ]] || { echo "FAIL: second approval never appeared"; exit 1; }
FLEET_OPERATION_KEY=answer2-key "$BIN/fleetctl" answer "$APPROVAL2" reject | grep -qiE 'dispatching|rejected'
wait "$WATCH2"
"$BIN/fleetctl" approval "$APPROVAL2" | grep -qi rejected
echo "PASS: rejection recorded"


# Un fallo de entrega deja la aprobación y la operación en uncertain, sin reenvío.
kill "$DAEMON_PID"; wait "$DAEMON_PID" 2>/dev/null || true
unset DAEMON_PID
rm -f "$RUNTIME/fleetd.addr" "$RUNTIME/fleetd.sock"
export FLEET_FAKE_FAIL_ANSWER=1
"$BIN/fleetd" >"$RUNTIME/daemon-fail.out" 2>"$RUNTIME/daemon-fail.err" &
DAEMON_PID=$!
for _ in $(seq 1 50); do [[ -f "$RUNTIME/fleetd.addr" ]] && break; sleep 0.1; done
SID3="$("$BIN/fleetctl" session create --workspace "$WS_ID" --provider fake | cut -f1)"
"$BIN/fleetctl" watch "$SID3" >"$RUNTIME/watch3.out" 2>"$RUNTIME/watch3.err" &
WATCH3=$!
for _ in $(seq 1 50); do grep -q subscribed "$RUNTIME/watch3.err" 2>/dev/null && break; sleep 0.1; done
"$BIN/fleetctl" prompt "$SID3" "fail delivery" >/dev/null
APPROVAL3=""
for _ in $(seq 1 100); do
  APPROVAL3="$(awk -F '\t' '$2=="approval_requested" {print $3; exit}' "$RUNTIME/watch3.out" 2>/dev/null || true)"
  [[ -n "$APPROVAL3" ]] && break
  sleep 0.05
done
[[ -n "$APPROVAL3" ]] || { echo "FAIL: no approval for uncertain case"; exit 1; }
set +e
FLEET_OPERATION_KEY=fail-key "$BIN/fleetctl" answer "$APPROVAL3" once >"$RUNTIME/answer3.out" 2>"$RUNTIME/answer3.err"
set -e
grep -qiE 'dispatching|NeedsReconciliation' "$RUNTIME/answer3.out" "$RUNTIME/answer3.err" || { echo "FAIL: answer result was neither dispatching nor uncertain"; cat "$RUNTIME/answer3.out" "$RUNTIME/answer3.err"; exit 1; }
for _ in $(seq 1 100); do
  "$BIN/fleetctl" approval "$APPROVAL3" | grep -q uncertain && break
  sleep 0.05
done
"$BIN/fleetctl" approval "$APPROVAL3" | grep -q uncertain || { echo "FAIL: approval is not uncertain"; exit 1; }
"$BIN/fleetctl" operation fail-key | grep -qi uncertain
set +e
FLEET_OPERATION_KEY=fail-key-retry "$BIN/fleetctl" answer "$APPROVAL3" once >"$RUNTIME/answer4.out" 2>"$RUNTIME/answer4.err"
RETRY_CODE=$?
set -e
[[ "$RETRY_CODE" != "0" ]] || { echo "FAIL: uncertain approval was retried"; exit 1; }
if ! grep -qi 'NeedsReconciliation' "$RUNTIME/answer4.err"; then
  echo "FAIL: retry was not rejected as uncertain"; cat "$RUNTIME/answer4.err"; exit 1
fi
kill "$WATCH3" 2>/dev/null || true
echo "PASS: uncertain approval delivery is durable and never auto-retried"
