#!/usr/bin/env bash
# P4.4-H: inyecta una credencial ficticia en un error nativo y comprueba que
# ninguna ruta pública (timeline, stderr) la expone.
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
BIN="$ROOT/target/debug"
SECRET='sk-FAKE-E2E-SECRET-1234567890'
RUNTIME="$(mktemp -d /tmp/gf-p44-redact-XXXXXX)"
WORKSPACE="$(mktemp -d /tmp/gf-p44-redact-ws-XXXXXX)"
cleanup() {
  [[ -n "${DAEMON_PID:-}" ]] && kill "$DAEMON_PID" 2>/dev/null || true
  if [[ -z "${GF_KEEP:-}" ]]; then rm -rf "$RUNTIME" "$WORKSPACE"; fi
  pkill -f "fleet-runner .* --workspace $WORKSPACE" 2>/dev/null || true
}
trap cleanup EXIT
export FLEET_RUNTIME_DIR="$RUNTIME" FLEET_DB_PATH="$RUNTIME/fleet.sqlite"
export FLEET_FAKE_FAIL_SEND=1 FLEET_FAKE_ERROR_SECRET="$SECRET"
"$BIN/fleetd" >"$RUNTIME/daemon.out" 2>"$RUNTIME/daemon.err" &
DAEMON_PID=$!
for _ in $(seq 1 50); do [[ -f "$RUNTIME/fleetd.addr" ]] && break; sleep 0.1; done
WS="$("$BIN/fleetctl" workspace add "$WORKSPACE" | cut -f1)"
SES="$("$BIN/fleetctl" session create --workspace "$WS" --provider fake | cut -f1)"
"$BIN/fleetctl" prompt "$SES" 'inject redaction probe' >"$RUNTIME/prompt.out" 2>&1 || true
sleep 1
"$BIN/fleetctl" timeline "$SES" --limit 200 >"$RUNTIME/timeline.out"
grep -q 'agent_error' "$RUNTIME/timeline.out" || { echo "FAIL: redacted error event missing"; exit 1; }
if grep -q "$SECRET" "$RUNTIME/timeline.out" "$RUNTIME/prompt.out" "$RUNTIME/daemon.err" "$RUNTIME"/*.err 2>/dev/null; then
  echo "FAIL: injected secret leaked into a public or diagnostic channel"; exit 1
fi
grep -q 'credential redacted' "$RUNTIME/timeline.out" || { echo "FAIL: no redaction marker in public event"; exit 1; }
echo "PASS: injected credential redacted on every public path"
