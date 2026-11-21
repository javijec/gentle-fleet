#!/usr/bin/env bash
# P3: matar el runner con SIGKILL debe producir fail-closed, no un sustituto.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
BIN="$ROOT/target/debug"
RUNTIME="$(mktemp -d /tmp/gf-p3-kill-XXXXXX)"
WORKSPACE="$(mktemp -d /tmp/gf-p3-kill-ws-XXXXXX)"

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
for _ in $(seq 1 50); do [[ -f "$RUNTIME/fleetd.addr" ]] && break; sleep 0.1; done
[[ -f "$RUNTIME/fleetd.addr" ]] || { echo "FAIL: fleetd did not start"; exit 1; }

WS_ID="$("$BIN/fleetctl" workspace add "$WORKSPACE" | cut -f1)"
SID="$("$BIN/fleetctl" session create --workspace "$WS_ID" --provider fake | cut -f1)"
RUNNER_PID="$(pgrep -f "fleet-runner --runner-id runner-$SID .* --workspace $WORKSPACE" | head -1)"
[[ -n "$RUNNER_PID" ]] || { echo "FAIL: no runner"; exit 1; }

kill -9 "$RUNNER_PID"
for _ in $(seq 1 100); do
  "$BIN/fleetctl" session list | awk -F '\t' -v id="$SID" '$1==id && $4=="NeedsReconciliation" {found=1} END{exit !found}' && break
  sleep 0.1
done
"$BIN/fleetctl" session list | awk -F '\t' -v id="$SID" '$1==id && $4=="NeedsReconciliation" {found=1} END{exit !found}' || { echo "FAIL: runner death did not mark session uncertain"; exit 1; }
echo "PASS: runner SIGKILL leaves session NeedsReconciliation"

if pgrep -f "fleet-runner --runner-id runner-$SID .* --workspace $WORKSPACE" >/dev/null; then
  echo "FAIL: a substitute runner was created"; exit 1
fi
echo "PASS: no substitute runner created"

set +e
FLEET_OPERATION_KEY=after-kill "$BIN/fleetctl" prompt "$SID" "should not run" >"$RUNTIME/prompt.out" 2>"$RUNTIME/prompt.err"
CODE=$?
set -e
[[ "$CODE" != "0" ]] || { echo "FAIL: prompt accepted after runner death"; exit 1; }
echo "PASS: uncertain session rejects new prompts"
