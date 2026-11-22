#!/usr/bin/env bash
# P5: SSH real sobre localhost con sshd efímero, claves y known_hosts propios.
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
BIN="$ROOT/target/debug"
command -v sshd >/dev/null || { echo "SKIP: sshd not installed"; exit 0; }
RUNTIME="$(mktemp -d /tmp/gf-p51-ssh-XXXXXX)"
WORKSPACE="$(mktemp -d /tmp/gf-p51-ssh-ws-XXXXXX)"
PORT="$(shuf -i 23000-32000 -n 1)"
USER_NAME="$(whoami)"
cleanup() {
  [[ -f "$RUNTIME/sshd.pid" ]] && kill "$(cat "$RUNTIME/sshd.pid")" 2>/dev/null || true
  [[ -n "${DAEMON_PID:-}" ]] && kill "$DAEMON_PID" 2>/dev/null || true
  pkill -f "$BIN/fleet-ssh-bridge" 2>/dev/null || true
  [[ -n "${GF_KEEP:-}" ]] || { rm -rf "$RUNTIME" "$WORKSPACE"; }
}
trap cleanup EXIT

ssh-keygen -q -t ed25519 -N '' -f "$RUNTIME/host_key"
ssh-keygen -q -t ed25519 -N '' -f "$RUNTIME/client_key"
cp "$RUNTIME/client_key.pub" "$RUNTIME/authorized_keys"
chmod 600 "$RUNTIME/authorized_keys"
cat >"$RUNTIME/sshd_config" <<EOF
Port $PORT
ListenAddress 127.0.0.1
HostKey $RUNTIME/host_key
PidFile $RUNTIME/sshd.pid
AuthorizedKeysFile $RUNTIME/authorized_keys
PasswordAuthentication no
PubkeyAuthentication yes
PermitRootLogin no
UsePAM no
StrictModes no
AllowUsers $USER_NAME
SetEnv FLEET_RUNTIME_DIR=$RUNTIME
SetEnv FLEET_DB_PATH=$RUNTIME/fleet.sqlite
SetEnv FLEET_FAKE_SLOW_TURN_MS=4000
LogLevel ERROR
EOF
/usr/sbin/sshd -f "$RUNTIME/sshd_config" -E "$RUNTIME/sshd.log"
for _ in $(seq 1 50); do [[ -s "$RUNTIME/sshd.pid" ]] && break; sleep 0.1; done
ssh-keyscan -p "$PORT" -t ed25519 127.0.0.1 >"$RUNTIME/known_hosts" 2>/dev/null

export FLEET_RUNTIME_DIR="$RUNTIME" FLEET_DB_PATH="$RUNTIME/fleet.sqlite"
export FLEET_SSH_CONFIG="$RUNTIME/hosts.json"
export FLEET_SSH_KNOWN_HOSTS="$RUNTIME/known_hosts"
"$BIN/fleetd" >"$RUNTIME/daemon.out" 2>"$RUNTIME/daemon.err" &
DAEMON_PID=$!
for _ in $(seq 1 100); do [[ -f "$RUNTIME/fleetd.addr" ]] && break; sleep 0.1; done
[[ -f "$RUNTIME/fleetd.addr" ]] || { echo "FAIL: daemon did not start"; exit 1; }

"$BIN/fleetctl" host add test "$USER_NAME@127.0.0.1" \
  --bridge "$BIN/fleet-ssh-bridge" --port "$PORT" --identity "$RUNTIME/client_key" >/dev/null

# 1. Paridad de proveedores local/remoto.
"$BIN/fleetctl" agents >"$RUNTIME/local-agents.out"
"$BIN/fleetctl" --host test agents >"$RUNTIME/remote-agents.out"
diff -u "$RUNTIME/local-agents.out" "$RUNTIME/remote-agents.out" || { echo "FAIL: agent parity"; exit 1; }

