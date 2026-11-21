#!/usr/bin/env bash
# Prueba end-to-end real con OpenCode 2.3.2: dos clientes, eventos de
# herramienta y detección de fin de turno.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
BIN="$ROOT/target/debug"
RUNTIME="$(mktemp -d /tmp/gf-e2e-oc-XXXXXX)"
WORKSPACE="$(mktemp -d /tmp/gf-e2e-oc-ws-XXXXXX)"

cleanup() {
  [[ -n "${SESSION_ID:-}" ]] && "$BIN/fleetctl" stop "$SESSION_ID" >/dev/null 2>&1 || true
  [[ -n "${DAEMON_PID:-}" ]] && kill "$DAEMON_PID" 2>/dev/null || true
  rm -rf "$RUNTIME" "$WORKSPACE"
    pkill -f "fleet-runner .* --workspace $WORKSPACE" 2>/dev/null || true
}
trap cleanup EXIT

export FLEET_RUNTIME_DIR="$RUNTIME"
export FLEET_DB_PATH="$RUNTIME/fleet.sqlite"
"$BIN/fleetd" >"$RUNTIME/daemon.out" 2>"$RUNTIME/daemon.err" &
DAEMON_PID=$!

for _ in $(seq 1 50); do
  [[ -f "$RUNTIME/fleetd.addr" ]] && break
  sleep 0.1
done
[[ -f "$RUNTIME/fleetd.addr" ]] || { echo "FAIL: fleetd did not start"; exit 1; }

WORKSPACE_ID="$("$BIN/fleetctl" workspace add "$WORKSPACE" | cut -f1)"
SESSION_ID="$("$BIN/fleetctl" session create --workspace "$WORKSPACE_ID" --provider opencode | cut -f1)"
echo "workspace=$WORKSPACE_ID session=$SESSION_ID"

"$BIN/fleetctl" watch "$SESSION_ID" --until-turn-end >"$RUNTIME/client-a.out" 2>"$RUNTIME/client-a.err" &
CLIENT_A=$!
"$BIN/fleetctl" watch "$SESSION_ID" --until-turn-end >"$RUNTIME/client-b.out" 2>"$RUNTIME/client-b.err" &
CLIENT_B=$!

for _ in $(seq 1 50); do
  if grep -q subscribed "$RUNTIME/client-a.err" 2>/dev/null && grep -q subscribed "$RUNTIME/client-b.err" 2>/dev/null; then
    break
  fi
  sleep 0.1
done
grep -q subscribed "$RUNTIME/client-a.err"
grep -q subscribed "$RUNTIME/client-b.err"

"$BIN/fleetctl" prompt "$SESSION_ID" "Run exactly this shell command: echo gf-fleet-p1 > p1-marker.txt. Then stop."

wait "$CLIENT_A"
wait "$CLIENT_B"

echo "--- raw client timeline ---"
cat "$RUNTIME/client-a.out"
diff -u "$RUNTIME/client-a.out" "$RUNTIME/client-b.out"
grep -qF "tool_started	shell" "$RUNTIME/client-a.out" || { echo "FAIL: no tool event"; exit 1; }
grep -qF "turn_completed	Completed" "$RUNTIME/client-a.out" || { echo "FAIL: no completed turn"; exit 1; }
[[ "$(cat "$WORKSPACE/p1-marker.txt" 2>/dev/null)" == "gf-fleet-p1" ]] || { echo "FAIL: marker file missing"; exit 1; }

echo "--- client timeline ---"
cat "$RUNTIME/client-a.out"
echo "PASS: e2e real OpenCode (two clients, tool event, turn end)"
