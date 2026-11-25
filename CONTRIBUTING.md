# Contributing

Read [AGENTS.md](AGENTS.md), [architecture](docs/ARCHITECTURE.md), [roadmap](docs/ROADMAP.md) and [quality gates](docs/QUALITY.md). Work on a focused branch and include the relevant deterministic regression tests. Preserve provider capability boundaries and do not claim support beyond tested platforms.

Run formatting, check, Clippy, tests and build with the locked workspace; follow [local CI](docs/LOCAL_CI.md) for the full native checks. Use isolated fixtures, never production credentials or a real user's session history. Real-provider tests are opt-in. Document PASS, FAIL, BLOCKED and UNTESTED separately.

Do not commit secrets, runtime data, personal paths or raw captures. Scan proposed commits with Gitleaks and manually inspect fixtures. Report security issues privately via [SECURITY.md](SECURITY.md).

By contributing, you agree to license your contribution under the repository's MIT license, subject to existing third-party notices.

Follow the [community code of conduct](CODE_OF_CONDUCT.md).
