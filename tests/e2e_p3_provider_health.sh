#!/usr/bin/env bash
# P3: pérdida de salud del proveedor con runner IPC vivo -> fail-closed.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
BIN="$ROOT/target/debug"
RUNTIME="$(mktemp -d /tmp/gf-p3-health-XXXXXX)"
WORKSPACE="$(mktemp -d /tmp/gf-p3-health-ws-XXXXXX)"

cleanup() {
  [[ -n "${RUNNER_PID:-}" ]] && kill "$RUNNER_PID" 2>/dev/null || true
  [[ -n "${DAEMON_PID:-}" ]] && kill "$DAEMON_PID" 2>/dev/null || true
  if [[ -z "${GF_KEEP:-}" ]]; then rm -rf "$RUNTIME" "$WORKSPACE"; fi
    pkill -f "fleet-runner .* --workspace $WORKSPACE" 2>/dev/null || true
}
trap cleanup EXIT

export FLEET_RUNTIME_DIR="$RUNTIME"
export FLEET_DB_PATH="$RUNTIME/fleet.sqlite"

"$BIN/fleetd" >"$RUNTIME/daemon.out" 2>"$RUNTIME/daemon.err" &
DAEMON_PID=$!
for _ in $(seq 1 50); do [[ -f "$RUNTIME/fleetd.addr" ]] && break; sleep 0.1; done
[[ -f "$RUNTIME/fleetd.addr" ]] || { echo "FAIL: fleetd did not start"; exit 1; }

WS_ID="$("$BIN/fleetctl" workspace add "$WORKSPACE" | cut -f1)"
SID="$("$BIN/fleetctl" session create --workspace "$WS_ID" --provider fake | cut -f1)"
RUNNER_ID="runner-$SID"
RUNNER_PID="$(pgrep -f "fleet-runner --runner-id $RUNNER_ID" | head -1)"
[[ -n "$RUNNER_PID" ]] || { echo "FAIL: no runner"; exit 1; }

FLEET_OPERATION_KEY=health-1 "$BIN/fleetctl" prompt "$SID" "first" >/dev/null
for _ in $(seq 1 100); do
  "$BIN/fleetctl" operation health-1 | grep -qi completed && break
  sleep 0.05
done
"$BIN/fleetctl" operation health-1 | grep -qi completed || { echo "FAIL: first turn did not complete"; exit 1; }

# Hook determinista: el runner evalúa este archivo en cada Status (IPC), sin
# depender del worker que puede estar bloqueado en una llamada al proveedor.
touch "$RUNTIME/fleet-runner-$RUNNER_ID.provider-dead"
for _ in $(seq 1 100); do
  "$BIN/fleetctl" session list | awk -F '\t' -v id="$SID" '$1==id && $4=="NeedsReconciliation" {found=1} END{exit !found}' && break
  sleep 0.1
done
"$BIN/fleetctl" session list | awk -F '\t' -v id="$SID" '$1==id && $4=="NeedsReconciliation" {found=1} END{exit !found}' || { echo "FAIL: health loss did not mark session NeedsReconciliation"; exit 1; }
kill -0 "$RUNNER_PID" 2>/dev/null || { echo "FAIL: runner died (should stay alive for diagnosis)"; exit 1; }
echo "PASS: health loss marks session NeedsReconciliation with runner IPC alive"

set +e
FLEET_OPERATION_KEY=health-2 "$BIN/fleetctl" prompt "$SID" "second" >"$RUNTIME/p2.out" 2>"$RUNTIME/p2.err"
CODE=$?
set -e
[[ "$CODE" != "0" ]] || { echo "FAIL: prompt accepted after health loss"; exit 1; }
grep -qiE 'session_stopped|runner|reconcil' "$RUNTIME/p2.err" || { echo "FAIL: unexpected error"; cat "$RUNTIME/p2.err"; exit 1; }
echo "PASS: uncertain session rejects new prompts"
