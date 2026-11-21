#!/usr/bin/env bash
# P2: timeline durable, reconexión de cliente, reinicio de fleetd e idempotencia.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
BIN="$ROOT/target/debug"
RUNTIME="$(mktemp -d /tmp/gf-p2-persist-XXXXXX)"
WORKSPACE="$(mktemp -d /tmp/gf-p2-ws-XXXXXX)"

cleanup() {
  [[ -n "${RUNNER_PID:-}" ]] && kill "$RUNNER_PID" 2>/dev/null || true
  [[ -n "${DAEMON_PID:-}" ]] && kill "$DAEMON_PID" 2>/dev/null || true
  rm -rf "$RUNTIME" "$WORKSPACE"
}
trap cleanup EXIT

export FLEET_RUNTIME_DIR="$RUNTIME"
export FLEET_DB_PATH="$RUNTIME/fleet.sqlite"

start_daemon() {
  "$BIN/fleetd" >"$RUNTIME/daemon.out" 2>"$RUNTIME/daemon.err" &
  DAEMON_PID=$!
  for _ in $(seq 1 50); do
    [[ -f "$RUNTIME/fleetd.addr" ]] && return
    sleep 0.1
  done
  echo "FAIL: fleetd did not start"; exit 1
}

start_daemon
WS_ID="$("$BIN/fleetctl" workspace add "$WORKSPACE" | cut -f1)"
SID="$("$BIN/fleetctl" session create --workspace "$WS_ID" --provider fake | cut -f1)"
RUNNER_PID="$(pgrep -f "fleet-runner --runner-id runner-$SID" | head -1)"

"$BIN/fleetctl" watch "$SID" --until-turn-end >"$RUNTIME/live.out" 2>"$RUNTIME/live.err" &
WATCH=$!
for _ in $(seq 1 50); do grep -q subscribed "$RUNTIME/live.err" 2>/dev/null && break; sleep 0.1; done
FLEET_OPERATION_KEY=p2-durable-key "$BIN/fleetctl" prompt "$SID" "persist this" >"$RUNTIME/receipt1"
wait "$WATCH"

# Cliente "reconectado": lee la timeline durable en vez del stream vivo.
"$BIN/fleetctl" timeline "$SID" --since 0 --limit 100 >"$RUNTIME/timeline1"
cut -f1 "$RUNTIME/live.out" >"$RUNTIME/live.seq"
cut -f1 "$RUNTIME/timeline1" >"$RUNTIME/timeline1.seq"
diff -u "$RUNTIME/live.seq" "$RUNTIME/timeline1.seq"
"$BIN/fleetctl" operation p2-durable-key | grep -qi 'completed'
echo "PASS: durable timeline and GetOperation"

# Reinicio del daemon con la misma base.
kill "$DAEMON_PID"; wait "$DAEMON_PID" 2>/dev/null || true
unset DAEMON_PID
rm -f "$RUNTIME/fleetd.addr" "$RUNTIME/fleetd.sock"
start_daemon

# P3 keeps the runner alive across daemon restarts. A completed prior
# turn must reattach into Running; NeedsReconciliation is only for lost runners.
"$BIN/fleetctl" session list | awk -F '\t' -v id="$SID" '$1==id && $4=="Running" && $5=="Completed" {found=1} END{exit !found}'
"$BIN/fleetctl" timeline "$SID" --since 0 --limit 100 >"$RUNTIME/timeline2"
N="$(wc -l < "$RUNTIME/timeline1.seq")"
diff -u "$RUNTIME/timeline1.seq" <(head -n "$N" "$RUNTIME/timeline2" | cut -f1)
# Reattachment adds a durable session_state event; old history is preserved.
tail -n 1 "$RUNTIME/timeline2" | grep -q 'session_state'
FLEET_OPERATION_KEY=p2-durable-key "$BIN/fleetctl" prompt "$SID" "persist this" >"$RUNTIME/receipt2"
diff -u "$RUNTIME/receipt1" "$RUNTIME/receipt2"
# A new operation must work after the recovered previous turn.
FLEET_OPERATION_KEY=p3-after-reattach "$BIN/fleetctl" prompt "$SID" "new turn after recovery" >/dev/null
for _ in $(seq 1 100); do
  "$BIN/fleetctl" operation p3-after-reattach | grep -qi completed && break
  sleep 0.05
done
"$BIN/fleetctl" operation p3-after-reattach | grep -qi completed
echo "PASS: restart recovered live runner, preserved timeline and accepted next turn"
