#!/usr/bin/env bash
# P5.7: dos hosts SSH independientes, con IDs de sesión iguales sin mezclarse.
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
BIN="$ROOT/target/debug"
command -v sshd >/dev/null || { echo "SKIP: sshd not installed"; exit 0; }
BASE="$(mktemp -d /tmp/gf-p52-hosts-XXXXXX)"
USER_NAME="$(whoami)"
PIDS=()
cleanup() {
  for pid in "${PIDS[@]:-}"; do kill "$pid" 2>/dev/null || true; done
  pkill -f "$BIN/fleet-ssh-bridge" 2>/dev/null || true
  [[ -n "${GF_KEEP:-}" ]] || rm -rf "$BASE"
}
trap cleanup EXIT

start_host() {
  local name="$1" port="$2"
  local dir="$BASE/$name"
  mkdir -p "$dir/runtime" "$dir/workspace"
  chmod 700 "$dir/runtime"
  ssh-keygen -q -t ed25519 -N '' -f "$dir/host_key"
  ssh-keygen -q -t ed25519 -N '' -f "$dir/client_key"
  cp "$dir/client_key.pub" "$dir/authorized_keys"
  chmod 600 "$dir/authorized_keys"
  cat >"$dir/sshd_config" <<EOF
Port $port
ListenAddress 127.0.0.1
HostKey $dir/host_key
PidFile $dir/sshd.pid
AuthorizedKeysFile $dir/authorized_keys
PasswordAuthentication no
PubkeyAuthentication yes
PermitRootLogin no
UsePAM no
StrictModes no
AllowUsers $USER_NAME
SetEnv FLEET_RUNTIME_DIR=$dir/runtime
SetEnv FLEET_DB_PATH=$dir/runtime/fleet.sqlite
LogLevel ERROR
EOF
  /usr/sbin/sshd -f "$dir/sshd_config" -E "$dir/sshd.log"
  PIDS+=("$(cat "$dir/sshd.pid")")
  ssh-keyscan -p "$port" -t ed25519 127.0.0.1 >"$dir/known_hosts" 2>/dev/null
  FLEET_RUNTIME_DIR="$dir/runtime" FLEET_DB_PATH="$dir/runtime/fleet.sqlite" \
    "$BIN/fleetd" >"$dir/daemon.out" 2>"$dir/daemon.err" &
  PIDS+=("$!")
  for _ in $(seq 1 100); do [[ -f "$dir/runtime/fleetd.addr" ]] && break; sleep 0.1; done
}

PORT_A="$(shuf -i 24000-29000 -n 1)"
PORT_B="$(shuf -i 29001-34000 -n 1)"
start_host a "$PORT_A"
start_host b "$PORT_B"

export FLEET_SSH_CONFIG="$BASE/hosts.json"
"$BIN/fleetctl" host add hosta "$USER_NAME@127.0.0.1" --bridge "$BIN/fleet-ssh-bridge" \
  --port "$PORT_A" --identity "$BASE/a/client_key" >/dev/null
"$BIN/fleetctl" host add hostb "$USER_NAME@127.0.0.1" --bridge "$BIN/fleet-ssh-bridge" \
  --port "$PORT_B" --identity "$BASE/b/client_key" >/dev/null

FLEET_SSH_KNOWN_HOSTS="$BASE/a/known_hosts" "$BIN/fleetctl" --host hosta agents >"$BASE/agents-a.out"
FLEET_SSH_KNOWN_HOSTS="$BASE/b/known_hosts" "$BIN/fleetctl" --host hostb agents >"$BASE/agents-b.out"
diff -u "$BASE/agents-a.out" "$BASE/agents-b.out" || { echo "FAIL: providers differ"; exit 1; }