# 2. Workspace remoto (ruta interpretada en el host destino) y conversación.
WS="$("$BIN/fleetctl" --host test workspace add "$WORKSPACE" | cut -f1)"
SES="$("$BIN/fleetctl" --host test session create --workspace "$WS" --provider fake | cut -f1)"
timeout 60 "$BIN/fleetctl" --host test watch "$SES" --until-turn-end >"$RUNTIME/watch.out" 2>"$RUNTIME/watch.err" &
WATCH=$!
sleep 1
"$BIN/fleetctl" --host test prompt "$SES" 'remote turn' >/dev/null
wait "$WATCH"
grep -q $'turn_completed\tCompleted' "$RUNTIME/watch.out" || { echo "FAIL: remote turn"; exit 1; }

# 3. Dos clientes remotos ven la misma timeline persistida.
"$BIN/fleetctl" --host test timeline "$SES" --limit 100 >"$RUNTIME/t1.out"
"$BIN/fleetctl" --host test timeline "$SES" --limit 100 >"$RUNTIME/t2.out"
diff -u "$RUNTIME/t1.out" "$RUNTIME/t2.out" || { echo "FAIL: two remote clients differ"; exit 1; }

# 4. Caída del transporte SSH durante un turno: el runner sobrevive y el
#    cliente reconectado recupera la timeline sin duplicar el prompt.
RUNNER="$(pgrep -f "fleet-runner --runner-id runner-$SES" | head -1)"
[[ -n "$RUNNER" ]] || { echo "FAIL: no runner"; exit 1; }
timeout 60 "$BIN/fleetctl" --host test watch "$SES" --until-turn-end >"$RUNTIME/live.out" 2>"$RUNTIME/live.err" &
LIVE=$!
sleep 1
"$BIN/fleetctl" --host test prompt "$SES" 'survive ssh disconnect' >/dev/null
sleep 1
kill "$LIVE" 2>/dev/null || true
pkill -f "$BIN/fleet-ssh-bridge" 2>/dev/null || true
sleep 1
kill -0 "$RUNNER" 2>/dev/null || { echo "FAIL: runner died with SSH"; exit 1; }
for _ in $(seq 1 100); do
  "$BIN/fleetctl" timeline "$SES" --limit 200 >"$RUNTIME/after.out"
  grep -q $'turn_completed\tCompleted' "$RUNTIME/after.out" && break
  sleep 0.2
done
grep -q $'turn_completed\tCompleted' "$RUNTIME/after.out" || { echo "FAIL: no replay after SSH loss"; exit 1; }
[[ "$(grep -c 'user.survive ssh disconnect' "$RUNTIME/after.out")" == "1" ]] || { echo "FAIL: duplicated prompt after reconnect"; exit 1; }
"$BIN/fleetctl" --host test timeline "$SES" --limit 200 >"$RUNTIME/replay.out"
diff -u "$RUNTIME/after.out" "$RUNTIME/replay.out" || { echo "FAIL: remote replay differs"; exit 1; }

# 5. Identidad de host rechazada.
cp "$RUNTIME/known_hosts" "$RUNTIME/known_hosts.bak"
: >"$RUNTIME/known_hosts"
if "$BIN/fleetctl" --host test agents >"$RUNTIME/hostkey.out" 2>&1; then
  echo "FAIL: untrusted SSH host accepted"; exit 1
fi
grep -qi 'HostKeyRejected\|host key' "$RUNTIME/hostkey.out" || { echo "FAIL: host key error not explicit"; cat "$RUNTIME/hostkey.out"; exit 1; }
cp "$RUNTIME/known_hosts.bak" "$RUNTIME/known_hosts"

# 6. Bridge ausente.
"$BIN/fleetctl" host add broken "$USER_NAME@127.0.0.1" --bridge /nonexistent/fleet-ssh-bridge \
  --port "$PORT" --identity "$RUNTIME/client_key" >/dev/null
if "$BIN/fleetctl" --host broken agents >"$RUNTIME/bridge.out" 2>&1; then
  echo "FAIL: missing bridge accepted"; exit 1
fi
grep -qi 'bridge\|closed\|No such file' "$RUNTIME/bridge.out" || { echo "FAIL: bridge error not explicit"; cat "$RUNTIME/bridge.out"; exit 1; }

echo "PASS: SSH transport parity, streaming, two clients, disconnect survival, replay and security errors"
