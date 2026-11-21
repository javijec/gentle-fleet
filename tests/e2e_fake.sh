#!/usr/bin/env bash
# Prueba negra de proceso con FakeDriver: dos clientes ven la misma secuencia.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
BIN="$ROOT/target/debug"
RUNTIME="$(mktemp -d /tmp/gf-e2e-fake-XXXXXX)"
WORKSPACE="$(mktemp -d /tmp/gf-e2e-ws-XXXXXX)"

cleanup() {
  [[ -n "${DAEMON_PID:-}" ]] && kill "$DAEMON_PID" 2>/dev/null || true
  if [[ -z "${GF_KEEP:-}" ]]; then rm -rf "$RUNTIME" "$WORKSPACE"; fi
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
SESSION_ID="$("$BIN/fleetctl" session create --workspace "$WORKSPACE_ID" --provider fake | cut -f1)"
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

"$BIN/fleetctl" prompt "$SESSION_ID" "hello from e2e"

wait "$CLIENT_A"
wait "$CLIENT_B"

diff -u "$RUNTIME/client-a.out" "$RUNTIME/client-b.out"
for expected in "assistant	Hello from fake" "tool_completed	fake_echo	true" "turn_completed	Completed"; do
  grep -qF "$expected" "$RUNTIME/client-a.out" || { echo "FAIL: missing $expected"; exit 1; }
done

echo "--- client timeline ---"
cat "$RUNTIME/client-a.out"
"$BIN/fleetctl" stop "$SESSION_ID"
echo "PASS: e2e fake (two clients, identical sequence)"