WSA="$(FLEET_SSH_KNOWN_HOSTS="$BASE/a/known_hosts" "$BIN/fleetctl" --host hosta workspace add "$BASE/a/workspace" | cut -f1)"
WSB="$(FLEET_SSH_KNOWN_HOSTS="$BASE/b/known_hosts" "$BIN/fleetctl" --host hostb workspace add "$BASE/b/workspace" | cut -f1)"
SA="$(FLEET_SSH_KNOWN_HOSTS="$BASE/a/known_hosts" "$BIN/fleetctl" --host hosta session create --workspace "$WSA" --provider fake | cut -f1)"
SB="$(FLEET_SSH_KNOWN_HOSTS="$BASE/b/known_hosts" "$BIN/fleetctl" --host hostb session create --workspace "$WSB" --provider fake | cut -f1)"
[[ -n "$SA" && -n "$SB" ]] || { echo "FAIL: session creation"; exit 1; }

FLEET_SSH_KNOWN_HOSTS="$BASE/a/known_hosts" "$BIN/fleetctl" --host hosta session list >"$BASE/list-a.out"
FLEET_SSH_KNOWN_HOSTS="$BASE/b/known_hosts" "$BIN/fleetctl" --host hostb session list >"$BASE/list-b.out"
awk -F '\t' -v id="$SA" -v ws="$WSA" '$1==id && $3==ws {ok=1} END{exit !ok}' "$BASE/list-a.out" || { echo "FAIL: session A missing on host A"; exit 1; }
awk -F '\t' -v id="$SB" -v ws="$WSB" '$1==id && $3==ws {ok=1} END{exit !ok}' "$BASE/list-b.out" || { echo "FAIL: session B missing on host B"; exit 1; }
FLEET_SSH_KNOWN_HOSTS="$BASE/a/known_hosts" "$BIN/fleetctl" --host hosta workspace list >"$BASE/ws-a.out"
FLEET_SSH_KNOWN_HOSTS="$BASE/b/known_hosts" "$BIN/fleetctl" --host hostb workspace list >"$BASE/ws-b.out"
grep -q "$BASE/a/workspace" "$BASE/ws-a.out" || { echo "FAIL: host A workspace path wrong"; exit 1; }
grep -q "$BASE/b/workspace" "$BASE/ws-b.out" || { echo "FAIL: host B workspace path wrong"; exit 1; }
! grep -q "$BASE/b/workspace" "$BASE/ws-a.out" || { echo "FAIL: host B path on host A"; exit 1; }
! grep -q "$BASE/a/workspace" "$BASE/ws-b.out" || { echo "FAIL: host A path on host B"; exit 1; }

# Misma operation_key en hosts distintos no debe mezclar operaciones.
FLEET_OPERATION_KEY="shared-key" FLEET_SSH_KNOWN_HOSTS="$BASE/a/known_hosts" \
  "$BIN/fleetctl" --host hosta prompt "$SA" 'host A' >/dev/null
FLEET_OPERATION_KEY="shared-key" FLEET_SSH_KNOWN_HOSTS="$BASE/b/known_hosts" \
  "$BIN/fleetctl" --host hostb prompt "$SB" 'host B' >/dev/null
FLEET_SSH_KNOWN_HOSTS="$BASE/a/known_hosts" "$BIN/fleetctl" --host hosta timeline "$SA" --limit 100 >"$BASE/tl-a.out"
FLEET_SSH_KNOWN_HOSTS="$BASE/b/known_hosts" "$BIN/fleetctl" --host hostb timeline "$SB" --limit 100 >"$BASE/tl-b.out"
grep -q 'user.host A' "$BASE/tl-a.out" && ! grep -q 'user.host B' "$BASE/tl-a.out" || { echo "FAIL: host A timeline mixed"; exit 1; }
grep -q 'user.host B' "$BASE/tl-b.out" && ! grep -q 'user.host A' "$BASE/tl-b.out" || { echo "FAIL: host B timeline mixed"; exit 1; }

echo "PASS: two independent SSH hosts; sessions, operation keys and timelines isolated"
