#!/usr/bin/env bash
# P4.4-F: auditoría de exposición de secretos e IDs nativos en la API pública.
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
BIN="$ROOT/target/debug"
RUNTIME="$(mktemp -d /tmp/gf-p44-sec-XXXXXX)"
WORKSPACE="$(mktemp -d /tmp/gf-p44-sec-ws-XXXXXX)"
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
WS="$("$BIN/fleetctl" workspace add "$WORKSPACE" | cut -f1)"

: >"$RUNTIME/public.txt"
for provider in opencode codex pi gentle-shell; do
  SES="$("$BIN/fleetctl" session create --workspace "$WS" --provider "$provider" | cut -f1)"
  "$BIN/fleetctl" timeline "$SES" --limit 200 >>"$RUNTIME/public.txt"
  "$BIN/fleetctl" approvals --session "$SES" >>"$RUNTIME/public.txt" 2>/dev/null || true
  "$BIN/fleetctl" native-sessions "$provider" >>"$RUNTIME/public.txt" 2>/dev/null || true
  "$BIN/fleetctl" stop "$SES" >/dev/null
done
"$BIN/fleetctl" agents >>"$RUNTIME/public.txt"

if grep -Eqi 'sk-[a-z0-9]{12,}|auth\.json|service\.json|/home/[a-z]+/\.codex|Bearer [A-Za-z0-9]|password[[:space:]]*[:=]' "$RUNTIME/public.txt"; then
  echo "FAIL: potential secret leaked into public API output"; grep -Ei 'sk-|auth\.json|Bearer|password' "$RUNTIME/public.txt" | head; exit 1
fi
if grep -Eq 'ses_[a-f0-9]{16,}|per_[a-z0-9_-]{12,}|exec_[a-z0-9-]{12,}|msg_[a-f0-9]{16,}' "$RUNTIME/public.txt"; then
  echo "FAIL: native opaque ID leaked into public API output"; exit 1
fi
TOKEN_FILE="$(find "$RUNTIME" -name '*.token' | head -1)"
if [[ -n "$TOKEN_FILE" ]]; then
  MODE="$(stat -c '%a' "$TOKEN_FILE")"
  [[ "$MODE" == "600" ]] || { echo "FAIL: runner token has permissive mode $MODE"; exit 1; }
fi
echo "PASS: no secrets or native IDs in public API output; token permissions safe"
