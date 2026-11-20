#!/usr/bin/env bash
# P1.1: aislamiento entre sesiones, turno único y clave de operación concurrente.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
BIN="$ROOT/target/debug"
RUNTIME="$(mktemp -d /tmp/gf-p11-conc-XXXXXX)"
WORKSPACE="$(mktemp -d /tmp/gf-p11-conc-ws-XXXXXX)"

cleanup() {
  [[ -n "${DAEMON_PID:-}" ]] && kill "$DAEMON_PID" 2>/dev/null || true
  if [[ -z "${GF_KEEP:-}" ]]; then rm -rf "$RUNTIME" "$WORKSPACE"; fi
}
trap cleanup EXIT

start_daemon() {
  export FLEET_RUNTIME_DIR="$RUNTIME"
export FLEET_DB_PATH="$RUNTIME/fleet.sqlite"
  "$BIN/fleetd" >"$RUNTIME/daemon.out" 2>"$RUNTIME/daemon.err" &
  DAEMON_PID=$!
  for _ in $(seq 1 50); do
    [[ -f "$RUNTIME/fleetd.addr" ]] && return
    sleep 0.1
  done
  echo "FAIL: fleetd did not start"; exit 1
}

start_daemon
WORKSPACE_ID="$("$BIN/fleetctl" workspace add "$WORKSPACE" | cut -f1)"
A="$("$BIN/fleetctl" session create --workspace "$WORKSPACE_ID" --provider fake | cut -f1)"
B="$("$BIN/fleetctl" session create --workspace "$WORKSPACE_ID" --provider fake | cut -f1)"

"$BIN/fleetctl" watch "$A" --until-turn-end >"$RUNTIME/a.out" 2>"$RUNTIME/a.err" &
WA=$!
"$BIN/fleetctl" watch "$B" --until-turn-end >"$RUNTIME/b.out" 2>"$RUNTIME/b.err" &
WB=$!
for _ in $(seq 1 50); do
  if grep -q subscribed "$RUNTIME/a.err" 2>/dev/null && grep -q subscribed "$RUNTIME/b.err" 2>/dev/null; then break; fi
  sleep 0.1
done
grep -q subscribed "$RUNTIME/a.err"
grep -q subscribed "$RUNTIME/b.err"

"$BIN/fleetctl" prompt "$A" "only A" >/dev/null
wait "$WA"
if grep -q "only A" "$RUNTIME/b.out"; then echo "FAIL: session B saw session A events"; exit 1; fi
"$BIN/fleetctl" prompt "$B" "only B" >/dev/null
wait "$WB"
if grep -q "only B" "$RUNTIME/a.out"; then echo "FAIL: session A saw session B events"; exit 1; fi
echo "PASS: sessions are isolated"
"$BIN/fleetctl" stop "$A" >/dev/null
"$BIN/fleetctl" stop "$B" >/dev/null
kill "$DAEMON_PID"; wait "$DAEMON_PID" 2>/dev/null || true
unset DAEMON_PID
rm -f "$RUNTIME/fleetd.addr" "$RUNTIME/fleetd.sock"

# Segundo daemon: driver bloqueado para hacer determinista la carrera de turnos.
export FLEET_FAKE_BLOCK_SEND=1
start_daemon
WS2="$("$BIN/fleetctl" workspace add "$WORKSPACE" | cut -f1)"
C="$("$BIN/fleetctl" session create --workspace "$WS2" --provider fake | cut -f1)"

( FLEET_OPERATION_KEY=key-1 "$BIN/fleetctl" prompt "$C" "first" >"$RUNTIME/p1.out" 2>"$RUNTIME/p1.err" ) &
P1=$!
sleep 0.1
( FLEET_OPERATION_KEY=key-2 "$BIN/fleetctl" prompt "$C" "second" >"$RUNTIME/p2.out" 2>"$RUNTIME/p2.err" ) || true
wait "$P1"
if ! grep -q "TurnAlreadyRunning" "$RUNTIME/p2.err"; then
  echo "FAIL: second concurrent prompt was not rejected"; cat "$RUNTIME/p2.err"; exit 1
fi
echo "PASS: second prompt rejected with TurnAlreadyRunning"

FLEET_OPERATION_KEY=key-1 "$BIN/fleetctl" prompt "$C" "first" >"$RUNTIME/p3.out"
diff -u "$RUNTIME/p1.out" "$RUNTIME/p3.out"
echo "PASS: duplicate operation key returned the same receipt"

START=$(date +%s%N)
set +e
"$BIN/fleetctl" stop "$C" >"$RUNTIME/stop.out" 2>"$RUNTIME/stop.err"
STOP_RC=$?
set -e
END=$(date +%s%N)
ELAPSED_MS=$(( (END - START) / 1000000 ))
if (( ELAPSED_MS > 2000 )); then echo "FAIL: stop blocked for too long"; exit 1; fi
if (( STOP_RC == 0 )); then echo "FAIL: blocked driver reported successful stop"; exit 1; fi
grep -q "StopUnconfirmed" "$RUNTIME/stop.err" || { echo "FAIL: missing unconfirmed stop error"; exit 1; }
if ! "$BIN/fleetctl" session list | awk -F '\t' -v id="$C" '$1==id && $4=="StopUnconfirmed" {found=1} END{exit !found}'; then
  echo "FAIL: blocked driver did not enter StopUnconfirmed"; exit 1
fi
echo "PASS: blocked driver stop is bounded and explicitly unconfirmed"
