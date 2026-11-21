#!/usr/bin/env bash
# P3: dos daemon-kills consecutivos con varias sesiones concurrentes.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
BIN="$ROOT/target/debug"
RUNTIME="$(mktemp -d /tmp/gf-p3-multi-XXXXXX)"
WORKSPACE="$(mktemp -d /tmp/gf-p3-multi-ws-XXXXXX)"
SLOW_MS=2500

cleanup() {
  [[ -n "${DAEMON_PID:-}" ]] && kill "$DAEMON_PID" 2>/dev/null || true
  if [[ -z "${GF_KEEP:-}" ]]; then rm -rf "$RUNTIME" "$WORKSPACE"; fi
    pkill -f "fleet-runner .* --workspace $WORKSPACE" 2>/dev/null || true
}
trap cleanup EXIT

export FLEET_RUNTIME_DIR="$RUNTIME"
export FLEET_DB_PATH="$RUNTIME/fleet.sqlite"
export FLEET_FAKE_SLOW_TURN_MS="$SLOW_MS"

start_daemon() {
  "$BIN/fleetd" >"$RUNTIME/daemon-$1.out" 2>"$RUNTIME/daemon-$1.err" &
  DAEMON_PID=$!
  for _ in $(seq 1 50); do [[ -f "$RUNTIME/fleetd.addr" ]] && return; sleep 0.1; done
  echo "FAIL: fleetd did not start ($1)"; exit 1
}

start_daemon 1
WS_ID="$("$BIN/fleetctl" workspace add "$WORKSPACE" | cut -f1)"
SESSIONS=""
for n in 1 2 3; do
  SID="$("$BIN/fleetctl" session create --workspace "$WS_ID" --provider fake | cut -f1)"
  SESSIONS="$SESSIONS $SID"
  FLEET_OPERATION_KEY="multi-$n" "$BIN/fleetctl" prompt "$SID" "multi session $n" >/dev/null
done
echo "sessions:$SESSIONS"

# Dos caídas consecutivas del daemon; los runners deben sobrevivir ambas.
for round in 1 2; do
  sleep 0.4
  kill -9 "$DAEMON_PID"; wait "$DAEMON_PID" 2>/dev/null || true
  unset DAEMON_PID
  sleep 1
  for SID in $SESSIONS; do
    if ! pgrep -f "fleet-runner --runner-id runner-$SID" >/dev/null; then
      echo "FAIL: runner for $SID died in round $round"; exit 1
    fi
  done
  sleep $((SLOW_MS / 1000))
  rm -f "$RUNTIME/fleetd.addr" "$RUNTIME/fleetd.sock"
  start_daemon "after-$round"
done

for SID in $SESSIONS; do
  for _ in $(seq 1 200); do
    "$BIN/fleetctl" timeline "$SID" --since 0 --limit 100 >"$RUNTIME/tl-$SID"
    grep -q 'turn_completed' "$RUNTIME/tl-$SID" && break
    sleep 0.1
  done
  grep -q 'turn_completed' "$RUNTIME/tl-$SID" || { echo "FAIL: $SID never completed"; exit 1; }
  for n in 1 2 3; do
    :
  done
  USERS="$(grep -c 'user.multi session' "$RUNTIME/tl-$SID" || true)"
  [[ "$USERS" == "1" ]] || { echo "FAIL: prompt duplicated in $SID ($USERS)"; exit 1; }
done
echo "PASS: three concurrent sessions survived two daemon kills without duplicate prompts"
