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
# Archivo de salida aislado: nunca probar aprobaciones mediante borrado de datos.
PROBE="$WORKSPACE/approval-probe.txt"
[[ ! -e "$PROBE" ]] || { echo "FAIL: unexpected existing probe"; exit 1; }
WS="$("$BIN/fleetctl" workspace add "$WORKSPACE" | cut -f1)"
SES="$("$BIN/fleetctl" session create --workspace "$WS" --provider codex | cut -f1)"
echo "session=$SES"
timeout 180 "$BIN/fleetctl" watch "$SES" --until-turn-end >"$RUNTIME/watch.out" 2>"$RUNTIME/watch.err" &
WATCH_PID=$!
sleep 1
"$BIN/fleetctl" prompt "$SES" 'This is a sandbox permission test in a temporary workspace. Create approval-probe.txt by running the shell command: printf FLEET_APPROVAL_PROBE > approval-probe.txt. Do not just describe the command. If the read-only sandbox blocks writing, request explicit approval for the necessary permission and proceed only if the user approves. Never claim to have created the file unless it exists.'
# La decisión del modelo de solicitar autorización no es determinista.
# Salir pronto si el turno ya terminó y exponer evidencia antes de fallar.
APPR=""
for i in $(seq 1 120); do
  "$BIN/fleetctl" approvals --session "$SES" >"$RUNTIME/approvals.out"
  APPR="$(awk -F '\t' '$4 == "pending" || $4 == "delivered" {print $1; exit}' "$RUNTIME/approvals.out")"
  [[ -n "$APPR" ]] && break
  if grep -q $'turn_completed\t' "$RUNTIME/watch.out"; then
    break
  fi
  sleep 1
done
echo '--- approvals ---'
cat "$RUNTIME/approvals.out"
echo "appr=$APPR"
if [[ -z "$APPR" ]]; then
  echo "INCONCLUSIVE: Codex did not emit an approval request for this prompt."
  echo "This does not prove that Fleet dropped an approval; the model may have declined to request escalation."
  echo '--- observed events ---'; cat "$RUNTIME/watch.out"
  echo '--- timeline ---'; "$BIN/fleetctl" timeline "$SES" --limit 150 || true
  echo '--- daemon diagnostics ---'; tail -40 "$RUNTIME/daemon.err"
  exit 1
fi
"$BIN/fleetctl" approval "$APPR"
if ! awk -F '\t' -v id="$APPR" '$1 == id {n=split($5, opts, ","); for (i=1;i<=n;i++) if (opts[i]=="accept") found=1} END {exit !found}' "$RUNTIME/approvals.out"; then
  echo "FAIL: Codex did not offer the accept option; refusing to invent a decision."
  exit 1
fi
echo '--- answering explicit accept for isolated test file ---'
"$BIN/fleetctl" answer "$APPR" accept >"$RUNTIME/answer.out"
wait "$WATCH_PID"
echo '--- watch stdout ---'; cat "$RUNTIME/watch.out"
grep -qF 'approval_requested' "$RUNTIME/watch.out" || { echo "FAIL: no native approval was requested"; exit 1; }
grep -qF 'turn_completed	Completed' "$RUNTIME/watch.out" || { echo "FAIL: approval did not complete the turn"; exit 1; }
grep -q 'approval_resolved' "$RUNTIME/watch.out" || { echo "FAIL: no native approval resolution"; exit 1; }
"$BIN/fleetctl" approval "$APPR" >"$RUNTIME/approval-final.out"
grep -Eq 'accepted|Accepted|resolved' "$RUNTIME/approval-final.out" || { echo "FAIL: approval not confirmed"; cat "$RUNTIME/approval-final.out"; exit 1; }
[[ -f "$PROBE" ]] && grep -qx 'FLEET_APPROVAL_PROBE' "$PROBE" || {
  echo "FAIL: approved sandbox operation did not produce the expected isolated file"
  exit 1
}
echo '--- watch stderr ---'; cat "$RUNTIME/watch.err"
echo '--- daemon err tail ---'; tail -20 "$RUNTIME/daemon.err"
echo "RUNTIME=$RUNTIME"
