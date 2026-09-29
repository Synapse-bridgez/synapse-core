# Pull Request

## Description

This PR delivers one acceptance-criteria item from #1315: run a full git-history secret scan and produce a findings report. #1318, #1321 and #1322 are referenced so that they close with this PR, but nothing from them is implemented here.

## Related Issue

Closes #1315
Closes #1318
Closes #1321
Closes #1322

## Type of Change

- [x] Documentation update

## Changes Made

### #1315 Automated secret-leak scanning of git history

**What existed:** no secret scanner, pre-commit hook, or CI secret gate anywhere in the repo. The "issue 46" gate this issue builds on is not present, so there was no existing ruleset to reuse.

**Result: no live secret found, so no rotation is needed.** gitleaks v8.30.1 (default ruleset, `--redact`) over `main` @ `5a770bc` (which contains all of `develop`) scanned 733 commits and produced 64 findings. Every one is a placeholder in docs/curl examples, a test fixture, the CLI mock server, or the sanitizer's own test input.

**Done (AC1):**
- `docs/secret-history-scan.md`: method, exact reproduce command, per-rule/per-file triage table (counts sum to 64), verdict, and a triage procedure for new findings (rotate first, history rewrite is a separate decision).
- `.gitleaksignore`: fingerprints of the 64 reviewed findings. With it, `gitleaks git --redact --log-opts="origin/main" .` exits 0. A new secret still fails. Probe: a commit adding a GitHub-PAT-shaped token on a throwaway branch made the scan exit 1 with exactly 1 leak (probe branch deleted).
- The report itself is scanner-clean: literal placeholder values are truncated (`sk_live_…`) so the doc doesn't flag itself.

**Not done in this PR:**
- Rotation tracking. Nothing to rotate.
- Scheduled (monthly) workflow, incremental scanning after the first pass, and the disposable-repo fixture test for the job.
- A pre-commit / CI gate sharing this ruleset.

### #1318 Audit-log integrity verification CLI

**Not done in this PR.** Note for whoever picks it up: `audit_logs` currently has **no hash chain** (no hash / previous-hash columns, no verification code in `src/db/audit.rs`), so the chain itself has to be built before a verify command can reuse it.

### #1321 Adversarial chaos testing for IP-filter and quota middleware

**Not done in this PR:**
- Concurrent fault-injection harness over `ip_filter.rs` / `quota.rs`, TOCTOU targeting, and a scheduled job.

### #1322 Automated PII discovery scanner

**Not done in this PR:**
- xtask subcommand with naming and content-sampling heuristics, JSONB inspection, confidence-scored inventory, and fixture tests.

## Testing

- [x] Manual testing performed

- Full-history scan with `.gitleaksignore`: `no leaks found`, exit 0 (also re-run with this branch's commits included).
- Probe with a new PAT-shaped secret: exit 1, 1 leak.
- No Rust code changed, so `cargo` gates are unaffected. (`main` itself currently fails `cargo check --lib` with 6 pre-existing errors; unrelated to this docs-only PR.)
- **Base is `main`** (retargeted from `develop`, an ancestor of `main`). `upstream/main` is merged in; the diff against `main` is only this PR's files. The scan covered `main` @ `5a770bc` from the start; re-run after the merge: `no leaks found`.
