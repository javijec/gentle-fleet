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
TOKEN_COUNT=0
PROVIDERS=(opencode codex pi gentle-shell)
if [[ "${GF_E2E_FAKE_ONLY:-}" == "1" ]]; then PROVIDERS=(fake); fi
for provider in "${PROVIDERS[@]}"; do
  SES="$("$BIN/fleetctl" session create --workspace "$WS" --provider "$provider" | cut -f1)"
  TOKEN_FILE="$RUNTIME/fleet-runner-runner-$SES.token"
  [[ -f "$TOKEN_FILE" ]] || { echo "FAIL: missing runner token for $provider"; exit 1; }
  MODE="$(stat -c '%a' "$TOKEN_FILE")"
  [[ "$MODE" == "600" ]] || { echo "FAIL: runner token has permissive mode $MODE"; exit 1; }
  TOKEN_COUNT=$((TOKEN_COUNT + 1))
  "$BIN/fleetctl" timeline "$SES" --limit 200 >>"$RUNTIME/public.txt"
  "$BIN/fleetctl" approvals --session "$SES" >>"$RUNTIME/public.txt"
  if [[ "$provider" != "fake" ]]; then
    if ! "$BIN/fleetctl" native-sessions "$provider" >>"$RUNTIME/public.txt" 2>"$RUNTIME/native.err"; then
      grep -Eq 'unsupported_capability|sessions_unavailable' "$RUNTIME/native.err" || { echo "FAIL: native discovery error"; cat "$RUNTIME/native.err"; exit 1; }
    fi
  fi
  "$BIN/fleetctl" stop "$SES" >/dev/null
done
"$BIN/fleetctl" agents >>"$RUNTIME/public.txt"

if grep -Eqi 'sk-[a-z0-9]{12,}|auth\.json|service\.json|/home/[a-z]+/\.codex|Bearer [A-Za-z0-9]|password[[:space:]]*[:=]' "$RUNTIME/public.txt"; then
  echo "FAIL: potential secret leaked into public API output"; grep -Ei 'sk-|auth\.json|Bearer|password' "$RUNTIME/public.txt" | head; exit 1
fi
if grep -Eq 'ses_[a-f0-9]{16,}|per_[a-z0-9_-]{12,}|exec_[a-z0-9-]{12,}|msg_[a-f0-9]{16,}' "$RUNTIME/public.txt"; then
  echo "FAIL: native opaque ID leaked into public API output"; exit 1
fi
[[ "$TOKEN_COUNT" -eq "${#PROVIDERS[@]}" && "$TOKEN_COUNT" -gt 0 ]] || { echo "FAIL: not all live runner tokens verified"; exit 1; }
echo "PASS: no exposed secrets or native IDs; checked permissions of $TOKEN_COUNT live runner tokens"
