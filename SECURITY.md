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

## Publishing Security Advisories

Applies to the independently versioned components consumed by third parties:
`synapse-sdk` (`sdks/rust/`) and `synapse-cli` (`cli/synapse-cli/`). Use the
template in [`docs/security/advisory-template.md`](docs/security/advisory-template.md).

### Checklist

Before the release:

- [ ] Open a **draft** GitHub Security Advisory from the template. Fill in
      affected versions, severity (CVSS v3.1) and CWE, and request a CVE from
      the advisory if the impact warrants one.
- [ ] Develop the fix in the advisory's temporary private fork, not in a
      public branch or PR.
- [ ] Pick the fixed version per the component's `VERSIONING.md`. A security
      fix that is backwards compatible is a PATCH release.
- [ ] Add a `### Security` entry under the new version in the component's
      `CHANGELOG.md` with a one-line impact summary and the advisory ID
      (`GHSA-xxxx-xxxx-xxxx`). Do not describe the exploit technique.
- [ ] Add the new version to `COMPATIBILITY.toml` and re-render
      (`cargo xtask compat-matrix render`). `cargo xtask release` refuses to
      tag a version without an entry.

Release:

- [ ] Merge the fix from the private fork, then run `cargo xtask release`
      for the new version and publish the crate.
- [ ] Confirm the fixed version is installable (`cargo install` /
      `cargo add`) before publishing anything.

After the release:

- [ ] Publish the advisory with the "Details" section kept high-level. GitHub
      notifies dependents through Dependabot.
- [ ] Link the advisory from the `CHANGELOG.md` entry if the ID changed.
- [ ] Once the embargo below has passed, expand "Details", link the fix
      commit, and complete the timeline.

### Disclosure timing (embargo)

- Nothing that identifies the vulnerable code path is public before the fixed
  version is released: no public issue, PR title or commit message.
- The advisory is published the same day as the fixed release, with impact,
  affected and fixed versions, and workarounds, but no exploit detail.
- Full technical details follow **14 days** after the release (7 days for
  Low severity), or earlier if the issue is already being exploited publicly.
- If no fix is possible within 90 days of the report, publish the advisory
  with workarounds only and keep the details embargoed until a fix ships.
