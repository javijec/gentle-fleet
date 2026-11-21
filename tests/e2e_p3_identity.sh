#!/usr/bin/env bash
# P3: identidad/token inválidos impiden el reattachment; nunca se inventa Running.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
BIN="$ROOT/target/debug"
RUNTIME="$(mktemp -d /tmp/gf-p3-identity-XXXXXX)"
WORKSPACE="$(mktemp -d /tmp/gf-p3-identity-ws-XXXXXX)"

cleanup() {
  [[ -n "${RUNNER_PID:-}" ]] && kill "$RUNNER_PID" 2>/dev/null || true
  [[ -n "${DAEMON_PID:-}" ]] && kill "$DAEMON_PID" 2>/dev/null || true
  if [[ -z "${GF_KEEP:-}" ]]; then rm -rf "$RUNTIME" "$WORKSPACE"; fi
}
trap cleanup EXIT

export FLEET_RUNTIME_DIR="$RUNTIME"
export FLEET_DB_PATH="$RUNTIME/fleet.sqlite"

start_daemon() {
  "$BIN/fleetd" >"$RUNTIME/daemon-$1.out" 2>"$RUNTIME/daemon-$1.err" &
  DAEMON_PID=$!
  for _ in $(seq 1 50); do [[ -f "$RUNTIME/fleetd.addr" ]] && return; sleep 0.1; done
  echo "FAIL: fleetd did not start"; exit 1
}

start_daemon 1
WS_ID="$("$BIN/fleetctl" workspace add "$WORKSPACE" | cut -f1)"
SID="$("$BIN/fleetctl" session create --workspace "$WS_ID" --provider fake | cut -f1)"
RUNNER_PID="$(pgrep -f "fleet-runner --runner-id runner-$SID" | head -1)"
[[ -n "$RUNNER_PID" ]] || { echo "FAIL: no runner"; exit 1; }

kill -9 "$DAEMON_PID"; wait "$DAEMON_PID" 2>/dev/null || true
unset DAEMON_PID
TOKEN_FILE="$RUNTIME/fleet-runner-runner-$SID.token"
ORIG_TOKEN="$(cat "$TOKEN_FILE")"
printf 'tampered-token' >"$TOKEN_FILE"
rm -f "$RUNTIME/fleetd.addr" "$RUNTIME/fleetd.sock"
start_daemon 2
sleep 2
"$BIN/fleetctl" session list | awk -F '\t' -v id="$SID" '$1==id && $4=="NeedsReconciliation" {found=1} END{exit !found}'
if "$BIN/fleetctl" session list | awk -F '\t' -v id="$SID" '$1==id && ($4=="Running" || $4=="Recovering") {found=1} END{exit !found}'; then
  echo "FAIL: reattached with a tampered token"; exit 1
fi
echo "PASS: invalid runner identity leaves session in NeedsReconciliation"

# Una versión de protocolo incompatible también se rechaza.
sqlite3 "$RUNTIME/fleet.sqlite" "UPDATE runners SET protocol_version='fleet-runner-v0' WHERE runner_id='runner-$SID';"
printf '%s' "$ORIG_TOKEN" >"$TOKEN_FILE"
kill "$DAEMON_PID"; wait "$DAEMON_PID" 2>/dev/null || true
unset DAEMON_PID
rm -f "$RUNTIME/fleetd.addr" "$RUNTIME/fleetd.sock"
start_daemon 3
sleep 2
"$BIN/fleetctl" session list | awk -F '\t' -v id="$SID" '$1==id && $4=="NeedsReconciliation" {found=1} END{exit !found}'
echo "PASS: incompatible runner protocol was rejected"
