#!/usr/bin/env bash
# P2.1: subscribe from seq=0 after >256 persisted events, without gaps.
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
BIN="$ROOT/target/debug"
RUNTIME="$(mktemp -d /tmp/gf-p21-replay-XXXXXX)"
WORKSPACE="$(mktemp -d /tmp/gf-p21-replay-ws-XXXXXX)"
cleanup() {
  [[ -n "${DAEMON_PID:-}" ]] && kill "$DAEMON_PID" 2>/dev/null || true
  rm -rf "$RUNTIME" "$WORKSPACE"
}
trap cleanup EXIT
export FLEET_RUNTIME_DIR="$RUNTIME"
export FLEET_DB_PATH="$RUNTIME/fleet.sqlite"
"$BIN/fleetd" >"$RUNTIME/fleetd.log" 2>&1 &
DAEMON_PID=$!
for _ in $(seq 1 50); do [[ -f "$RUNTIME/fleetd.addr" ]] && break; sleep 0.1; done
[[ -f "$RUNTIME/fleetd.addr" ]] || { echo "FAIL: fleetd did not start"; exit 1; }
WS="$("$BIN/fleetctl" workspace add "$WORKSPACE" | cut -f1)"
SID="$("$BIN/fleetctl" session create --workspace "$WS" --provider fake | cut -f1)"
for n in $(seq 1 32); do
  key="replay-$n"
  FLEET_OPERATION_KEY="$key" "$BIN/fleetctl" prompt "$SID" "message $n" >/dev/null
  completed=0
  for _ in $(seq 1 100); do
    if "$BIN/fleetctl" operation "$key" | grep -qi 'Completed'; then
      completed=1
      break
    fi
    sleep 0.02
  done
  if [[ "$completed" != 1 ]]; then echo "FAIL: prompt $n never completed"; exit 1; fi
done
timeout 20s "$BIN/fleetctl" watch "$SID" --since 0 --count 270 >"$RUNTIME/replay.out" 2>"$RUNTIME/replay.err"
awk -F '\t' '
  NR == 1 && $1 != 1 { exit 1 }
  NR > 1 && $1 != previous + 1 { exit 1 }
  {previous = $1}
  END {if (NR != 270 || previous != 270) exit 1}
' "$RUNTIME/replay.out" || {
  echo "FAIL: 270-event replay was incomplete or out of order"
  cat "$RUNTIME/replay.err"
  exit 1
}
echo "PASS: long timeline replayed from 1 through 270 without gaps"
