#!/usr/bin/env bash
# P5: SSH real sobre localhost con sshd efímero, claves y known_hosts propios.
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
BIN="$ROOT/target/debug"
cd "$ROOT"
cargo build --locked -p fleetd -p fleetctl -p fleet-client --example sdk_smoke >/dev/null
command -v sshd >/dev/null || { echo "SKIP: sshd not installed"; exit 0; }
RUNTIME="$(mktemp -d /tmp/gf-p51-ssh-XXXXXX)"
WORKSPACE="$(mktemp -d /tmp/gf-p51-ssh-ws-XXXXXX)"
PORT="$(shuf -i 23000-32000 -n 1)"
USER_NAME="$(whoami)"
cleanup() {
  [[ -f "$RUNTIME/sshd.pid" ]] && kill "$(cat "$RUNTIME/sshd.pid")" 2>/dev/null || true
  [[ -n "${DAEMON_PID:-}" ]] && kill "$DAEMON_PID" 2>/dev/null || true
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
export FLEET_CURSOR_JOURNAL="$RUNTIME/cursors.jsonl"
export FLEET_OPERATION_JOURNAL="$RUNTIME/operations.jsonl"
"$BIN/fleetd" >"$RUNTIME/daemon.out" 2>"$RUNTIME/daemon.err" &
DAEMON_PID=$!
for _ in $(seq 1 100); do [[ -f "$RUNTIME/fleetd.addr" ]] && break; sleep 0.1; done
[[ -f "$RUNTIME/fleetd.addr" ]] || { echo "FAIL: daemon did not start"; exit 1; }

"$BIN/fleetctl" host add test "$USER_NAME@127.0.0.1" \
  --bridge "$BIN/fleet-ssh-bridge" --port "$PORT" --identity "$RUNTIME/client_key" >/dev/null

# The reusable Rust SDK must consume the same remote Fleet API as fleetctl.
"$BIN/fleetctl" --host test agents | cut -f1 | sort >"$RUNTIME/cli-remote-agents"
"$BIN/examples/sdk_smoke" --host test "$WORKSPACE" >"$RUNTIME/sdk-remote.out"
grep '^SDK_AGENT' "$RUNTIME/sdk-remote.out" | cut -f2 | sort >"$RUNTIME/sdk-remote-agents"
diff -u "$RUNTIME/cli-remote-agents" "$RUNTIME/sdk-remote-agents"

# A live SDK observer reconnects after its bridge is killed. FakeDriver's
# configured delay leaves a deterministic window after the accepted prompt.
timeout 90 "$BIN/examples/sdk_smoke" --reconnect test "$WORKSPACE" \
  >"$RUNTIME/sdk-reconnect.out" 2>"$RUNTIME/sdk-reconnect.err" &
SDK_PID=$!
for _ in $(seq 1 300); do
  grep -q '^SDK_PROMPT_SENT$' "$RUNTIME/sdk-reconnect.out" 2>/dev/null && break
  kill -0 "$SDK_PID" 2>/dev/null || { cat "$RUNTIME/sdk-reconnect.err"; exit 1; }
  sleep 0.05
done
grep -q '^SDK_PROMPT_SENT$' "$RUNTIME/sdk-reconnect.out" || {
  echo "FAIL: SDK did not reach accepted prompt"; exit 1;
}
# Resolve this test's timeout(1) child, then only its SSH processes.
# Never kill fleet-ssh-bridge globally: other users may be connected.
SDK_CHILD="$(pgrep -P "$SDK_PID" -x sdk_smoke | head -1 || true)"
[[ -n "$SDK_CHILD" ]] || { echo "FAIL: SDK child process not found"; exit 1; }
SSH_CHILDREN=""
for _ in $(seq 1 100); do
  SSH_CHILDREN="$(pgrep -P "$SDK_CHILD" -x ssh || true)"
  SSH_COUNT="$(printf '%s\n' "$SSH_CHILDREN" | grep -Ec '^[0-9]+
wait "$SDK_PID" || { cat "$RUNTIME/sdk-reconnect.err"; exit 1; }
SDK_SESSION="$(awk -F '\t' '$1=="SDK_SESSION" {print $2; exit}' "$RUNTIME/sdk-reconnect.out")"
[[ -n "$SDK_SESSION" ]] || { echo "FAIL: SDK session ID missing"; exit 1; }
[[ "$(grep -c '^SDK_EVENT' "$RUNTIME/sdk-reconnect.out")" -eq \
   "$(grep '^SDK_EVENT' "$RUNTIME/sdk-reconnect.out" | cut -f2 | sort -u | wc -l)" ]] || {
  echo "FAIL: SDK emitted duplicate event sequences"; exit 1;
}
"$BIN/fleetctl" --host test timeline "$SDK_SESSION" --limit 100 \
  >"$RUNTIME/sdk-reconnect-timeline.out"
[[ "$(grep -c 'user.SDK smoke' "$RUNTIME/sdk-reconnect-timeline.out")" == "1" ]] || {
  echo "FAIL: SDK reconnect duplicated prompt"; exit 1;
}

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
"$BIN/fleetctl" --host test prompt "$SES" 'remote turn' >"$RUNTIME/first-receipt.out" 2>"$RUNTIME/first-receipt.err"
wait "$WATCH"
[[ -s "$FLEET_OPERATION_JOURNAL" ]] || { echo "FAIL: no durable operation journal"; exit 1; }
[[ "$(stat -c %a "$FLEET_OPERATION_JOURNAL")" == "600" ]] || { echo "FAIL: operation journal not private"; exit 1; }
FIRST_KEY="$(sed -n 's/.*operation_key=\([^ ]*\).*/\1/p' "$RUNTIME/first-receipt.err" | head -1)"
[[ -n "$FIRST_KEY" ]] || { echo "FAIL: lost generated operation key"; exit 1; }
"$BIN/fleetctl" --host test pending | grep -Fq "$FIRST_KEY" || { echo "FAIL: generated key cannot be recovered"; exit 1; }
"$BIN/fleetctl" --host test operation "$FIRST_KEY" >"$RUNTIME/first-operation.out" || { echo "FAIL: cannot reconcile generated operation key"; exit 1; }
grep -q $'turn_completed\tCompleted' "$RUNTIME/watch.out" || { echo "FAIL: remote turn"; exit 1; }

# 3. Dos clientes remotos ven la misma timeline persistida.
"$BIN/fleetctl" --host test timeline "$SES" --limit 100 >"$RUNTIME/t1.out"
"$BIN/fleetctl" --host test timeline "$SES" --limit 100 >"$RUNTIME/t2.out"
diff -u "$RUNTIME/t1.out" "$RUNTIME/t2.out" || { echo "FAIL: two remote clients differ"; exit 1; }

# 4. Caída del transporte SSH durante un turno: el runner sobrevive y el
#    cliente reconectado recupera la timeline sin duplicar el prompt.
RUNNER="$(pgrep -f "fleet-runner --runner-id runner-$SES" | head -1)"
[[ -n "$RUNNER" ]] || { echo "FAIL: no runner"; exit 1; }
# Capture the cursor after the FIRST terminal. A previous completion is
# not evidence that the later turn survived SSH disconnection.
"$BIN/fleetctl" timeline "$SES" --limit 200 >"$RUNTIME/before-second.out"
BASELINE="$(tail -1 "$RUNTIME/before-second.out" | cut -f1)"
[[ "$BASELINE" =~ ^[0-9]+$ ]] || { echo "FAIL: invalid baseline cursor"; exit 1; }
timeout 60 "$BIN/fleetctl" --host test watch "$SES" --since "$BASELINE" --until-turn-end >"$RUNTIME/live.out" 2>"$RUNTIME/live.err" &
LIVE=$!
sleep 1
FLEET_OPERATION_KEY="p51-survive-key" "$BIN/fleetctl" --host test prompt "$SES" \
  'survive ssh disconnect' >/dev/null
sleep 1
# Disconnect only this test's watcher and its children. A global pkill of
# fleet-ssh-bridge could terminate unrelated user connections.
pkill -P "$LIVE" 2>/dev/null || true
kill "$LIVE" 2>/dev/null || true
wait "$LIVE" 2>/dev/null || true
sleep 1
kill -0 "$RUNNER" 2>/dev/null || { echo "FAIL: runner died with SSH"; exit 1; }
for _ in $(seq 1 100); do
  "$BIN/fleetctl" timeline "$SES" --since "$BASELINE" --limit 200 >"$RUNTIME/after.out"
  if grep -q $'turn_completed\tCompleted' "$RUNTIME/after.out" &&
     grep -q $'user\tsurvive ssh disconnect' "$RUNTIME/after.out"; then
    break
  fi
  sleep 0.2
done
[[ "$(grep -c $'turn_completed\tCompleted' "$RUNTIME/after.out")" == "1" ]] || { echo "FAIL: new turn must have exactly one Completed terminal"; exit 1; }
[[ "$(grep -c 'turn_started' "$RUNTIME/after.out")" == "1" ]] || { echo "FAIL: new turn started incorrectly"; exit 1; }
[[ "$(grep -c $'user\tsurvive ssh disconnect' "$RUNTIME/after.out")" == "1" ]] || { echo "FAIL: new prompt missing or duplicated"; exit 1; }
! grep -Eq 'turn_uncertain|turn_completed[[:space:]]+(Failed|Interrupted)' "$RUNTIME/after.out" || { echo "FAIL: contradictory terminal"; exit 1; }
"$BIN/fleetctl" --host test operation "p51-survive-key" >"$RUNTIME/reconciled-operation.out" || { echo "FAIL: operation not recoverable after SSH loss"; exit 1; }
"$BIN/fleetctl" --host test timeline "$SES" --since "$BASELINE" --limit 200 >"$RUNTIME/replay.out"
diff -u "$RUNTIME/after.out" "$RUNTIME/replay.out" || { echo "FAIL: remote replay differs"; exit 1; }

# 4b. Un reenvío explícito con la MISMA operation_key no duplica el prompt.
FLEET_OPERATION_KEY="p51-survive-key" "$BIN/fleetctl" --host test prompt "$SES" \
  'survive ssh disconnect' >"$RUNTIME/retry.out"
"$BIN/fleetctl" timeline "$SES" --since "$BASELINE" --limit 200 >"$RUNTIME/after-retry.out"
[[ "$(grep -c 'user.survive ssh disconnect' "$RUNTIME/after-retry.out")" == "1" ]] || {
  echo "FAIL: prompt duplicated after explicit retry with same operation_key"; exit 1;
}

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

if [[ "${GF_E2E_REAL:-}" == "1" ]]; then
  REAL_UNTESTED=0
  for provider in opencode codex pi gentle-shell; do
    REAL_SES="$("$BIN/fleetctl" --host test session create --workspace "$WS" --provider "$provider" | cut -f1)"
    timeout 180 "$BIN/fleetctl" --host test watch "$REAL_SES" --until-turn-end \
      >"$RUNTIME/chat-$provider.out" 2>"$RUNTIME/chat-$provider.err" &
    RW=$!
    sleep 1
    if ! "$BIN/fleetctl" --host test prompt "$REAL_SES" "Reply with exactly: P5 $provider OK" \
      >"$RUNTIME/prompt-$provider.out" 2>"$RUNTIME/prompt-$provider.err"; then
      kill "$RW" 2>/dev/null || true
      if grep -Eqi 'model|unsupported' "$RUNTIME/prompt-$provider.err"; then
        echo "UNTESTED: $provider has no configured model"
        REAL_UNTESTED=$((REAL_UNTESTED + 1))
        continue
      fi
      echo "FAIL: $provider remote prompt failed"; cat "$RUNTIME/prompt-$provider.err"; exit 1
    fi
    wait "$RW"
    grep -q $'turn_completed\tCompleted' "$RUNTIME/chat-$provider.out" || {
      echo "FAIL: $provider remote turn did not complete"; exit 1;
    }
    grep -qi "P5 $provider OK" "$RUNTIME/chat-$provider.out" || {
      echo "FAIL: $provider remote text missing"; exit 1;
    }
    "$BIN/fleetctl" --host test stop "$REAL_SES" >/dev/null
    echo "PASS: real remote chat $provider"
  done
  [[ "$REAL_UNTESTED" == "0" ]] || { echo "FAIL: $REAL_UNTESTED real provider smokes remain UNTESTED"; exit 1; }
fi

echo "PASS: SSH transport parity, streaming, two clients, disconnect survival, replay and security errors"
 || true)"
  [[ "$SSH_COUNT" -ge 2 ]] && break
  sleep 0.05
