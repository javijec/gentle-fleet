#!/usr/bin/env bash
# P6.2-B: integración real desde GPUI contra un fleetd local aislado y un host
# SSH de fixture. Desktop no arranca el daemon: lo hace este test explícitamente.
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
BIN="$ROOT/target/debug"
cd "$ROOT"
command -v sshd >/dev/null || { echo "SKIP: sshd not installed"; exit 0; }

BASE="$(mktemp -d /tmp/gf-p62b-gpui-XXXXXX)"
LOCAL_RUNTIME="$BASE/local-runtime"
LOCAL_WORKSPACE="$BASE/local-workspace"
REMOTE_RUNTIME="$BASE/remote-runtime"
REMOTE_WORKSPACE="$BASE/remote-workspace"
PORT="$(shuf -i 23000-32000 -n 1)"
USER_NAME="$(whoami)"
PIDS=()
cleanup() {
  for pid in "${PIDS[@]:-}"; do kill "$pid" 2>/dev/null || true; done
  [[ -n "${GF_KEEP:-}" ]] || rm -rf "$BASE"
}
trap cleanup EXIT

mkdir -p "$LOCAL_RUNTIME" "$LOCAL_WORKSPACE" "$REMOTE_RUNTIME" "$REMOTE_WORKSPACE"
chmod 700 "$LOCAL_RUNTIME" "$REMOTE_RUNTIME"

cargo build --locked -p fleetd -p fleetctl -p fleet-ssh -p fleet-runner >/dev/null
cargo test --locked -p gentle-fleet-desktop --no-run >/dev/null

# Local daemon, isolated from the user's real runtime.
FLEET_RUNTIME_DIR="$LOCAL_RUNTIME" \
FLEET_DB_PATH="$LOCAL_RUNTIME/fleet.sqlite" \
FLEET_FAKE_SLOW_TURN_MS=8000 \
  "$BIN/fleetd" >"$LOCAL_RUNTIME/daemon.out" 2>"$LOCAL_RUNTIME/daemon.err" &
PIDS+=("$!")
for _ in $(seq 1 100); do [[ -f "$LOCAL_RUNTIME/fleetd.addr" ]] && break; sleep 0.1; done
[[ -f "$LOCAL_RUNTIME/fleetd.addr" ]] || { cat "$LOCAL_RUNTIME/daemon.err"; exit 1; }

# Real OpenSSH fixture; its sshd injects the remote runtime so the bridge
# reaches an independent remote fleetd, not the local one.
ssh-keygen -q -t ed25519 -N '' -f "$BASE/host_key"
ssh-keygen -q -t ed25519 -N '' -f "$BASE/client_key"
cp "$BASE/client_key.pub" "$BASE/authorized_keys"
chmod 600 "$BASE/authorized_keys"
cat >"$BASE/sshd_config" <<EOF
Port $PORT
ListenAddress 127.0.0.1
HostKey $BASE/host_key
PidFile $BASE/sshd.pid
AuthorizedKeysFile $BASE/authorized_keys
PasswordAuthentication no
PubkeyAuthentication yes
PermitRootLogin no
UsePAM no
StrictModes no
AllowUsers $USER_NAME
SetEnv FLEET_RUNTIME_DIR=$REMOTE_RUNTIME
SetEnv FLEET_DB_PATH=$REMOTE_RUNTIME/fleet.sqlite
SetEnv FLEET_FAKE_SLOW_TURN_MS=8000
LogLevel ERROR
EOF
/usr/sbin/sshd -f "$BASE/sshd_config" -E "$BASE/sshd.log"
PIDS+=("$(cat "$BASE/sshd.pid")")
for _ in $(seq 1 50); do [[ -s "$BASE/sshd.pid" ]] && break; sleep 0.1; done
ssh-keyscan -p "$PORT" -t ed25519 127.0.0.1 >"$BASE/known_hosts" 2>/dev/null

FLEET_RUNTIME_DIR="$REMOTE_RUNTIME" \
FLEET_DB_PATH="$REMOTE_RUNTIME/fleet.sqlite" \
FLEET_FAKE_SLOW_TURN_MS=8000 \
  "$BIN/fleetd" >"$REMOTE_RUNTIME/daemon.out" 2>"$REMOTE_RUNTIME/daemon.err" &
PIDS+=("$!")
for _ in $(seq 1 100); do [[ -f "$REMOTE_RUNTIME/fleetd.addr" ]] && break; sleep 0.1; done
[[ -f "$REMOTE_RUNTIME/fleetd.addr" ]] || { cat "$REMOTE_RUNTIME/daemon.err"; exit 1; }

