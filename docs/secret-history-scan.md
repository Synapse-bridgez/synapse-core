# Git History Secret Scan

Full-history scan for secrets committed before any secret-scanning gate
existed. Issue #1315.

## Result

**No live secret found.** All 64 findings are placeholder values in
documentation, test fixtures, or a mock server. No credential rotation is
required.

The reviewed findings are recorded by fingerprint in `.gitleaksignore`. With
that file in place the scan below exits `0`, and any **new** finding still
fails it (exit `1`).

## Method

| | |
|---|---|
| Scanner | [gitleaks](https://github.com/gitleaks/gitleaks) v8.30.1, default ruleset |
| Refs | `main` at `5a770bc` (`develop` is an ancestor of `main`, so it adds no commits) |
| Commits scanned | 733. gitleaks scans each non-merge commit's patch. Merge commits' combined diffs are not scanned. |
| Output | `--redact`: secret values never appear in the report |

There is no pre-commit or CI secret-scanning gate in this repository yet, so
there was no existing ruleset to reuse. The gitleaks defaults were used, and
a future gate should use the same scanner and `.gitleaksignore` so the two
agree.

Reproduce from a full clone:

```bash
gitleaks git --redact --log-opts="origin/main" .
```

## Findings

| Rule | Where | Count | What it is |
|---|---|---|---|
| `curl-auth-header` | `docs/*.md`, `NEXT_STEPS.md`, `PR_DESCRIPTION.md`, `src/Multi-Tenant Isolation Layer (Architecture)/*` | 33 | curl examples using `dev-admin-key`, `<admin-token>`, `your-api-key`, `demo_api_key_anchor_platform_001`, `test_api_key_partner_002`, `inactive_api_key_003`, `$TENANT1_KEY`, and example `X-Idempotency-Key` values |
| `generic-api-key` | `docs/*.md`, `src/auth/idempotency.md`, `NEXT_STEPS.md`, `PR_DESCRIPTION.md`, `src/Multi-Tenant Isolation Layer (Architecture)/*` | 17 | Example idempotency keys (`anchor-tx-12345`, a UUID) and an `sk_live_…` placeholder for `SYNAPSE_API_KEY` in `docs/CLI.md` |
| `generic-api-key` | `cli/synapse-cli/src/bin/mock-server.rs` | 6 | Mock server scenario idempotency keys and a fixed UUID token |
| `generic-api-key` | `tests/*.rs`, `src/ws/health.rs` (test module) | 7 | Test fixtures (`sk_live_initial_key_…`, `test_api_key_…`, `secret_api_key_…`, `test-key-duplicate-…`) |
| `stripe-access-token` | `src/utils/sanitize.rs` (test module) | 1 | An `sk_live_…` sample string, the input for the sanitizer's own redaction test |

Several documentation examples use `sk_live_`-prefixed placeholders. They are
not real keys, but that prefix is what scanners and people look for. Prefer
`sk_test_example` or `<api-key>` in new docs.

## Triaging a new finding

1. Look at the matched line: `gitleaks git -v --log-opts=<range> .` (drop
   `--redact` locally only).
2. **Live secret:** rotate it immediately and track the rotation separately.
   Do not wait on a history rewrite. Removing it from history is a separate
   maintainer decision.
3. **Placeholder or fixture:** add its `Fingerprint` to `.gitleaksignore`
   with the same review, or change the example to an obvious placeholder.