done
[[ "$SSH_COUNT" -ge 2 ]] || {
  echo "FAIL: expected both SDK-owned SSH connections"; exit 1;
}
while read -r SSH_CHILD; do
  [[ -n "$SSH_CHILD" ]] && kill "$SSH_CHILD" 2>/dev/null || true
done <<< "$SSH_CHILDREN"
wait "$SDK_PID" || { cat "$RUNTIME/sdk-reconnect.err"; exit 1; }
SDK_SESSION="$(awk -F '\t' '$1=="SDK_SESSION" {print $2; exit}' "$RUNTIME/sdk-reconnect.out")"
[[ -n "$SDK_SESSION" ]] || { echo "FAIL: SDK session ID missing"; exit 1; }
[[ "$(grep -c '^SDK_EVENT' "$RUNTIME/sdk-reconnect.out")" -eq \
   "$(grep '^SDK_EVENT' "$RUNTIME/sdk-reconnect.out" | cut -f2 | sort -u | wc -l)" ]] || {
  echo "FAIL: SDK emitted duplicate event sequences"; exit 1;
}
"$BIN/fleetctl" --host test timeline "$SDK_SESSION" --limit 100 \
  >"$RUNTIME/sdk-reconnect-timeline.out"
[[ "$(grep -c 'user.SDK smoke' "$RUNTIME/sdk-reconnect-timeline.out")" == "1" ]] || {
  echo "FAIL: SDK reconnect duplicated prompt"; exit 1;
}

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
"$BIN/fleetctl" --host test prompt "$SES" 'remote turn' >"$RUNTIME/first-receipt.out" 2>"$RUNTIME/first-receipt.err"
wait "$WATCH"
[[ -s "$FLEET_OPERATION_JOURNAL" ]] || { echo "FAIL: no durable operation journal"; exit 1; }
[[ "$(stat -c %a "$FLEET_OPERATION_JOURNAL")" == "600" ]] || { echo "FAIL: operation journal not private"; exit 1; }
FIRST_KEY="$(sed -n 's/.*operation_key=\([^ ]*\).*/\1/p' "$RUNTIME/first-receipt.err" | head -1)"
[[ -n "$FIRST_KEY" ]] || { echo "FAIL: lost generated operation key"; exit 1; }
"$BIN/fleetctl" --host test pending | grep -Fq "$FIRST_KEY" || { echo "FAIL: generated key cannot be recovered"; exit 1; }
"$BIN/fleetctl" --host test operation "$FIRST_KEY" >"$RUNTIME/first-operation.out" || { echo "FAIL: cannot reconcile generated operation key"; exit 1; }
grep -q $'turn_completed\tCompleted' "$RUNTIME/watch.out" || { echo "FAIL: remote turn"; exit 1; }

