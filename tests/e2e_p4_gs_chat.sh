#!/usr/bin/env bash
# P4.2-B: smoke real de chat Gentle Shell. Opt-in y solo si hay un modelo configurado.
set -euo pipefail
if [[ "${GF_E2E_REAL:-}" != "1" ]]; then
  echo "SKIP: set GF_E2E_REAL=1 for a real Gentle Shell chat smoke"
  exit 0
fi
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
BIN="$ROOT/target/debug"
RUNTIME="$(mktemp -d /tmp/gf-p4-gs-chat-XXXXXX)"
WORKSPACE="$(mktemp -d /tmp/gf-p4-gs-chat-ws-XXXXXX)"
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
SES="$("$BIN/fleetctl" session create --workspace "$WS" --provider gentle-shell | cut -f1)"
"$BIN/fleetctl" controls "$SES" >"$RUNTIME/controls.out"
MODEL_LINE="$(awk -F '\t' '$1 == "model" {print; exit}' "$RUNTIME/controls.out")"
MODELS="$(echo "$MODEL_LINE" | cut -f3)"
if [[ -z "$MODELS" ]]; then
  echo "UNTESTED: Gentle Shell $(gentle-shell --version | head -1) reports no configured model in this environment."
  echo "Fleet correctly declares the capability as UNTESTED; no credential was modified."
  exit 0
fi
timeout 180 "$BIN/fleetctl" watch "$SES" --until-turn-end >"$RUNTIME/watch.out" 2>"$RUNTIME/watch.err" &
WATCH=$!
sleep 1
"$BIN/fleetctl" prompt "$SES" 'Reply with exactly: P4 GS OK.' >/dev/null
wait "$WATCH"
grep -qF $'delta\t' "$RUNTIME/watch.out" || { echo "FAIL: no streaming deltas"; exit 1; }
grep -qF 'turn_completed	Completed' "$RUNTIME/watch.out" || { echo "FAIL: no agent_settled completion"; exit 1; }
grep -qi 'P4 GS OK' "$RUNTIME/watch.out" || { echo "FAIL: assistant response missing"; exit 1; }
echo "PASS: real Gentle Shell chat through Fleet API"
