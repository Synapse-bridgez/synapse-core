# Security Policy

## Reporting a Vulnerability

Please **do not** open a public GitHub issue for security vulnerabilities.

Instead, report it privately using one of the following channels:

1. **GitHub Private Vulnerability Reporting** — go to the "Security" tab of this
   repository and select "Report a vulnerability". This creates a private
   advisory visible only to maintainers.
2. If private reporting is unavailable, email the maintainers listed in
   `CONTRIBUTING.md` with the subject line `SECURITY:` and details of the issue.

## What to Include

- A description of the vulnerability and its potential impact.
- Steps to reproduce (proof-of-concept code or requests, if applicable).
- Affected version(s) / commit hash.

## Triage Process

- Reports are labeled `security` and triaged by maintainers as private
  GitHub Security Advisories (not public issues).
- We aim to acknowledge new reports within 5 business days.
- Once a fix is available, a coordinated disclosure and patch release will
  follow before public details are shared.

## Scope

This includes, but is not limited to: authentication/authorization bypass,
tenant data isolation issues, secret/credential exposure, and financial data
integrity issues (Stellar/fiat conversion paths).

## Secret scanning guardrails

The repository runs a secret-pattern scan in both local development and CI:

- `scripts/scan-secrets.sh` performs the repository scan.
- `.pre-commit-config.yaml` installs the hook for local commits.
- `.github/workflows/secrets-scan.yml` runs the same scanner in GitHub Actions.

Known false positives may be added to `.secrets-allowlist.txt`, but each entry
must be justified and kept minimal. This provides a documented, reviewable allowlist
mechanism without disabling the guardrail for real credentials.
