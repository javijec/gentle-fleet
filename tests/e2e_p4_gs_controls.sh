#!/usr/bin/env bash
# P4.3-H: verifica la aplicación NATIVA de controles Gentle Shell, no solo el
# estado deferred de Fleet. Opt-in: puede consumir tokens del modelo configurado.
set -euo pipefail
if [[ "${GF_E2E_REAL:-}" != "1" ]]; then
  echo "SKIP: set GF_E2E_REAL=1 to run real Gentle Shell control verification"
  exit 0
fi
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
BIN="$ROOT/target/debug"
RUNTIME="$(mktemp -d /tmp/gf-p43-gs-controls-XXXXXX)"
WORKSPACE="$(mktemp -d /tmp/gf-p43-gs-controls-ws-XXXXXX)"
cleanup() {
  [[ -n "${DAEMON_PID:-}" ]] && kill "$DAEMON_PID" 2>/dev/null || true
  [[ -z "${GF_KEEP:-}" ]] && rm -rf "$RUNTIME" "$WORKSPACE"
  pkill -f "fleet-runner .* --workspace $WORKSPACE" 2>/dev/null || true
}
trap cleanup EXIT
export FLEET_RUNTIME_DIR="$RUNTIME" FLEET_DB_PATH="$RUNTIME/fleet.sqlite"
"$BIN/fleetd" >"$RUNTIME/daemon.out" 2>"$RUNTIME/daemon.err" &
DAEMON_PID=$!
for _ in $(seq 1 50); do [[ -f "$RUNTIME/fleetd.addr" ]] && break; sleep 0.1; done
[[ -f "$RUNTIME/fleetd.addr" ]] || { echo "FAIL: no daemon"; exit 1; }
WS="$("$BIN/fleetctl" workspace add "$WORKSPACE" | cut -f1)"
SES="$("$BIN/fleetctl" session create --workspace "$WS" --provider gentle-shell | cut -f1)"
"$BIN/fleetctl" controls "$SES" >"$RUNTIME/before.out"

choose_native_control() {
  local name="$1" row current available
  row="$(awk -F '\t' -v key="$name" '$1 == key {print; exit}' "$RUNTIME/before.out")"
  [[ -n "$row" ]] || return 1
  current="$(printf '%s\n' "$row" | cut -f2)"
  available="$(printf '%s\n' "$row" | cut -f3)"
  [[ -n "$available" ]] || return 1
  if [[ -n "$current" && "$current" != "unknown" ]]; then
    printf '%s\n' "$current"
  else
    printf '%s\n' "${available%%,*}"
  fi
}
MODEL="$(choose_native_control model || true)"
EFFORT="$(choose_native_control effort || true)"
if [[ -z "$MODEL" || -z "$EFFORT" ]]; then
  echo "UNTESTED: no configured model/effort exposed by Gentle Shell"
  exit 0
fi

# Permission modes are read-only until home/profile isolation is provable.
if awk -F '\t' '$1 == "permission_mode" {found=1} END {exit !found}' "$RUNTIME/before.out"; then
  if "$BIN/fleetctl" set-control "$SES" permission_mode normal session >"$RUNTIME/mode.out" 2>&1; then
    echo "FAIL: read-only permission_mode was accepted"; cat "$RUNTIME/mode.out"; exit 1
  fi
fi
"$BIN/fleetctl" set-control "$SES" model "$MODEL" turn >"$RUNTIME/set-model.out"
"$BIN/fleetctl" set-control "$SES" effort "$EFFORT" turn >"$RUNTIME/set-effort.out"
grep -q $'^model\t.*\tdeferred$' "$RUNTIME/set-model.out" || { echo "FAIL: model was not deferred"; exit 1; }
grep -q $'^effort\t.*\tdeferred$' "$RUNTIME/set-effort.out" || { echo "FAIL: effort was not deferred"; exit 1; }

timeout 180 "$BIN/fleetctl" watch "$SES" --until-turn-end >"$RUNTIME/watch.out" 2>"$RUNTIME/watch.err" &
WATCH=$!
sleep 1
"$BIN/fleetctl" prompt "$SES" 'Reply with exactly: P4 GS CONTROL OK.' >/dev/null
wait "$WATCH"
grep -q $'turn_completed\tCompleted' "$RUNTIME/watch.out" || {
  echo "FAIL: turn did not finish"; cat "$RUNTIME/watch.out"; exit 1;
}
"$BIN/fleetctl" timeline "$SES" --limit 256 >"$RUNTIME/timeline.out"
grep -Fq "control model applied: $MODEL" "$RUNTIME/timeline.out" || {
  echo "FAIL: model not confirmed by native RPC"; cat "$RUNTIME/timeline.out"; exit 1;
}
grep -Fq "control effort applied: $EFFORT" "$RUNTIME/timeline.out" || {
  echo "FAIL: effort not confirmed by native RPC"; cat "$RUNTIME/timeline.out"; exit 1;
}
echo "PASS: Gentle Shell native model and effort applied; permission mode remained read-only"
