# Security policy

Gentle Fleet is experimental. No stable release or production security certification is implied. Linux is the reference platform; the Windows native daemon and runners remain subject to P7 acceptance, and macOS is not fully validated.

## Reporting a vulnerability

Use GitHub's private vulnerability reporting feature (Security → Report a vulnerability), once enabled. If it is unavailable, contact the repository maintainer privately to arrange a secure channel. Do not disclose exploit details, credentials or private logs in public issues.

Include the affected commit, OS, provider version, a minimal reproduction using disposable data, expected/actual behavior and impact. Redact secrets, account identifiers, paths and hostnames. Do not test against other people's accounts or machines without authorization.

The trust model and platform limits are documented in [docs/SECURITY_AND_PLATFORMS.md](docs/SECURITY_AND_PLATFORMS.md). Security fixes require regression evidence and must not weaken existing gates.
