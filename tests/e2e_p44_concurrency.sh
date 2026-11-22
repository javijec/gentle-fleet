#!/usr/bin/env bash
# P4.4-H: aislamiento bidireccional entre todos los pares de proveedores.
# Cada dirección mata el runner A y exige que B siga vivo, consistente y capaz
# de completar un turno NUEVO, sin reutilizar eventos de timelines anteriores.
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
[[ -f "$RUNTIME/fleetd.addr" ]] || { echo "FAIL: daemon did not start"; exit 1; }
runner_gone() {
  local pid="$1"
  if ! kill -0 "$pid" 2>/dev/null; then return 0; fi
  # Un zombie ya no ejecuta código: fleetd todavía no lo ha reapeado.
  if [[ -r "/proc/$pid/stat" ]] && grep -qE '^[^)]*\) Z ' "/proc/$pid/stat"; then return 0; fi
  return 1
}
WS="$("$BIN/fleetctl" workspace add "$WORKSPACE" | cut -f1)"

DIRECTIONS=(
  "opencode:codex" "codex:opencode"
  "opencode:pi" "pi:opencode"
  "opencode:gentle-shell" "gentle-shell:opencode"
  "codex:pi" "pi:codex"
  "codex:gentle-shell" "gentle-shell:codex"
  "pi:gentle-shell" "gentle-shell:pi"
)
if [[ "${GF_E2E_FAKE_ONLY:-}" == "1" ]]; then DIRECTIONS=("fake:fake"); fi
EXPECTED="${#DIRECTIONS[@]}"
EXECUTED=0
for direction in "${DIRECTIONS[@]}"; do
  A="${direction%%:*}"; B="${direction##*:}"
  SA="$("$BIN/fleetctl" session create --workspace "$WS" --provider "$A" | cut -f1)" || { echo "FAIL: create $A"; exit 1; }
  SB="$("$BIN/fleetctl" session create --workspace "$WS" --provider "$B" | cut -f1)" || { echo "FAIL: create $B"; exit 1; }
  [[ -n "$SA" && -n "$SB" && "$SA" != "$SB" ]] || { echo "FAIL: sessions not distinct"; exit 1; }
  RA="$(pgrep -f "fleet-runner --runner-id runner-$SA .* --workspace $WORKSPACE" | head -1 || :)"
  RB="$(pgrep -f "fleet-runner --runner-id runner-$SB .* --workspace $WORKSPACE" | head -1 || :)"
  [[ -n "$RA" && -n "$RB" && "$RA" != "$RB" ]] || { echo "FAIL: runners not distinct for $direction"; exit 1; }

  kill -9 "$RA"
  for _ in $(seq 1 40); do runner_gone "$RA" && break; sleep 0.1; done
  runner_gone "$RA" || { echo "FAIL: victim runner $A survived SIGKILL"; exit 1; }
  kill -0 "$RB" 2>/dev/null || { echo "FAIL: $B runner died after $A failure"; exit 1; }

  for _ in $(seq 1 60); do
    "$BIN/fleetctl" session list >"$RUNTIME/sessions-$EXECUTED.out"
    awk -F '\t' -v sid="$SA" '$1 == sid && $4 != "Running" {ok=1} END {exit !ok}' "$RUNTIME/sessions-$EXECUTED.out" && break
    sleep 0.1
  done
  awk -F '\t' -v sid="$SB" '$1 == sid && $4 == "Running" {ok=1} END {exit !ok}' "$RUNTIME/sessions-$EXECUTED.out" || {
    echo "FAIL: survivor session $B not Running"; exit 1;
  }
  awk -F '\t' -v sid="$SA" '$1 == sid && $4 != "Running" {ok=1} END {exit !ok}' "$RUNTIME/sessions-$EXECUTED.out" || {
    echo "FAIL: terminated provider $A still reported Running without evidence"; exit 1;
  }

  "$BIN/fleetctl" timeline "$SB" --limit 256 >"$RUNTIME/base-$EXECUTED.out"
  awk -F '\t' 'NR==1 && $1!=1 {exit 1} NR>1 && $1!=prev+1 {exit 1} {prev=$1} END {exit !(NR>0)}' "$RUNTIME/base-$EXECUTED.out" || {
    echo "FAIL: survivor timeline not contiguous"; exit 1;
  }
  BASELINE="$(tail -1 "$RUNTIME/base-$EXECUTED.out" | cut -f1)"

  if [[ "${GF_E2E_FAKE_ONLY:-}" == "1" || "${GF_E2E_REAL:-}" == "1" ]]; then
    PROMPT_OUT="$("$BIN/fleetctl" prompt "$SB" "Reply with exactly: SURVIVOR $direction")"
    DONE=0
    for _ in $(seq 1 320); do
      "$BIN/fleetctl" timeline "$SB" --since "$BASELINE" --limit 256 >"$RUNTIME/new-$EXECUTED.out"
      if grep -qF "turn_completed	Completed" "$RUNTIME/new-$EXECUTED.out"; then DONE=1; break; fi
      sleep 0.25
    done
    [[ "$DONE" == "1" ]] || { echo "FAIL: survivor $B did not complete a NEW turn"; cat "$RUNTIME/new-$EXECUTED.out"; exit 1; }
    [[ "$(grep -c 'turn_completed' "$RUNTIME/new-$EXECUTED.out")" == "1" ]] || {
      echo "FAIL: contradictory or duplicate terminal events"; exit 1;
    }
    # El prompt es el único enviado en esta ventana: exactamente un inicio de
    # turno, un mensaje de usuario y un terminal demuestra pertenencia sin
    # reutilizar eventos de timelines anteriores.
    [[ "$(grep -c 'turn_started' "$RUNTIME/new-$EXECUTED.out")" == "1" ]] || { echo "FAIL: more than one new turn"; exit 1; }
    [[ "$(grep -c '^.*user' "$RUNTIME/new-$EXECUTED.out")" == "1" ]] || { echo "FAIL: more than one new user message"; exit 1; }
    ! grep -q 'turn_uncertain' "$RUNTIME/new-$EXECUTED.out" || { echo "FAIL: unexpected uncertain turn on survivor"; exit 1; }
  fi

  "$BIN/fleetctl" stop "$SB" >/dev/null
  EXECUTED=$((EXECUTED + 1))
  echo "PASS: isolation $A -> $B ($EXECUTED/$EXPECTED)"
done
[[ "$EXECUTED" -eq "$EXPECTED" && "$EXPECTED" -gt 0 ]] || { echo "FAIL: only $EXECUTED/$EXPECTED directions executed"; exit 1; }
echo "PASS: all $EXPECTED mandatory directions isolated"