# 3. Dos clientes remotos ven la misma timeline persistida.
"$BIN/fleetctl" --host test timeline "$SES" --limit 100 >"$RUNTIME/t1.out"
"$BIN/fleetctl" --host test timeline "$SES" --limit 100 >"$RUNTIME/t2.out"
diff -u "$RUNTIME/t1.out" "$RUNTIME/t2.out" || { echo "FAIL: two remote clients differ"; exit 1; }

# 4. Caída del transporte SSH durante un turno: el runner sobrevive y el
#    cliente reconectado recupera la timeline sin duplicar el prompt.
RUNNER="$(pgrep -f "fleet-runner --runner-id runner-$SES" | head -1)"
[[ -n "$RUNNER" ]] || { echo "FAIL: no runner"; exit 1; }
# Capture the cursor after the FIRST terminal. A previous completion is
# not evidence that the later turn survived SSH disconnection.
"$BIN/fleetctl" timeline "$SES" --limit 200 >"$RUNTIME/before-second.out"
BASELINE="$(tail -1 "$RUNTIME/before-second.out" | cut -f1)"
[[ "$BASELINE" =~ ^[0-9]+$ ]] || { echo "FAIL: invalid baseline cursor"; exit 1; }
timeout 60 "$BIN/fleetctl" --host test watch "$SES" --since "$BASELINE" --until-turn-end >"$RUNTIME/live.out" 2>"$RUNTIME/live.err" &
LIVE=$!
sleep 1
FLEET_OPERATION_KEY="p51-survive-key" "$BIN/fleetctl" --host test prompt "$SES" \
  'survive ssh disconnect' >/dev/null
