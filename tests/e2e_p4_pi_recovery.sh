#!/usr/bin/env bash
# P4.2-F/G: el runner Pi posee el proceso y sobrevive a SIGKILL de fleetd.
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
BIN="$ROOT/target/debug"
RUNTIME="$(mktemp -d /tmp/gf-p4-pi-rec-XXXXXX)"
WORKSPACE="$(mktemp -d /tmp/gf-p4-pi-rec-ws-XXXXXX)"
cleanup() {
  [[ -n "${DAEMON_PID:-}" ]] && kill "$DAEMON_PID" 2>/dev/null || true
  [[ -n "${RUNNER_PID:-}" ]] && kill "$RUNNER_PID" 2>/dev/null || true
  if [[ -z "${GF_KEEP:-}" ]]; then rm -rf "$RUNTIME" "$WORKSPACE"; fi
  pkill -f "fleet-runner .* --workspace $WORKSPACE" 2>/dev/null || true
}
trap cleanup EXIT
export FLEET_RUNTIME_DIR="$RUNTIME" FLEET_DB_PATH="$RUNTIME/fleet.sqlite"
start_daemon() {
  "$BIN/fleetd" >"$RUNTIME/daemon-$1.out" 2>"$RUNTIME/daemon-$1.err" &
  DAEMON_PID=$!
  for _ in $(seq 1 50); do [[ -f "$RUNTIME/fleetd.addr" ]] && return; sleep 0.1; done
  echo "FAIL: daemon did not start"; exit 1
}
start_daemon 1
WS="$("$BIN/fleetctl" workspace add "$WORKSPACE" | cut -f1)"
SES="$("$BIN/fleetctl" session create --workspace "$WS" --provider pi | cut -f1)"
RUNNER_PID="$(pgrep -f "fleet-runner --runner-id runner-$SES" | head -1)"
[[ -n "$RUNNER_PID" ]] || { echo "FAIL: no Pi runner"; exit 1; }
kill -9 "$DAEMON_PID"; wait "$DAEMON_PID" 2>/dev/null || true
sleep 1
kill -0 "$RUNNER_PID" || { echo "FAIL: Pi runner died with fleetd"; exit 1; }
echo 'PASS: Pi runner alive after SIGKILL of fleetd'
rm -f "$RUNTIME/fleetd.addr" "$RUNTIME/fleetd.sock"
start_daemon 2
for _ in $(seq 1 80); do
  "$BIN/fleetctl" session list | awk -F '\t' -v id="$SES" '$1==id && ($4=="Running" || $4=="Recovering") {found=1} END{exit !found}' && break
  sleep 0.1
done
"$BIN/fleetctl" session list | awk -F '\t' -v id="$SES" '$1==id && ($4=="Running" || $4=="Recovering") {found=1} END{exit !found}' || {
  echo "FAIL: Pi session was not reattached"; exit 1;
}
"$BIN/fleetctl" timeline "$SES" --limit 100 >"$RUNTIME/timeline.out"
[[ "$(grep -c 'session_created' "$RUNTIME/timeline.out")" == "1" ]] || { echo "FAIL: duplicated session creation"; exit 1; }
echo "PASS: Pi reattach, durable timeline and no duplicated session events"
