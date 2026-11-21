#!/usr/bin/env bash
# P3: SIGKILL real de fleetd con un turno en curso; runner y proveedor siguen.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
BIN="$ROOT/target/debug"
RUNTIME="$(mktemp -d /tmp/gf-p3-survival-XXXXXX)"
WORKSPACE="$(mktemp -d /tmp/gf-p3-survival-ws-XXXXXX)"
SLOW_MS=4000

cleanup() {
  [[ -n "${RUNNER_PID:-}" ]] && kill "$RUNNER_PID" 2>/dev/null || true
  [[ -n "${DAEMON_PID:-}" ]] && kill "$DAEMON_PID" 2>/dev/null || true
  if [[ -z "${GF_KEEP:-}" ]]; then rm -rf "$RUNTIME" "$WORKSPACE"; fi
}
trap cleanup EXIT

export FLEET_RUNTIME_DIR="$RUNTIME"
export FLEET_DB_PATH="$RUNTIME/fleet.sqlite"
export FLEET_FAKE_SLOW_TURN_MS="$SLOW_MS"

start_daemon() {
  "$BIN/fleetd" >"$RUNTIME/daemon-$1.out" 2>"$RUNTIME/daemon-$1.err" &
  DAEMON_PID=$!
  for _ in $(seq 1 50); do [[ -f "$RUNTIME/fleetd.addr" ]] && return; sleep 0.1; done
  echo "FAIL: fleetd did not start"; exit 1
}

start_daemon 1
WS_ID="$("$BIN/fleetctl" workspace add "$WORKSPACE" | cut -f1)"
SID="$("$BIN/fleetctl" session create --workspace "$WS_ID" --provider fake | cut -f1)"

"$BIN/fleetctl" watch "$SID" --until-turn-end >"$RUNTIME/live.out" 2>"$RUNTIME/live.err" &
WATCH=$!
for _ in $(seq 1 50); do grep -q subscribed "$RUNTIME/live.err" 2>/dev/null && break; sleep 0.1; done
FLEET_OPERATION_KEY=p3-survival-key "$BIN/fleetctl" prompt "$SID" "survive daemon death" >/dev/null

for _ in $(seq 1 100); do
  grep -q 'delta.Hello' "$RUNTIME/live.out" 2>/dev/null && break
  sleep 0.05
done
grep -q 'delta.Hello' "$RUNTIME/live.out" || { echo "FAIL: turn did not start"; exit 1; }

RUNNER_PID="$(pgrep -f "fleet-runner --runner-id runner-$SID" | head -1)"
[[ -n "$RUNNER_PID" ]] || { echo "FAIL: no runner process"; exit 1; }

kill -9 "$DAEMON_PID"
wait "$DAEMON_PID" 2>/dev/null || true
unset DAEMON_PID
kill -0 "$RUNNER_PID" 2>/dev/null || { echo "FAIL: runner died with daemon"; exit 1; }
echo "PASS: runner alive after SIGKILL of fleetd"

# El turno sigue produciendo eventos mientras el daemon está caído.
sleep $((SLOW_MS / 1000 + 2))
SPOOL="$RUNTIME/fleet-runner-runner-$SID.spool.sqlite"
SPOOL_COUNT="$(sqlite3 "$SPOOL" "SELECT COUNT(*) FROM spool;" 2>/dev/null || echo 0)"
[[ "$SPOOL_COUNT" -gt 1 ]] || { echo "FAIL: no events generated while daemon was down ($SPOOL_COUNT)"; exit 1; }
echo "PASS: $SPOOL_COUNT events spooled while daemon was down"

rm -f "$RUNTIME/fleetd.addr" "$RUNTIME/fleetd.sock"
start_daemon 2
for _ in $(seq 1 100); do
  "$BIN/fleetctl" session list | awk -F '\t' -v id="$SID" '$1==id && ($4=="Running" || $4=="Recovering") {found=1} END{exit !found}' && break
  sleep 0.1
done
"$BIN/fleetctl" session list | awk -F '\t' -v id="$SID" '$1==id && ($4=="Running" || $4=="Recovering") {found=1} END{exit !found}'

"$BIN/fleetctl" timeline "$SID" --since 0 --limit 100 >"$RUNTIME/timeline"
for expected in 'delta.Hello' 'delta.from fake' 'tool_completed' 'turn_completed.Completed'; do
  grep -q "$expected" "$RUNTIME/timeline" || { echo "FAIL: missing $expected in replay"; exit 1; }
done
USER_MSGS="$(grep -c 'user.survive daemon death' "$RUNTIME/timeline" || true)"
[[ "$USER_MSGS" == "1" ]] || { echo "FAIL: prompt was duplicated ($USER_MSGS)"; exit 1; }
"$BIN/fleetctl" operation p3-survival-key | grep -qi completed
"$BIN/fleetctl" session list | awk -F '\t' -v id="$SID" '$1==id && $4=="Running" && $5=="Completed" {found=1} END{exit !found}'
echo "PASS: daemon reattached, replayed spool, and returned to Running"

timeout 30 "$BIN/fleetctl" watch "$SID" --since 0 --until-turn-end >"$RUNTIME/replay.out" 2>"$RUNTIME/replay.err"
# --until-turn-end correctly stops at TurnCompleted. Recovery emits a
# *subsequent* SessionStateChanged(Running) event in the durable timeline.
REPLAY_LINES="$(wc -l <"$RUNTIME/replay.out")"
diff -u <(head -n "$REPLAY_LINES" "$RUNTIME/timeline" | cut -f1) <(cut -f1 "$RUNTIME/replay.out")
tail -n 1 "$RUNTIME/replay.out" | grep -q 'turn_completed.Completed'
tail -n 1 "$RUNTIME/timeline" | grep -q 'session_state.Running'
echo "PASS: terminal watch is contiguous and subsequent recovery state is durable"