sleep 1
# Disconnect only this test's watcher and its children. A global pkill of
# fleet-ssh-bridge could terminate unrelated user connections.
pkill -P "$LIVE" 2>/dev/null || true
kill "$LIVE" 2>/dev/null || true
wait "$LIVE" 2>/dev/null || true
sleep 1
kill -0 "$RUNNER" 2>/dev/null || { echo "FAIL: runner died with SSH"; exit 1; }
for _ in $(seq 1 100); do
  "$BIN/fleetctl" timeline "$SES" --since "$BASELINE" --limit 200 >"$RUNTIME/after.out"
  if grep -q $'turn_completed\tCompleted' "$RUNTIME/after.out" &&
     grep -q $'user\tsurvive ssh disconnect' "$RUNTIME/after.out"; then
    break
  fi
  sleep 0.2
done
[[ "$(grep -c $'turn_completed\tCompleted' "$RUNTIME/after.out")" == "1" ]] || { echo "FAIL: new turn must have exactly one Completed terminal"; exit 1; }
[[ "$(grep -c 'turn_started' "$RUNTIME/after.out")" == "1" ]] || { echo "FAIL: new turn started incorrectly"; exit 1; }
[[ "$(grep -c $'user\tsurvive ssh disconnect' "$RUNTIME/after.out")" == "1" ]] || { echo "FAIL: new prompt missing or duplicated"; exit 1; }
! grep -Eq 'turn_uncertain|turn_completed[[:space:]]+(Failed|Interrupted)' "$RUNTIME/after.out" || { echo "FAIL: contradictory terminal"; exit 1; }
"$BIN/fleetctl" --host test operation "p51-survive-key" >"$RUNTIME/reconciled-operation.out" || { echo "FAIL: operation not recoverable after SSH loss"; exit 1; }
"$BIN/fleetctl" --host test timeline "$SES" --since "$BASELINE" --limit 200 >"$RUNTIME/replay.out"
diff -u "$RUNTIME/after.out" "$RUNTIME/replay.out" || { echo "FAIL: remote replay differs"; exit 1; }

