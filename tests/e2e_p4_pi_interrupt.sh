#!/usr/bin/env bash
# P4.2-D: interrupción real de un turno Pi en streaming.
set -euo pipefail
if [[ "${GF_E2E_REAL:-}" != "1" ]]; then
  echo "SKIP: set GF_E2E_REAL=1 for a real Pi interrupt smoke"
  exit 0
fi
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
BIN="$ROOT/target/debug"
RUNTIME="$(mktemp -d /tmp/gf-p4-pi-int-XXXXXX)"
WORKSPACE="$(mktemp -d /tmp/gf-p4-pi-int-ws-XXXXXX)"
cleanup() {
  [[ -n "${DAEMON_PID:-}" ]] && kill "$DAEMON_PID" 2>/dev/null || true
  if [[ -z "${GF_KEEP:-}" ]]; then rm -rf "$RUNTIME" "$WORKSPACE"; fi
  pkill -f "fleet-runner .* --workspace $WORKSPACE" 2>/dev/null || true
}
trap cleanup EXIT
export FLEET_RUNTIME_DIR="$RUNTIME" FLEET_DB_PATH="$RUNTIME/fleet.sqlite"
"$BIN/fleetd" >"$RUNTIME/daemon.out" 2>"$RUNTIME/daemon.err" &
DAEMON_PID=$!
for _ in $(seq 1 50); do [[ -f "$RUNTIME/fleetd.addr" ]] && break; sleep 0.1; done
WS="$("$BIN/fleetctl" workspace add "$WORKSPACE" | cut -f1)"
SES="$("$BIN/fleetctl" session create --workspace "$WS" --provider pi | cut -f1)"
"$BIN/fleetctl" controls "$SES" >"$RUNTIME/controls.out"
[[ -n "$(awk -F '\t' '$1=="model" {print $3}' "$RUNTIME/controls.out")" ]] || {
  echo "UNTESTED: Pi reports no configured model"; exit 0;
}
timeout 180 "$BIN/fleetctl" watch "$SES" --until-turn-end >"$RUNTIME/watch.out" 2>"$RUNTIME/watch.err" &
WATCH=$!
sleep 1
OUT="$("$BIN/fleetctl" prompt "$SES" 'Write a very long, detailed essay about the history of distributed systems. Aim for at least 3000 words and do not stop early.')"
TURN="$(echo "$OUT" | cut -f3)"
for _ in $(seq 1 100); do
  grep -q 'delta' "$RUNTIME/watch.out" 2>/dev/null && break
  sleep 0.1
done
grep -q 'delta' "$RUNTIME/watch.out" || { echo "FAIL: streaming did not start"; exit 1; }
"$BIN/fleetctl" interrupt "$SES" "$TURN" >"$RUNTIME/interrupt.out" || { echo "FAIL: interrupt command rejected"; exit 1; }
wait "$WATCH" || true
grep -q $'turn_completed\tInterrupted' "$RUNTIME/watch.out" || {
  echo "FAIL: no interrupted terminal from agent_settled"; cat "$RUNTIME/watch.out"; exit 1;
}
[[ "$(grep -c 'turn_completed' "$RUNTIME/watch.out")" == "1" ]] || {
  echo "FAIL: contradictory terminal events"; exit 1;
}
grep -q 'user.Write a very long' "$RUNTIME/watch.out" || { echo "FAIL: prompt event missing"; exit 1; }
echo "PASS: Pi interrupt produced one Interrupted terminal via agent_settled"
