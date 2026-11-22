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
REPORT_DIR="$RUNTIME/reports"
mkdir -p "$REPORT_DIR"
export FLEET_CONFORMANCE_REPORT_DIR="$REPORT_DIR"
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
python3 - "$RUNTIME/report.json" "$REPORT_DIR" "$PROVIDERS" "${GF_E2E_REAL:-0}" <<'PY'
import json, os, sys
with open(sys.argv[1], encoding="utf-8") as fd:
    report = json.load(fd)
report_dir, providers_arg, real = sys.argv[2], set(sys.argv[3].split(",")), sys.argv[4] == "1"
data = report["results"]
if not data:
    raise SystemExit("FAIL: missing conformance results")
for field in ("commit_sha", "branch", "platform", "executed_at_unix_ms", "global_status"):
    if not report.get(field):
        raise SystemExit(f"FAIL: report metadata missing {field}")
if report["global_status"] != "PASS":
    raise SystemExit("FAIL: global report status is not PASS")
files = [name for name in os.listdir(report_dir) if name.endswith(".json")]
if not files:
    raise SystemExit("FAIL: sanitized JSON report was not written")
with open(os.path.join(report_dir, files[0]), encoding="utf-8") as fd:
    persisted = json.load(fd)
if persisted["commit_sha"] != report["commit_sha"] or persisted["results"] != data:
    raise SystemExit("FAIL: persisted report differs from execution")
if {row["provider"] for row in data} != providers_arg:
    raise SystemExit("FAIL: provider coverage incomplete")
required = {"agent_registered", "isolated_workspace", "session_lifecycle",
            "timeline_integrity", "two_client_replay", "stop_idempotence"}
if real:
    required.add("real_chat")
for provider in providers_arg:
    checks = {row["check"]: row for row in data if row["provider"] == provider}
    for check in required:
        if checks.get(check, {}).get("status") != "PASS":
            raise SystemExit(f"FAIL: {provider}/{check}: {checks.get(check)}")
    for row in checks.values():
        if row["status"] != "PASS" and not row.get("justification"):
            raise SystemExit(f"FAIL: {provider}/{row['check']} lacks justification")
    print(f"{provider} {checks.get('agent_registered', {}).get('provider_version', '?')}: {len(checks)} checks, required PASS")
print(f"PASS: one execution, {len(data)} cells, {len(providers_arg)} provider(s), report={files[0]}")
PY