# 4b. Un reenvío explícito con la MISMA operation_key no duplica el prompt.
FLEET_OPERATION_KEY="p51-survive-key" "$BIN/fleetctl" --host test prompt "$SES" \
  'survive ssh disconnect' >"$RUNTIME/retry.out"
"$BIN/fleetctl" timeline "$SES" --since "$BASELINE" --limit 200 >"$RUNTIME/after-retry.out"
[[ "$(grep -c 'user.survive ssh disconnect' "$RUNTIME/after-retry.out")" == "1" ]] || {
  echo "FAIL: prompt duplicated after explicit retry with same operation_key"; exit 1;
}

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

if [[ "${GF_E2E_REAL:-}" == "1" ]]; then
  REAL_UNTESTED=0
  for provider in opencode codex pi gentle-shell; do
    REAL_SES="$("$BIN/fleetctl" --host test session create --workspace "$WS" --provider "$provider" | cut -f1)"
    timeout 180 "$BIN/fleetctl" --host test watch "$REAL_SES" --until-turn-end \
      >"$RUNTIME/chat-$provider.out" 2>"$RUNTIME/chat-$provider.err" &
    RW=$!
    sleep 1
    if ! "$BIN/fleetctl" --host test prompt "$REAL_SES" "Reply with exactly: P5 $provider OK" \
      >"$RUNTIME/prompt-$provider.out" 2>"$RUNTIME/prompt-$provider.err"; then
      kill "$RW" 2>/dev/null || true
      if grep -Eqi 'model|unsupported' "$RUNTIME/prompt-$provider.err"; then
        echo "UNTESTED: $provider has no configured model"
        REAL_UNTESTED=$((REAL_UNTESTED + 1))
        continue
      fi
      echo "FAIL: $provider remote prompt failed"; cat "$RUNTIME/prompt-$provider.err"; exit 1
    fi
    wait "$RW"
    grep -q $'turn_completed\tCompleted' "$RUNTIME/chat-$provider.out" || {
      echo "FAIL: $provider remote turn did not complete"; exit 1;
    }
    grep -qi "P5 $provider OK" "$RUNTIME/chat-$provider.out" || {
      echo "FAIL: $provider remote text missing"; exit 1;
    }
    "$BIN/fleetctl" --host test stop "$REAL_SES" >/dev/null
    echo "PASS: real remote chat $provider"
  done
  [[ "$REAL_UNTESTED" == "0" ]] || { echo "FAIL: $REAL_UNTESTED real provider smokes remain UNTESTED"; exit 1; }
fi

echo "PASS: SSH transport parity, streaming, two clients, disconnect survival, replay and security errors"
