#!/usr/bin/env bash
# P4.1-G: sesiones OpenCode y Codex concurrentes bajo el mismo fleetd.
# Requiere GF_E2E_REAL=1 para la parte Codex (OpenCode es local y determinista).
set -uo pipefail
if [[ "${GF_E2E_REAL:-}" != "1" ]]; then
  echo "SKIP: set GF_E2E_REAL=1 to run real Codex + OpenCode concurrency smoke"
  exit 0
fi
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
BIN="$ROOT/target/debug"
RUNTIME="$(mktemp -d /tmp/gf-p4-concurrency-XXXXXX)"
WORKSPACE="$(mktemp -d /tmp/gf-p4-concurrency-ws-XXXXXX)"
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
OC="$("$BIN/fleetctl" session create --workspace "$WS" --provider opencode | cut -f1)"
CX="$("$BIN/fleetctl" session create --workspace "$WS" --provider codex | cut -f1)"
timeout 180 "$BIN/fleetctl" watch "$OC" --until-turn-end >"$RUNTIME/oc.out" 2>"$RUNTIME/oc.err" &
W1=$!
timeout 180 "$BIN/fleetctl" watch "$CX" --until-turn-end >"$RUNTIME/cx.out" 2>"$RUNTIME/cx.err" &
W2=$!
sleep 1
"$BIN/fleetctl" prompt "$OC" 'Reply with exactly: OPENCODE OK.' >/dev/null
"$BIN/fleetctl" prompt "$CX" 'Reply with exactly: CODEX OK.' >/dev/null
wait "$W1" || { echo "FAIL: OpenCode turn"; tail -10 "$RUNTIME/oc.err"; exit 1; }
wait "$W2" || { echo "FAIL: Codex turn"; tail -10 "$RUNTIME/cx.err"; exit 1; }
grep -q 'turn_completed' "$RUNTIME/oc.out" || { echo "FAIL: OpenCode no terminal"; exit 1; }
grep -q 'turn_completed' "$RUNTIME/cx.out" || { echo "FAIL: Codex no terminal"; exit 1; }
# Ninguna sesión debe contaminar la timeline de la otra.
! grep -q 'CODEX OK' "$RUNTIME/oc.out" || { echo "FAIL: cross-provider leak"; exit 1; }
! grep -q 'OPENCODE OK' "$RUNTIME/cx.out" || { echo "FAIL: cross-provider leak"; exit 1; }
echo "PASS: concurrent OpenCode and Codex sessions"
