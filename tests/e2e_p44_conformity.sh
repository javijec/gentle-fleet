#!/usr/bin/env bash
# P4.4-H: ejecutar exactamente una vez, en JSON. CI usa FakeDriver
# explícitamente; los cuatro proveedores reales requieren entorno local.
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
BIN="$ROOT/target/debug"
RUNTIME="$(mktemp -d /tmp/gf-p44-conf-XXXXXX)"
WORKSPACE="$(mktemp -d /tmp/gf-p44-conf-ws-XXXXXX)"
cleanup() {
  [[ -n "${DAEMON_PID:-}" ]] && kill "$DAEMON_PID" 2>/dev/null || true
  pkill -f "fleet-runner .* --workspace $WORKSPACE" 2>/dev/null || true
  [[ -n "${GF_KEEP:-}" ]] || rm -rf "$RUNTIME" "$WORKSPACE"
}
trap cleanup EXIT
export FLEET_RUNTIME_DIR="$RUNTIME" FLEET_DB_PATH="$RUNTIME/fleet.sqlite"
"$BIN/fleetd" >"$RUNTIME/daemon.out" 2>"$RUNTIME/daemon.err" &
DAEMON_PID=$!
for _ in $(seq 1 100); do [[ -f "$RUNTIME/fleetd.addr" ]] && break; sleep 0.1; done
[[ -f "$RUNTIME/fleetd.addr" ]] || { echo "FAIL: daemon unavailable"; exit 1; }
WS="$("$BIN/fleetctl" workspace add "$WORKSPACE" | cut -f1)"
[[ -n "$WS" ]] || { echo "FAIL: no workspace"; exit 1; }
PROVIDERS="opencode,codex,pi,gentle-shell"
if [[ "${GF_E2E_FAKE_ONLY:-}" == "1" ]]; then PROVIDERS="fake"; fi
ARGS=(--providers "$PROVIDERS" --workspace "$WS" --json)
if [[ "${GF_E2E_REAL:-}" == "1" ]]; then ARGS+=(--real); fi
if ! "$BIN/fleet-conformance" "${ARGS[@]}" >"$RUNTIME/report.json" 2>"$RUNTIME/report.err"; then
  cat "$RUNTIME/report.err" >&2
  cat "$RUNTIME/report.json" >&2
  echo "FAIL: conformance returned nonzero"; exit 1
fi
python3 - "$RUNTIME/report.json" "$PROVIDERS" "${GF_E2E_REAL:-0}" <<'PY'
import json, sys
with open(sys.argv[1], encoding="utf-8") as fd:
    data = json.load(fd)
expected = set(sys.argv[2].split(","))
real = sys.argv[3] == "1"
if not isinstance(data, list) or not data:
    raise SystemExit("FAIL: missing conformance results")
if {row["provider"] for row in data} != expected:
    raise SystemExit("FAIL: provider coverage incomplete")
required = {"agent_registered", "isolated_workspace", "session_lifecycle",
            "timeline_integrity", "two_client_replay", "stop_idempotence"}
if real:
    required.add("real_chat")
for provider in expected:
    checks = {row["check"]: row for row in data if row["provider"] == provider}
    for check in required:
        if checks.get(check, {}).get("status") != "PASS":
            raise SystemExit(f"FAIL: {provider}/{check}: {checks.get(check)}")
    if any(row["status"] == "FAIL" for row in checks.values()):
        raise SystemExit(f"FAIL: {provider} contains FAIL")
    print(f"{provider}: {len(checks)} explicit checks, required PASS")
print(f"PASS: one execution, {len(data)} cells, {len(expected)} provider(s)")
PY
