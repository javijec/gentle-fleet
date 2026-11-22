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
if [[ "${GF_E2E_FAKE_ONLY:-}" == "1" ]]; then
  PAIRS=("fake:fake")
fi
EXPECTED="${#PAIRS[@]}"
EXECUTED=0
for pair in "${PAIRS[@]}"; do
  A="${pair%%:*}"; B="${pair##*:}"
  SA="$("$BIN/fleetctl" session create --workspace "$WS" --provider "$A" | cut -f1)" || { echo "FAIL: cannot create $A in $pair"; exit 1; }
  SB="$("$BIN/fleetctl" session create --workspace "$WS" --provider "$B" | cut -f1)" || { echo "FAIL: cannot create $B in $pair"; exit 1; }
  [[ -n "$SA" && -n "$SB" ]] || { echo "FAIL: no sessions for $pair"; exit 1; }
  RA="$(pgrep -f "fleet-runner --runner-id runner-$SA " | head -1 || :)"
  RB="$(pgrep -f "fleet-runner --runner-id runner-$SB " | head -1 || :)"
  [[ -n "$RA" && -n "$RB" && "$RA" != "$RB" ]] || { echo "FAIL: missing or identical runners for $pair"; exit 1; }
  kill -9 "$RA"
  sleep 1
  kill -0 "$RB" 2>/dev/null || { echo "FAIL: $B runner died after $A failure"; exit 1; }
  "$BIN/fleetctl" session list >"$RUNTIME/sessions-$A-$B.out"
  awk -F '\t' -v sid="$SB" '$1 == sid && $4 == "Running" {ok=1} END {exit !ok}' "$RUNTIME/sessions-$A-$B.out" || { echo "FAIL: $B session not running"; exit 1; }
  "$BIN/fleetctl" timeline "$SB" --limit 256 >"$RUNTIME/timeline-$A-$B.out"
  grep -q "session_created" "$RUNTIME/timeline-$A-$B.out" || { echo "FAIL: survivor timeline unreadable"; exit 1; }
  # Confirmar una conversación nueva en FakeDriver (CI) y en modo real opt-in.
  if [[ "${GF_E2E_FAKE_ONLY:-}" == "1" || "${GF_E2E_REAL:-}" == "1" ]]; then
    "$BIN/fleetctl" prompt "$SB" "Reply with exactly: SURVIVOR $pair" >/dev/null
    DONE=0
    for _ in $(seq 1 120); do
      "$BIN/fleetctl" timeline "$SB" --limit 256 >"$RUNTIME/timeline-$A-$B.out"
      if grep -Eqi 'turn_completed.*Completed' "$RUNTIME/timeline-$A-$B.out"; then DONE=1; break; fi
      sleep 0.25
    done
    [[ "$DONE" == "1" ]] || { echo "FAIL: survivor $B could not complete next turn"; exit 1; }
  fi
  "$BIN/fleetctl" stop "$SB" >/dev/null
  EXECUTED=$((EXECUTED + 1))
  echo "PASS: isolation $A -> $B ($EXECUTED/$EXPECTED)"
done
[[ "$EXECUTED" -eq "$EXPECTED" && "$EXPECTED" -gt 0 ]] || { echo "FAIL: only $EXECUTED/$EXPECTED pairs executed"; exit 1; }
echo "PASS: all $EXPECTED mandatory pairs isolated"