export FLEET_SSH_CONFIG="$BASE/hosts.json"
export FLEET_OPERATION_JOURNAL="$BASE/operations.jsonl"
"$BIN/fleetctl" host add fixturebox "$USER_NAME@127.0.0.1" \
  --bridge "$BIN/fleet-ssh-bridge" --port "$PORT" --identity "$BASE/client_key" >/dev/null

# Fixture data for the GUI read-only queries.
export FLEET_RUNTIME_DIR="$LOCAL_RUNTIME" FLEET_DB_PATH="$LOCAL_RUNTIME/fleet.sqlite"
LOCAL_WS="$("$BIN/fleetctl" workspace add "$LOCAL_WORKSPACE" | cut -f1)"
LOCAL_SESSION="$("$BIN/fleetctl" session create --workspace "$LOCAL_WS" --provider fake | cut -f1)"
FLEET_OPERATION_KEY="p62b-local-key" "$BIN/fleetctl" prompt "$LOCAL_SESSION" \
  'desktop gui local turn' >/dev/null

export FLEET_SSH_KNOWN_HOSTS="$BASE/known_hosts"
REMOTE_WS="$(FLEET_SSH_KNOWN_HOSTS="$BASE/known_hosts" "$BIN/fleetctl" --host fixturebox \
  workspace add "$REMOTE_WORKSPACE" | cut -f1)"
REMOTE_SESSION="$(FLEET_SSH_KNOWN_HOSTS="$BASE/known_hosts" "$BIN/fleetctl" --host fixturebox \
  session create --workspace "$REMOTE_WS" --provider fake | cut -f1)"
FLEET_OPERATION_KEY="p62b-remote-key" FLEET_SSH_KNOWN_HOSTS="$BASE/known_hosts" \
  "$BIN/fleetctl" --host fixturebox prompt "$REMOTE_SESSION" 'desktop gui remote turn' >/dev/null

LOCAL_RUNNER="$(pgrep -f "fleet-runner --runner-id runner-$LOCAL_SESSION" | head -1 || true)"
REMOTE_RUNNER="$(pgrep -f "fleet-runner --runner-id runner-$REMOTE_SESSION" | head -1 || true)"
[[ -n "$LOCAL_RUNNER" ]] || { echo "FAIL: local runner not spawned"; exit 1; }
[[ -n "$REMOTE_RUNNER" ]] || { echo "FAIL: remote runner not spawned"; exit 1; }

export FLEET_DESKTOP_EXPECT_LOCAL_SESSION="$LOCAL_SESSION"
export FLEET_DESKTOP_SSH_ALIAS="fixturebox"
export FLEET_DESKTOP_EXPECT_SSH_SESSION="$REMOTE_SESSION"

# The test drives the real GPUI view: catalog, Local connect/refresh/disconnect/
# retry, SSH connect/disconnect against the real SSH transport.
FLEET_RUNTIME_DIR="$LOCAL_RUNTIME" \
FLEET_DB_PATH="$LOCAL_RUNTIME/fleet.sqlite" \
FLEET_SSH_CONFIG="$BASE/hosts.json" \
FLEET_SSH_KNOWN_HOSTS="$BASE/known_hosts" \
FLEET_OPERATION_JOURNAL="$BASE/operations.jsonl" \
  cargo test --locked -p gentle-fleet-desktop app::tests::real_local_and_ssh_fleet_from_gpui \
    -- --ignored --exact --nocapture

# Desktop disconnecting its client must not stop the pre-existing runners.
kill -0 "$LOCAL_RUNNER" 2>/dev/null || { echo "FAIL: local runner died"; exit 1; }
kill -0 "$REMOTE_RUNNER" 2>/dev/null || { echo "FAIL: remote runner died"; exit 1; }

for _ in $(seq 1 80); do
  "$BIN/fleetctl" timeline "$LOCAL_SESSION" --limit 100 \
    >"$BASE/local-timeline.out" 2>/dev/null || true
  grep -q $'turn_completed\tCompleted' "$BASE/local-timeline.out" && break
  sleep 0.25
done
grep -q $'turn_completed\tCompleted' "$BASE/local-timeline.out" || {
  echo "FAIL: local runner turn did not complete after GUI disconnect"; exit 1;
}

for _ in $(seq 1 80); do
  FLEET_SSH_KNOWN_HOSTS="$BASE/known_hosts" "$BIN/fleetctl" --host fixturebox \
    timeline "$REMOTE_SESSION" --limit 100 >"$BASE/remote-timeline.out" 2>/dev/null || true
  grep -q $'turn_completed\tCompleted' "$BASE/remote-timeline.out" && break
  sleep 0.25
done
grep -q $'turn_completed\tCompleted' "$BASE/remote-timeline.out" || {
  echo "FAIL: remote runner turn did not complete after GUI disconnect"; exit 1;
}

echo "PASS: GPUI local + SSH Fleet queries, refresh, disconnect, retry and runner survival"
