#!/usr/bin/env bash
# P1.1: cliente lento desconectado con aviso explícito, sin pérdida silenciosa.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
BIN="$ROOT/target/debug"
RUNTIME="$(mktemp -d /tmp/gf-p11-bp-XXXXXX)"
WORKSPACE="$(mktemp -d /tmp/gf-p11-bp-ws-XXXXXX)"

cleanup() {
  [[ -n "${DAEMON_PID:-}" ]] && kill "$DAEMON_PID" 2>/dev/null || true
  rm -rf "$RUNTIME" "$WORKSPACE"
}
trap cleanup EXIT

export FLEET_RUNTIME_DIR="$RUNTIME"
export FLEET_DB_PATH="$RUNTIME/fleet.sqlite"
export FLEET_SUBSCRIBER_LIMIT=1
export FLEET_TEST_FORWARD_DELAY_MS=250
"$BIN/fleetd" >"$RUNTIME/daemon.out" 2>"$RUNTIME/daemon.err" &
DAEMON_PID=$!
for _ in $(seq 1 50); do
  [[ -f "$RUNTIME/fleetd.addr" ]] && break
  sleep 0.1
done
[[ -f "$RUNTIME/fleetd.addr" ]] || { echo "FAIL: fleetd did not start"; exit 1; }

WORKSPACE_ID="$("$BIN/fleetctl" workspace add "$WORKSPACE" | cut -f1)"
SESSION_ID="$("$BIN/fleetctl" session create --workspace "$WORKSPACE_ID" --provider fake | cut -f1)"

set +e
timeout 15s "$BIN/fleetctl" watch "$SESSION_ID" --count 50 >"$RUNTIME/watch.out" 2>"$RUNTIME/watch.err" &
WATCH_PID=$!
for _ in $(seq 1 50); do grep -q subscribed "$RUNTIME/watch.err" 2>/dev/null && break; sleep 0.1; done
"$BIN/fleetctl" prompt "$SESSION_ID" "flood" >/dev/null
wait "$WATCH_PID"
WATCH_CODE=$?
set -e

if ! grep -q "Backpressure" "$RUNTIME/watch.err"; then
  echo "FAIL: no explicit backpressure notice"; cat "$RUNTIME/watch.err"; exit 1
fi
if (( WATCH_CODE == 0 )); then
  echo "FAIL: slow client was not disconnected"; exit 1
fi
if ! grep -q "driver did not acknowledge" "$RUNTIME/daemon.err" 2>/dev/null; then
  : # el driver normal sí reconoce el cierre; no es requisito aquí
fi
echo "PASS: slow client disconnected explicitly with Backpressure"
