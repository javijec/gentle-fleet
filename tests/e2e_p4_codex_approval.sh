#!/usr/bin/env bash
set -euo pipefail
if [[ "${GF_E2E_REAL:-}" != "1" ]]; then
  echo "SKIP: set GF_E2E_REAL=1 to run real Codex smoke (billable model calls)"
  exit 0
fi
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
BIN="$ROOT/target/debug"
RUNTIME="$(mktemp -d /tmp/p41-codex-appr-XXXXXX)"
WORKSPACE="$(mktemp -d /tmp/p41-codex-apprws-XXXXXX)"
cleanup() {
  [[ -n "${DAEMON_PID:-}" ]] && kill "$DAEMON_PID" 2>/dev/null || true
  pkill -f "fleet-runner .* --workspace $WORKSPACE" 2>/dev/null || true
  if [[ -z "${GF_KEEP:-}" ]]; then rm -rf "$RUNTIME" "$WORKSPACE"; fi
}
trap cleanup EXIT
export FLEET_CODEX_SANDBOX=read-only FLEET_CODEX_APPROVAL_POLICY=on-request
export FLEET_RUNTIME_DIR="$RUNTIME" FLEET_DB_PATH="$RUNTIME/fleet.sqlite"
"$BIN/fleetd" >"$RUNTIME/daemon.out" 2>"$RUNTIME/daemon.err" &
DAEMON_PID=$!
for _ in $(seq 1 50); do [[ -f "$RUNTIME/fleetd.addr" ]] && break; sleep 0.1; done
mkdir -p "$WORKSPACE/victimdir" && echo x > "$WORKSPACE/victimdir/file.txt"
WS="$("$BIN/fleetctl" workspace add "$WORKSPACE" | cut -f1)"
SES="$("$BIN/fleetctl" session create --workspace "$WS" --provider codex | cut -f1)"
echo "session=$SES"
timeout 180 "$BIN/fleetctl" watch "$SES" --until-turn-end >"$RUNTIME/watch.out" 2>"$RUNTIME/watch.err" &
WATCH_PID=$!
sleep 1
"$BIN/fleetctl" prompt "$SES" 'Remove the test directory by running `rm -r victimdir` in the workspace.'
for i in $(seq 1 120); do
  if "$BIN/fleetctl" approvals --session "$SES" 2>/dev/null | grep -q appr_; then break; fi
  sleep 1
done
echo '--- approvals ---'
"$BIN/fleetctl" approvals --session "$SES" || true
echo '--- approval detail (first) ---'
APPR="$("$BIN/fleetctl" approvals --session "$SES" 2>/dev/null | grep -o 'appr_[[:alnum:]]*' | head -1 || true)"
echo "appr=$APPR"
[[ -n "$APPR" ]] || { echo "FAIL: no approval id"; exit 1; }
"$BIN/fleetctl" approval "$APPR"
echo '--- answering accept ---'
"$BIN/fleetctl" answer "$APPR" accept >"$RUNTIME/answer.out"
wait "$WATCH_PID"
echo '--- watch stdout ---'; cat "$RUNTIME/watch.out"
grep -qF 'approval_requested' "$RUNTIME/watch.out" || { echo "FAIL: no native approval was requested"; exit 1; }
grep -qF 'turn_completed	Completed' "$RUNTIME/watch.out" || { echo "FAIL: approval did not complete the turn"; exit 1; }
grep -q 'approval_resolved' "$RUNTIME/watch.out" || { echo "FAIL: no native approval resolution"; exit 1; }
"$BIN/fleetctl" approval "$APPR" >"$RUNTIME/approval-final.out"
grep -Eq 'accepted|Accepted|resolved' "$RUNTIME/approval-final.out" || { echo "FAIL: approval not confirmed"; cat "$RUNTIME/approval-final.out"; exit 1; }
echo '--- watch stderr ---'; cat "$RUNTIME/watch.err"
echo '--- daemon err tail ---'; tail -20 "$RUNTIME/daemon.err"
echo "RUNTIME=$RUNTIME"
