#!/usr/bin/env bash
# P4.1-G: SIGKILL real de fleetd con un turno Codex en curso; el runner y el
# App Server siguen vivos, generan eventos en el spool y el replay es durable
# sin duplicar el prompt. Requiere GF_E2E_REAL=1 (llamada real al modelo).
set -uo pipefail

if [[ "${GF_E2E_REAL:-}" != "1" ]]; then
  echo "SKIP: set GF_E2E_REAL=1 to run real Codex survival smoke"
  exit 0
fi

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
BIN="$ROOT/target/debug"
RUNTIME="$(mktemp -d /tmp/gf-p4-codex-survival-XXXXXX)"
WORKSPACE="$(mktemp -d /tmp/gf-p4-codex-survival-ws-XXXXXX)"

cleanup() {
  [[ -n "${DAEMON_PID:-}" ]] && kill "$DAEMON_PID" 2>/dev/null || true
  [[ -n "${RUNNER_PID:-}" ]] && kill "$RUNNER_PID" 2>/dev/null || true
  if [[ -z "${GF_KEEP:-}" ]]; then rm -rf "$RUNTIME" "$WORKSPACE"; fi
  pkill -f "fleet-runner .* --workspace $WORKSPACE" 2>/dev/null || true
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
SID="$("$BIN/fleetctl" session create --workspace "$WS_ID" --provider codex | cut -f1)"

timeout 180 "$BIN/fleetctl" watch "$SID" --until-turn-end >"$RUNTIME/live.out" 2>"$RUNTIME/live.err" &
WATCH=$!
sleep 1
FLEET_OPERATION_KEY=p4-codex-survival-key "$BIN/fleetctl" prompt "$SID" \
  'Run `sleep 30` in a shell and then report that you are done.' >/dev/null

for _ in $(seq 1 200); do
  grep -q 'tool_started' "$RUNTIME/live.out" 2>/dev/null && break
  sleep 0.1
done
grep -q 'tool_started' "$RUNTIME/live.out" || { echo "FAIL: turn did not start"; exit 1; }

RUNNER_PID="$(pgrep -f "fleet-runner --runner-id runner-$SID" | head -1)"
[[ -n "$RUNNER_PID" ]] || { echo "FAIL: no runner process"; exit 1; }

kill -9 "$DAEMON_PID"
wait "$DAEMON_PID" 2>/dev/null || true
unset DAEMON_PID
kill -0 "$RUNNER_PID" 2>/dev/null || { echo "FAIL: runner died with daemon"; exit 1; }
echo "PASS: Codex runner and App Server alive after SIGKILL of fleetd"

sleep 4
SPOOL="$RUNTIME/fleet-runner-runner-$SID.spool.sqlite"
SPOOL_COUNT="$(sqlite3 "$SPOOL" "SELECT COUNT(*) FROM spool;" 2>/dev/null || echo 0)"
[[ "$SPOOL_COUNT" -gt 1 ]] || { echo "FAIL: no events spooled while daemon was down ($SPOOL_COUNT)"; exit 1; }
echo "PASS: $SPOOL_COUNT events spooled while daemon was down"

wait "$WATCH" 2>/dev/null || true
rm -f "$RUNTIME/fleetd.addr" "$RUNTIME/fleetd.sock"
start_daemon 2
for _ in $(seq 1 100); do
  "$BIN/fleetctl" session list | awk -F '\t' -v id="$SID" '$1==id && ($4=="Running" || $4=="Recovering") {found=1} END{exit !found}' && break
  sleep 0.1
done
for _ in $(seq 1 120); do
  "$BIN/fleetctl" timeline "$SID" --since 0 --limit 200 >"$RUNTIME/timeline"
  grep -q 'turn_completed' "$RUNTIME/timeline" && break
  sleep 1
done
grep -q 'turn_completed' "$RUNTIME/timeline" || { echo "FAIL: turn never reached a terminal event after replay"; exit 1; }
USER_MSGS="$(grep -c 'user.Run .sleep 30' "$RUNTIME/timeline" || true)"
[[ "$USER_MSGS" == "1" ]] || { echo "FAIL: prompt was duplicated ($USER_MSGS)"; exit 1; }
grep -q 'tool_started' "$RUNTIME/timeline" || { echo "FAIL: no tool event in replay"; exit 1; }
"$BIN/fleetctl" operation p4-codex-survival-key >"$RUNTIME/operation" 2>&1
grep -qi completed "$RUNTIME/operation" || {
  echo "FAIL: prompt operation is not completed after replay"; cat "$RUNTIME/operation"; exit 1;
}
echo "PASS: daemon reattached, replayed Codex spool and did not duplicate the prompt"

timeout 30 "$BIN/fleetctl" stop "$SID" >/dev/null
echo "PASS: P4.1 Codex survival"
