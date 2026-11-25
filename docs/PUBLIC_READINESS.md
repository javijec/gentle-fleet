# Public-readiness gate

**OPEN — keep the repository private until the gate is accepted.** This change sanitizes selected current files, not Git history. It does not change visibility, rewrite references, rotate credentials or certify P7.

## Before changing visibility

1. Obtain a fresh mirror clone in a private location: `git clone --mirror <repository-url> fleet-audit.git`. Enumerate branches, tags and pull-request refs; inspect historical commit metadata (including email addresses), messages and paths. Deleted refs, remote retention and cached objects may require additional review.
2. From the mirror run `gitleaks git --log-opts="--all" --redact .`. Use a current Gitleaks version and the default rules; investigate every finding without printing secrets. Also scan a clean checkout of every surviving branch using `gitleaks dir --redact .`. Do not confuse a failed scan with a clean result. Keep reports private.
3. Review raw captures, account metadata, personal paths, hostnames, logs, workflow artifacts, issues, PRs and comments. Review old development history and releases too. Rotate/revoke any actual credential before considering removal from history. If history cleanup is needed, plan and authorize it explicitly with backups and coordination; a normal commit is not sufficient.
4. Verify license and third-party notices, intended copyright attribution, and public build reproducibility from a clean checkout. Run the applicable full gates without changing P7 acceptance criteria.
5. Enable private vulnerability reporting, secret scanning and push protection where available. Review repository and workflow permissions, runners and branch protections.
6. Record evidence and remaining limitations for each item. Only then change visibility with owner approval.

## Current evidence

- Selected fixture records have synthetic account/quota values; event shapes and technical evidence remain.
- Personal SSH examples and billing details are generalized in selected documents; CI remains BLOCKED where previously documented.
- Historical scan, all surviving branch scans, GitHub metadata/artifact review, clean-clone build and security settings: **UNTESTED**.
- P7 acceptance remains independent and unchanged.

Do not merge or publish solely on the basis of this preparation.
