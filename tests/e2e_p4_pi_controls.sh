#!/usr/bin/env bash
# P4.2-C: descubrimiento y selección real de modelo/esfuerzo en Pi RPC.
set -euo pipefail
if [[ "${GF_E2E_REAL:-}" != "1" ]]; then
  echo "SKIP: set GF_E2E_REAL=1 for real Pi control smoke"
  exit 0
fi
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
BIN="$ROOT/target/debug"
RUNTIME="$(mktemp -d /tmp/gf-p4-pi-ctl-XXXXXX)"
WORKSPACE="$(mktemp -d /tmp/gf-p4-pi-ctl-ws-XXXXXX)"
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
MODELS="$(awk -F '\t' '$1=="model" {print $3}' "$RUNTIME/controls.out")"
EFFORTS="$(awk -F '\t' '$1=="effort" {print $3}' "$RUNTIME/controls.out")"
[[ -n "$MODELS" ]] || { echo "UNTESTED: Pi reports no configured model"; exit 0; }
[[ -n "$EFFORTS" ]] || { echo "FAIL: no thinking levels discovered"; exit 1; }
MODEL="$(echo "$MODELS" | cut -d, -f2)"
[[ -n "$MODEL" ]] || MODEL="$(echo "$MODELS" | cut -d, -f1)"
EFFORT="$(echo "$EFFORTS" | grep -q '^medium$' && echo medium || echo "$EFFORTS" | cut -d, -f1)"
"$BIN/fleetctl" set-control "$SES" model "$MODEL" turn >"$RUNTIME/set-model.out"
grep -qE $'^model\t.*\t(applied|deferred)$' "$RUNTIME/set-model.out" || { echo "FAIL: model control was not accepted"; cat "$RUNTIME/set-model.out"; exit 1; }
"$BIN/fleetctl" set-control "$SES" effort "$EFFORT" turn >"$RUNTIME/set-effort.out"
grep -qE $'^effort\t.*\t(applied|deferred)$' "$RUNTIME/set-effort.out" || { echo "FAIL: effort control was not accepted"; cat "$RUNTIME/set-effort.out"; exit 1; }
# La API registra la preferencia; el proveedor la aplica al siguiente turno.
timeout 180 "$BIN/fleetctl" watch "$SES" --until-turn-end >"$RUNTIME/watch.out" 2>"$RUNTIME/watch.err" &
WATCH=$!
sleep 1
"$BIN/fleetctl" prompt "$SES" 'Reply with exactly: CONTROL OK.' >/dev/null
wait "$WATCH"
grep -q $'turn_completed\tCompleted' "$RUNTIME/watch.out" || { echo "FAIL: control turn incomplete"; exit 1; }

for _ in $(seq 1 50); do
  "$BIN/fleetctl" timeline "$SES" --limit 100 >"$RUNTIME/timeline.out"
  grep -q "control model applied: $MODEL" "$RUNTIME/timeline.out" && break
  sleep 0.1
done
grep -q "control model applied: $MODEL" "$RUNTIME/timeline.out" || {
  echo "FAIL: native model application was not confirmed"; cat "$RUNTIME/timeline.out"; cat "$RUNTIME/set-model.out"; exit 1;
}
grep -q "control effort applied: $EFFORT" "$RUNTIME/timeline.out" || { echo "FAIL: native effort application was not confirmed"; exit 1; }
if "$BIN/fleetctl" set-control "$SES" effort not-a-real-level turn >"$RUNTIME/invalid.out" 2>&1; then
  echo "FAIL: invalid effort was accepted"; exit 1
fi
echo "PASS: real Pi model and effort controls discovered, applied and validated ($MODEL, $EFFORT)"
