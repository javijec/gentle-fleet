#!/usr/bin/env bash
# P2: smoke real opt-in con OpenCode: aprobación aceptada y rechazada.
# Ejecutar con GF_E2E_REAL=1 (usa el servicio OpenCode del usuario).
set -euo pipefail

if [[ "${GF_E2E_REAL:-0}" != "1" ]]; then
  echo "SKIP: real OpenCode approval smoke is opt-in (set GF_E2E_REAL=1)"
  exit 0
fi

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
BIN="$ROOT/target/debug"
RUNTIME="$(mktemp -d /tmp/gf-p2-oc-appr-XXXXXX)"
WORKSPACE="$(mktemp -d /tmp/gf-p2-oc-appr-ws-XXXXXX)"

cleanup() {
  [[ -n "${DAEMON_PID:-}" ]] && kill "$DAEMON_PID" 2>/dev/null || true
  [[ -n "${WATCH_PID:-}" ]] && kill "$WATCH_PID" 2>/dev/null || true
  [[ -n "${WATCH2_PID:-}" ]] && kill "$WATCH2_PID" 2>/dev/null || true
  rm -rf "$RUNTIME" "$WORKSPACE"
    pkill -f "fleet-runner .* --workspace $WORKSPACE" 2>/dev/null || true
}
trap cleanup EXIT

export FLEET_RUNTIME_DIR="$RUNTIME"
export FLEET_DB_PATH="$RUNTIME/fleet.sqlite"
export FLEET_OPENCODE_ASK_SHELL=1

"$BIN/fleetd" >"$RUNTIME/daemon.out" 2>"$RUNTIME/daemon.err" &
DAEMON_PID=$!
for _ in $(seq 1 50); do [[ -f "$RUNTIME/fleetd.addr" ]] && break; sleep 0.1; done
[[ -f "$RUNTIME/fleetd.addr" ]] || { echo "FAIL: fleetd did not start"; exit 1; }

WS_ID="$("$BIN/fleetctl" workspace add "$WORKSPACE" | cut -f1)"

run_case() {
  local label="$1" text="$2" decision="$3" file="$4" expect_file="$5"
  local SID="$("$BIN/fleetctl" session create --workspace "$WS_ID" --provider opencode | cut -f1)"
  "$BIN/fleetctl" watch "$SID" >"$RUNTIME/$label.watch" 2>"$RUNTIME/$label.err" &
  local WATCHER=$!
  for _ in $(seq 1 50); do grep -q subscribed "$RUNTIME/$label.err" 2>/dev/null && break; sleep 0.1; done
  "$BIN/fleetctl" prompt "$SID" "$text" >/dev/null
  local APPROVAL=""
  for _ in $(seq 1 600); do
    APPROVAL="$(awk -F '\t' '$2=="approval_requested" {print $3; exit}' "$RUNTIME/$label.watch" 2>/dev/null || true)"
    [[ -n "$APPROVAL" ]] && break
    sleep 0.25
  done
  [[ -n "$APPROVAL" ]] || { echo "FAIL: no real approval for $label"; cat "$RUNTIME/$label.err"; return 1; }
  FLEET_OPERATION_KEY="$label-key" "$BIN/fleetctl" answer "$APPROVAL" "$decision" >"$RUNTIME/$label.answer"
  for _ in $(seq 1 600); do
    grep -q 'turn_completed' "$RUNTIME/$label.watch" && break
    sleep 0.25
  done
  grep -q 'turn_completed' "$RUNTIME/$label.watch" || { echo "FAIL: turn did not end for $label"; return 1; }
  kill "$WATCHER" 2>/dev/null || true
  "$BIN/fleetctl" stop "$SID" >/dev/null 2>&1 || true
  if [[ "$expect_file" == "yes" ]]; then
    [[ -f "$WORKSPACE/$file" ]] || { echo "FAIL: approved command did not create $file"; return 1; }
  else
    [[ ! -f "$WORKSPACE/$file" ]] || { echo "FAIL: rejected command created $file"; return 1; }
  fi
  echo "PASS: real OpenCode approval $label ($decision)"
}

run_case approve "Run exactly this shell command: echo gf-p2-approved > p2-approved.txt. Then stop." once p2-approved.txt yes
run_case reject "Run exactly this shell command: echo gf-p2-rejected > p2-rejected.txt. Then stop." reject p2-rejected.txt no
