#!/usr/bin/env bash
set -euo pipefail
if [[ "${GF_E2E_REAL:-}" != "1" ]]; then
  echo "SKIP: set GF_E2E_REAL=1 to run real Codex smoke (billable model calls)"
  exit 0
fi
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
BIN="$ROOT/target/debug"
RUNTIME="$(mktemp -d /tmp/p41-codex-disc-XXXXXX)"
WORKSPACE="$(mktemp -d /tmp/p41-codex-discws-XXXXXX)"
cleanup() {
  [[ -n "${DAEMON_PID:-}" ]] && kill "$DAEMON_PID" 2>/dev/null || true
  pkill -f "fleet-runner .* --workspace $WORKSPACE" 2>/dev/null || true
  if [[ -z "${GF_KEEP:-}" ]]; then rm -rf "$RUNTIME" "$WORKSPACE"; fi
}
trap cleanup EXIT
export FLEET_RUNTIME_DIR="$RUNTIME" FLEET_DB_PATH="$RUNTIME/fleet.sqlite"
"$BIN/fleetd" >"$RUNTIME/daemon.out" 2>"$RUNTIME/daemon.err" &
DAEMON_PID=$!
for _ in $(seq 1 50); do [[ -f "$RUNTIME/fleetd.addr" ]] && break; sleep 0.1; done
WS="$("$BIN/fleetctl" workspace add "$WORKSPACE" | cut -f1)"
SES="$("$BIN/fleetctl" session create --workspace "$WS" --provider codex | cut -f1)"
timeout 120 "$BIN/fleetctl" watch "$SES" --until-turn-end >"$RUNTIME/watch.out" 2>"$RUNTIME/watch.err" &
WATCH_PID=$!
sleep 1
"$BIN/fleetctl" prompt "$SES" 'Reply with exactly: HISTORY SEED.' >/dev/null
wait "$WATCH_PID"
echo '--- native sessions ---'
"$BIN/fleetctl" native-sessions codex > "$RUNTIME/native.out" 2>"$RUNTIME/native.err" || true
head -5 "$RUNTIME/native.out"; tail -5 "$RUNTIME/native.err"
REF="$(awk -F '\t' -v workspace="$WORKSPACE" '$2==workspace {print $1; exit}' "$RUNTIME/native.out")"
echo "ref=$REF"
[[ -n "$REF" ]] || { echo "FAIL: native session discovery did not find the test workspace"; exit 1; }
echo '--- import history into second session ---'
SES2="$("$BIN/fleetctl" session create --workspace "$WS" --provider codex | cut -f1)"
"$BIN/fleetctl" import-history "$SES2" codex "$REF" fixed-op-key >"$RUNTIME/import.out"
grep -Eq 
echo '--- daemon err tail ---'; tail -10 "$RUNTIME/daemon.err"
^imported\\t[1-9][0-9]*
echo '--- daemon err tail ---'; tail -10 "$RUNTIME/daemon.err"
 "$RUNTIME/import.out" || { cat "$RUNTIME/import.out"; exit 1; }
"$BIN/fleetctl" timeline "$SES2" --limit 200 >"$RUNTIME/timeline-first.out"
COUNT_FIRST="$(grep -c 
echo '--- daemon err tail ---'; tail -10 "$RUNTIME/daemon.err"
history\\t' "$RUNTIME/timeline-first.out" || true)"
[[ "$COUNT_FIRST" -gt 0 ]] || { echo "FAIL: no imported events"; exit 1; }
"$BIN/fleetctl" import-history "$SES2" codex "$REF" fixed-op-key >"$RUNTIME/reimport.out"
cmp -s "$RUNTIME/import.out" "$RUNTIME/reimport.out" || { echo "FAIL: idempotent import count changed"; exit 1; }
"$BIN/fleetctl" timeline "$SES2" --limit 200 >"$RUNTIME/timeline-second.out"
[[ "$(grep -c 
echo '--- daemon err tail ---'; tail -10 "$RUNTIME/daemon.err"
history\\t' "$RUNTIME/timeline-second.out" || true)" == "$COUNT_FIRST" ]] || { echo "FAIL: duplicate history after replay"; exit 1; }
echo '--- daemon err tail ---'; tail -10 "$RUNTIME/daemon.err"
