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

## GitHub metadata inventory (preliminary, before this audit commit)

- Five branches; the four other branch tips were ancestors of `chore/public-readiness` at `373dd65`. The API returned 347 reachable commits in four pages. This inspected metadata, not all historical file contents.
- No tag refs or releases were returned. One draft PR, no independent issues; the inventory did not inspect all comments or historical deleted objects.
- One distinct non-noreply email was detected in commit author/committer metadata (value deliberately omitted). Decide whether the address may be public; changing Git's future email does not sanitize old commits.
- One commit message (`bfea8d6`) contains a reference to billing; no credential pattern was detected in that message. Normal commits cannot remove historical messages.
- 333 workflow runs were listed by count; individual logs and artifacts remain UNTESTED. Repository-level artifact listing was unavailable through the connector.
- All-history Gitleaks, snapshot Gitleaks, private settings, and clean builds remain UNTESTED.

## Reproducible local scan

Run from a private checkout of this branch with a current Gitleaks installed. Use a new mirror and a new private report directory outside both repositories. Example (PowerShell or Bash, adjust paths as needed):

```sh
git clone --mirror https://github.com/javijec/gentle-fleet.git ../fleet-audit.git
python scripts/public_audit.py ../fleet-audit.git --reports ../fleet-audit-reports
```

Check which refs the server actually advertised. If pull refs are not included, explicitly fetch the accessible PR heads into the disposable mirror before scanning. A mirror does not capture server-retained unreachable objects or private logs and artifacts. The helper scans Git history using unmodified default Gitleaks rules and checks each branch snapshot using a disposable detached worktree. Exit 0 means the automated checks found no candidate; 1 means findings or non-noreply metadata; 2 means a scan could not complete. Manual review remains mandatory in every case. Reports can still contain confidential information; share only aggregate counts and statuses, never raw findings. The helper has not been executed against this repository at the time of authoring.

If preserving all private history is unnecessary, consider a separate sanitized public repository starting from an audited snapshot while keeping the development repository private. Do not rewrite or delete historical references without explicit approval, a backup and a coordinated plan.
