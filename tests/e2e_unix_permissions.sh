#!/usr/bin/env bash
# P1.1: el socket Unix solo es accesible por el usuario dueño del daemon.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
BIN="$ROOT/target/debug"
RUNTIME="$(mktemp -d /tmp/gf-p11-acl-XXXXXX)"

cleanup() {
  [[ -n "${DAEMON_PID:-}" ]] && kill "$DAEMON_PID" 2>/dev/null || true
  rm -rf "$RUNTIME"
}
trap cleanup EXIT

if [[ "$(uname -s)" != "Linux" ]]; then
  echo "SKIP: socket permission test requires Linux (platform UNTESTED)"
  exit 0
fi

export FLEET_RUNTIME_DIR="$RUNTIME"
"$BIN/fleetd" >"$RUNTIME/daemon.out" 2>"$RUNTIME/daemon.err" &
DAEMON_PID=$!
for _ in $(seq 1 50); do
  [[ -f "$RUNTIME/fleetd.addr" ]] && break
  sleep 0.1
done
[[ -f "$RUNTIME/fleetd.addr" ]] || { echo "FAIL: fleetd did not start"; exit 1; }

SOCKET="$(cat "$RUNTIME/fleetd.addr")"
MODE="$(stat -c '%a' "$SOCKET")"
DIR_MODE="$(stat -c '%a' "$RUNTIME")"
echo "socket mode=$MODE runtime dir mode=$DIR_MODE"
[[ "$MODE" == "600" ]] || { echo "FAIL: socket is not 0600"; exit 1; }
[[ "$DIR_MODE" == "700" ]] || { echo "FAIL: runtime dir is not 0700"; exit 1; }

if [[ "$EUID" -ne 0 ]] || ! id nobody >/dev/null 2>&1 || ! command -v runuser >/dev/null 2>&1; then
  echo "UNTESTED: cross-user check requires root, nobody and runuser; filesystem modes verified"
  exit 0
fi

set +e
runuser -u nobody -- env FLEET_RUNTIME_DIR="$RUNTIME" "$BIN/fleetctl" agents \
  >"$RUNTIME/other.out" 2>"$RUNTIME/other.err"
OTHER_CODE=$?
set -e
if (( OTHER_CODE == 0 )); then
  echo "FAIL: another local user connected to fleetd"; exit 1
fi
echo "PASS: another local user was rejected (exit $OTHER_CODE)"
