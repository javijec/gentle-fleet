#!/usr/bin/env bash
# P3: aprobación pendible mientras fleetd muere; se responde tras el reattach.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
BIN="$ROOT/target/debug"
RUNTIME="$(mktemp -d /tmp/gf-p3-appr-disc-XXXXXX)"
WORKSPACE="$(mktemp -d /tmp/gf-p3-appr-disc-ws-XXXXXX)"

cleanup() {
  [[ -n "${RUNNER_PID:-}" ]] && kill "$RUNNER_PID" 2>/dev/null || true
  [[ -n "${DAEMON_PID:-}" ]] && kill "$DAEMON_PID" 2>/dev/null || true
  if [[ -z "${GF_KEEP:-}" ]]; then rm -rf "$RUNTIME" "$WORKSPACE"; fi
}
trap cleanup EXIT

export FLEET_RUNTIME_DIR="$RUNTIME"
export FLEET_DB_PATH="$RUNTIME/fleet.sqlite"
export FLEET_FAKE_APPROVAL=1

start_daemon() {
  "$BIN/fleetd" >"$RUNTIME/daemon-$1.out" 2>"$RUNTIME/daemon-$1.err" &
  DAEMON_PID=$!
  for _ in $(seq 1 50); do [[ -f "$RUNTIME/fleetd.addr" ]] && return; sleep 0.1; done
  echo "FAIL: fleetd did not start"; exit 1
}

start_daemon 1
WS_ID="$("$BIN/fleetctl" workspace add "$WORKSPACE" | cut -f1)"
SID="$("$BIN/fleetctl" session create --workspace "$WS_ID" --provider fake | cut -f1)"
"$BIN/fleetctl" watch "$SID" >"$RUNTIME/watch.out" 2>"$RUNTIME/watch.err" &
WATCH=$!
for _ in $(seq 1 50); do grep -q subscribed "$RUNTIME/watch.err" 2>/dev/null && break; sleep 0.1; done
"$BIN/fleetctl" prompt "$SID" "approval across restart" >/dev/null
APPROVAL=""
for _ in $(seq 1 100); do
  APPROVAL="$(awk -F '\t' '$2=="approval_requested" {print $3; exit}' "$RUNTIME/watch.out" 2>/dev/null || true)"
  [[ -n "$APPROVAL" ]] && break
  sleep 0.05
done
[[ -n "$APPROVAL" ]] || { echo "FAIL: no approval request"; exit 1; }

RUNNER_PID="$(pgrep -f "fleet-runner --runner-id runner-$SID" | head -1)"
kill -9 "$DAEMON_PID"; wait "$DAEMON_PID" 2>/dev/null || true
unset DAEMON_PID
kill -0 "$RUNNER_PID" 2>/dev/null || { echo "FAIL: runner died with daemon"; exit 1; }
sleep 1
echo "PASS: approval pending while daemon was down"

rm -f "$RUNTIME/fleetd.addr" "$RUNTIME/fleetd.sock"
start_daemon 2
for _ in $(seq 1 100); do
  "$BIN/fleetctl" approvals --session "$SID" | grep -q "$APPROVAL" && break
  sleep 0.1
done
"$BIN/fleetctl" approvals --session "$SID" | grep -q "$APPROVAL"
FLEET_OPERATION_KEY=p3-appr-key "$BIN/fleetctl" answer "$APPROVAL" once >/dev/null
for _ in $(seq 1 100); do
  "$BIN/fleetctl" approval "$APPROVAL" | grep -q accepted && break
  sleep 0.1
done
"$BIN/fleetctl" approval "$APPROVAL" | grep -q accepted
"$BIN/fleetctl" timeline "$SID" --since 0 --limit 100 | grep -q 'approval_resolved'
echo "PASS: approval survived daemon death and was decided once after reattach"

kill "$WATCH" 2>/dev/null || true
