#!/usr/bin/env bash
# P6.1 black-box SDK contract: local API, chat, event subscription and cancel.
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"
cargo build --locked -p fleetd -p fleetctl -p fleet-client --example sdk_smoke
RUNTIME="$(mktemp -d /tmp/gf-p61-sdk-XXXXXX)"
WORKSPACE="$(mktemp -d /tmp/gf-p61-sdk-ws-XXXXXX)"
chmod 700 "$RUNTIME"
cleanup() {
  [[ -n "${DAEMON_PID:-}" ]] && kill "$DAEMON_PID" 2>/dev/null || true
  [[ -n "${GF_KEEP:-}" ]] || { rm -rf "$RUNTIME" "$WORKSPACE"; }
}
trap cleanup EXIT
export FLEET_RUNTIME_DIR="$RUNTIME" FLEET_DB_PATH="$RUNTIME/fleet.sqlite"
export FLEET_CURSOR_JOURNAL="$RUNTIME/cursors.jsonl"
"$ROOT/target/debug/fleetd" >"$RUNTIME/daemon.out" 2>"$RUNTIME/daemon.err" &
DAEMON_PID=$!
for _ in $(seq 1 100); do [[ -f "$RUNTIME/fleetd.addr" ]] && break; sleep 0.1; done
[[ -f "$RUNTIME/fleetd.addr" ]] || { cat "$RUNTIME/daemon.err"; exit 1; }
"$ROOT/target/debug/fleetctl" agents | cut -f1 | sort >"$RUNTIME/cli-agents"
"$ROOT/target/debug/examples/sdk_smoke" "$WORKSPACE" >"$RUNTIME/sdk.out"
grep '^SDK_AGENT' "$RUNTIME/sdk.out" | cut -f2 | sort >"$RUNTIME/sdk-agents"
diff -u "$RUNTIME/cli-agents" "$RUNTIME/sdk-agents"
cat "$RUNTIME/sdk.out"
if [[ "${GF_E2E_REAL:-}" == "1" ]]; then
  FLEET_SDK_PROVIDER=opencode \
    FLEET_SDK_PROMPT="Reply with a short greeting for the Fleet SDK smoke." \
    timeout 300 "$ROOT/target/debug/examples/sdk_smoke" "$WORKSPACE" \
    >"$RUNTIME/sdk-opencode.out"
  grep -q '^PASS:' "$RUNTIME/sdk-opencode.out"
  echo "PASS: opt-in real OpenCode SDK smoke"
fi
