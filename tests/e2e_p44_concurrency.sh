#!/usr/bin/env bash
# P4.4-E: aislamiento determinista entre todos los pares de proveedores.
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
BIN="$ROOT/target/debug"
RUNTIME="$(mktemp -d /tmp/gf-p44-conc-XXXXXX)"
WORKSPACE="$(mktemp -d /tmp/gf-p44-conc-ws-XXXXXX)"
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

PAIRS=(
  "opencode:codex"
  "opencode:pi"
  "opencode:gentle-shell"
  "codex:pi"
  "codex:gentle-shell"
  "pi:gentle-shell"
)
for pair in "${PAIRS[@]}"; do
  A="${pair%%:*}"; B="${pair##*:}"
  SA="$("$BIN/fleetctl" session create --workspace "$WS" --provider "$A" | cut -f1)" || continue
  SB="$("$BIN/fleetctl" session create --workspace "$WS" --provider "$B" | cut -f1)" || continue
  [[ -n "$SA" && -n "$SB" ]] || { echo "FAIL: no sessions for $pair"; exit 1; }
  RA="$(pgrep -f "fleet-runner --runner-id runner-$SA" | head -1)"
  RB="$(pgrep -f "fleet-runner --runner-id runner-$SB" | head -1)"
  [[ -n "$RA" && -n "$RB" ]] || { echo "FAIL: missing runners for $pair"; exit 1; }
  kill -9 "$RA"
  sleep 1
  kill -0 "$RB" 2>/dev/null || { echo "FAIL: $B runner died after $A failure"; exit 1; }
  "$BIN/fleetctl" session list >"$RUNTIME/sessions-$A-$B.out"
  grep -q "$SB" "$RUNTIME/sessions-$A-$B.out" || { echo "FAIL: $B session vanished"; exit 1; }
  "$BIN/fleetctl" stop "$SB" >/dev/null 2>&1 || true
  echo "PASS: isolation $A -> $B"
done
echo "PASS: all provider pairs isolated"
