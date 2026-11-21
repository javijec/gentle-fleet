#!/usr/bin/env bash
# P3 real: turno largo de OpenCode sobrevive a SIGKILL de fleetd. Opt-in.
set -euo pipefail

if [[ "${GF_E2E_REAL:-0}" != "1" ]]; then
  echo "SKIP: real OpenCode survival smoke is opt-in (set GF_E2E_REAL=1)"
  exit 0
fi

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
BIN="$ROOT/target/debug"
RUNTIME="$(mktemp -d /tmp/gf-p3-oc-surv-XXXXXX)"
WORKSPACE="$(mktemp -d /tmp/gf-p3-oc-surv-ws-XXXXXX)"

cleanup() {
  [[ -n "${RUNNER_PID:-}" ]] && kill "$RUNNER_PID" 2>/dev/null || true
  [[ -n "${DAEMON_PID:-}" ]] && kill "$DAEMON_PID" 2>/dev/null || true
  if [[ -z "${GF_KEEP:-}" ]]; then rm -rf "$RUNTIME" "$WORKSPACE"; fi
}
trap cleanup EXIT

export FLEET_RUNTIME_DIR="$RUNTIME"
export FLEET_DB_PATH="$RUNTIME/fleet.sqlite"

start_daemon() {
  "$BIN/fleetd" >"$RUNTIME/daemon-$1.out" 2>"$RUNTIME/daemon-$1.err" &
  DAEMON_PID=$!
  for _ in $(seq 1 50); do [[ -f "$RUNTIME/fleetd.addr" ]] && return; sleep 0.1; done
  echo "FAIL: fleetd did not start"; exit 1
}

start_daemon 1
WS_ID="$("$BIN/fleetctl" workspace add "$WORKSPACE" | cut -f1)"
SID="$("$BIN/fleetctl" session create --workspace "$WS_ID" --provider opencode | cut -f1)"
"$BIN/fleetctl" watch "$SID" >"$RUNTIME/watch.out" 2>"$RUNTIME/watch.err" &
WATCH=$!
for _ in $(seq 1 50); do grep -q subscribed "$RUNTIME/watch.err" 2>/dev/null && break; sleep 0.1; done
"$BIN/fleetctl" prompt "$SID" "Run exactly this shell command: sleep 12 && echo gf-p3-real > p3-real.txt. Then stop." >/dev/null

for _ in $(seq 1 600); do
  grep -q 'tool_started.shell' "$RUNTIME/watch.out" 2>/dev/null && break
  sleep 0.25
done
grep -q 'tool_started.shell' "$RUNTIME/watch.out" || { echo "FAIL: long command did not start"; exit 1; }

RUNNER_PID="$(pgrep -f "fleet-runner --runner-id runner-$SID" | head -1)"
kill -9 "$DAEMON_PID"; wait "$DAEMON_PID" 2>/dev/null || true
unset DAEMON_PID
kill -0 "$RUNNER_PID" 2>/dev/null || { echo "FAIL: runner died with daemon"; exit 1; }
echo "PASS: OpenCode runner alive after SIGKILL of fleetd"

# Esperar a que el comando largo termine y el runner spooles el resultado.
sleep 16
[[ "$(cat "$WORKSPACE/p3-real.txt" 2>/dev/null || true)" == "gf-p3-real" ]] || { echo "FAIL: long command did not finish"; exit 1; }
SPOOL="$RUNTIME/fleet-runner-runner-$SID.spool.sqlite"
SPOOL_COUNT="$(sqlite3 "$SPOOL" "SELECT COUNT(*) FROM spool;" 2>/dev/null || echo 0)"
[[ "$SPOOL_COUNT" -gt 0 ]] || { echo "FAIL: no events spooled while daemon was down"; exit 1; }

rm -f "$RUNTIME/fleetd.addr" "$RUNTIME/fleetd.sock"
start_daemon 2
for _ in $(seq 1 100); do
  "$BIN/fleetctl" session list | awk -F '\t' -v id="$SID" '$1==id && ($4=="Running" || $4=="Recovering") {found=1} END{exit !found}' && break
  sleep 0.1
done
"$BIN/fleetctl" timeline "$SID" --since 0 --limit 100 >"$RUNTIME/timeline"
grep -q 'tool_completed' "$RUNTIME/timeline" || { echo "FAIL: tool completion not replayed"; exit 1; }
grep -q 'turn_completed' "$RUNTIME/timeline" || { echo "FAIL: terminal turn not replayed"; exit 1; }
USER_MSGS="$(grep -c 'user.Run exactly this shell command' "$RUNTIME/timeline" || true)"
[[ "$USER_MSGS" == "1" ]] || { echo "FAIL: prompt duplicated ($USER_MSGS)"; exit 1; }
echo "PASS: real OpenCode turn survived daemon death, replayed without duplicate prompt"

kill "$WATCH" 2>/dev/null || true
