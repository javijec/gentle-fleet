#!/usr/bin/env bash
# Linux-only deterministic merge gate for the P6 headless SDK.
set -euo pipefail
cd "$(git rev-parse --show-toplevel)"
export PATH="$PATH:/usr/sbin:/sbin"
unset GF_E2E_REAL GF_E2E_FAKE_ONLY
for tool in cargo python3 ssh sshd ssh-keygen ssh-keyscan timeout pgrep; do
  command -v "$tool" >/dev/null || { echo "BLOCKED: missing $tool" >&2; exit 1; }
done
cargo metadata --locked --format-version 1 | python3 -c '
import json, sys
data = json.load(sys.stdin)
for p in data["packages"]:
    name = p["name"].lower()
    if name == "gentle-fleet-desktop" or name == "gpui" or name.startswith("gpui-"):
        raise SystemExit("FAIL: graphical dependency in motor: " + name)
print("PASS: headless Cargo metadata")
'
cargo fmt --all -- --check
cargo check --workspace --locked
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --locked
cargo build --workspace --locked
for script in tests/*.sh; do bash -n "$script"; done
bash tests/e2e_fake.sh
bash tests/e2e_concurrency.sh
bash tests/e2e_backpressure.sh
bash tests/e2e_unix_permissions.sh
bash tests/e2e_persistence.sh
bash tests/e2e_approvals.sh
bash tests/e2e_p3_survival.sh
bash tests/e2e_p3_approval_disconnect.sh
bash tests/e2e_p3_identity.sh
bash tests/e2e_p3_provider_health.sh
bash tests/e2e_p3_runner_kill.sh
bash tests/e2e_p3_multi_session.sh
bash tests/e2e_long_replay.sh
GF_E2E_FAKE_ONLY=1 bash tests/e2e_p44_conformity.sh
GF_E2E_FAKE_ONLY=1 bash tests/e2e_p44_concurrency.sh
GF_E2E_FAKE_ONLY=1 bash tests/e2e_p44_security.sh
bash tests/e2e_p44_redaction.sh
bash tests/e2e_p44_approvals.sh
bash tests/e2e_p51_ssh.sh
bash tests/e2e_p52_two_hosts.sh
bash tests/e2e_p61_sdk.sh
echo "PASS: P6 headless deterministic gate (21 E2E scripts)"
