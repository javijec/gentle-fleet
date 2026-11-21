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
    pkill -f "fleet-runner .* --workspace $WORKSPACE" 2>/dev/null || true
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


# ReadTimeline devuelve páginas acotadas: una petición --limit 300 no supera 256.
"$BIN/fleetctl" timeline "$SID" --since 0 --limit 300 >"$RUNTIME/page1"
PAGE1="$(wc -l <"$RUNTIME/page1")"
[[ "$PAGE1" -eq 256 ]] || { echo "FAIL: page cap is $PAGE1, expected 256"; exit 1; }
[[ "$(head -1 "$RUNTIME/page1" | cut -f1)" == "1" ]]
[[ "$(tail -1 "$RUNTIME/page1" | cut -f1)" == "256" ]]
# Recorrer todas las páginas para conocer el total sin cargarlas de una vez.
TOTAL=0
CURSOR=0
while true; do
  "$BIN/fleetctl" timeline "$SID" --since "$CURSOR" --limit 256 >"$RUNTIME/page"
  COUNT="$(wc -l <"$RUNTIME/page")"
  TOTAL=$((TOTAL + COUNT))
  [[ "$COUNT" -lt 256 ]] && break
  CURSOR="$(tail -1 "$RUNTIME/page" | cut -f1)"
done
[[ "$TOTAL" -gt 256 ]] || { echo "FAIL: timeline is not longer than one page"; exit 1; }
TAIL_COUNT=$((TOTAL - 270))
timeout 20s "$BIN/fleetctl" watch "$SID" --since 270 --count "$TAIL_COUNT" >"$RUNTIME/tail.out" 2>"$RUNTIME/tail.err"
awk -F '\t' -v expected="$TOTAL" '
  NR == 1 && $1 != 271 { exit 1 }
  NR > 1 && $1 != previous + 1 { exit 1 }
  { previous = $1 }
  END {if (NR != expected - 270 || previous != expected) exit 1}
' "$RUNTIME/tail.out" || { echo "FAIL: live tail after replay had gaps"; cat "$RUNTIME/tail.err"; exit 1; }
echo "PASS: paged ReadTimeline and live tail after 256-event cutoff"
