#!/usr/bin/env bash
# P4.4: ejecutor común de conformidad sobre los cuatro proveedores reales.
# Sin GF_E2E_REAL=1 ejecuta solo comprobaciones deterministas de contrato.
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
BIN="$ROOT/target/debug"
RUNTIME="$(mktemp -d /tmp/gf-p44-conf-XXXXXX)"
WORKSPACE="$(mktemp -d /tmp/gf-p44-conf-ws-XXXXXX)"
cleanup() {
  [[ -n "${DAEMON_PID:-}" ]] && kill "$DAEMON_PID" 2>/dev/null || true
  if [[ -z "${GF_KEEP:-}" ]]; then rm -rf "$RUNTIME" "$WORKSPACE"; fi
  pkill -f "fleet-runner .* --workspace $WORKSPACE" 2>/dev/null || true
}
trap cleanup EXIT
export FLEET_RUNTIME_DIR="$RUNTIME" FLEET_DB_PATH="$RUNTIME/fleet.sqlite"
"$BIN/fleetd" >"$RUNTIME/daemon.out" 2>"$RUNTIME/daemon.err" &
DAEMON_PID=$!
for _ in $(seq 1 50); do [[ -f "$RUNTIME/fleetd.addr" ]] && break; sleep 0.1; done
[[ -f "$RUNTIME/fleetd.addr" ]] || { echo "FAIL: daemon did not start"; exit 1; }
"$BIN/fleetctl" workspace add "$WORKSPACE" >/dev/null

REAL_FLAG=""
if [[ "${GF_E2E_REAL:-}" == "1" ]]; then REAL_FLAG="--real"; fi
if "$BIN/fleet-conformance" --providers opencode,codex,pi,gentle-shell $REAL_FLAG \
  >"$RUNTIME/report.txt" 2>"$RUNTIME/report.err"; then
  CONFORMITY_RC=0
else
  CONFORMITY_RC=$?
fi
"$BIN/fleet-conformance" --providers opencode,codex,pi,gentle-shell $REAL_FLAG --json \
  >"$RUNTIME/report.json" 2>/dev/null || true

cat "$RUNTIME/report.txt"
cat "$RUNTIME/report.err" >&2
if [[ "$CONFORMITY_RC" -ne 0 ]] || grep -q ' FAIL ' "$RUNTIME/report.txt"; then
  echo "FAIL: conformity suite reported failing checks"; exit 1
fi
if ! grep -q 'agent_registered' "$RUNTIME/report.txt"; then
  echo "FAIL: conformity report is empty"; exit 1
fi
for provider in opencode codex pi gentle-shell; do
  grep -q "$provider" "$RUNTIME/report.txt" || { echo "FAIL: missing $provider"; exit 1; }
done
python3 -c 'import json,sys; data=json.load(open(sys.argv[1])); assert data; print("json_checks="+str(len(data)))' "$RUNTIME/report.json"
echo "PASS: common conformity suite (deterministic${REAL_FLAG:+, real})"
