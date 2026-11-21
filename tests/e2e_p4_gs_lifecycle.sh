#!/usr/bin/env bash
# P4.2: ciclo de vida Gentle Shell RPC sin modelo (no consume tokens).
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
BIN="$ROOT/target/debug"
RUNTIME="$(mktemp -d /tmp/gf-p4-gs-life-XXXXXX)"
WORKSPACE="$(mktemp -d /tmp/gf-p4-gs-life-ws-XXXXXX)"
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
"$BIN/fleetctl" agents >"$RUNTIME/agents.out"
awk -F '\t' '$1 == "gentle-shell" && $2 ~ /^4\./ && $3 == "Available" {found=1} END{exit !found}' "$RUNTIME/agents.out" || {
  echo "FAIL: pi is not registered as an independent available provider"; cat "$RUNTIME/agents.out"; exit 1;
}
WS="$("$BIN/fleetctl" workspace add "$WORKSPACE" | cut -f1)"
SES="$("$BIN/fleetctl" session create --workspace "$WS" --provider gentle-shell | cut -f1)"
[[ -n "$SES" ]] || { echo "FAIL: no Gentle Shell session created"; exit 1; }
"$BIN/fleetctl" controls "$SES" >"$RUNTIME/controls.out"
grep -q '^model' "$RUNTIME/controls.out" || { echo "FAIL: model control not discovered"; exit 1; }
grep -q '^effort' "$RUNTIME/controls.out" || { echo "FAIL: effort control not discovered"; exit 1; }
RUNNER="$(pgrep -f "fleet-runner --runner-id runner-$SES" | head -1)"
[[ -n "$RUNNER" ]] || { echo "FAIL: no Gentle Shell runner process"; exit 1; }
kill -0 "$RUNNER" || { echo "FAIL: Gentle Shell runner died"; exit 1; }
"$BIN/fleetctl" stop "$SES" | grep -q stopped || { echo "FAIL: stop was not confirmed"; exit 1; }
echo "PASS: Gentle Shell RPC lifecycle (probe, open, controls, runner ownership, stop)"
