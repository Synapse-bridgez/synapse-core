# SOC 2 Control Mapping and Evidence Collection

- `controls.json` is the **source of truth** (schema_version 1). Each control
  lists its implementation, enforcing files, evidence sources, owner, and, for
  `partial`/`gap` controls, a mandatory `finding`.
- `control-matrix.md` is **generated**: `cargo run -p xtask -- compliance --render docs/compliance/control-matrix.md`.
  CI (`compliance-evidence.yml`) and a unit test fail if it drifts.
- Evidence: `cargo run -p xtask -- compliance --period 2026-10 --out compliance-evidence`
  writes one artifact per `(control id, period)` (idempotent: reruns overwrite).
  Artifacts record only source paths, presence, and sizes, never file contents,
  so no secrets or log data are copied. A missing required source, a `gap`, or a
  missing enforcing file produces a `fail` artifact with findings and a non-zero
  exit, never an empty pass.
- Honesty rules: `automation: automated` is allowed only with a
  machine-checkable (`file`/`ci_job`) source, and gaps are listed as open findings
  with owners instead of being omitted. Presence checks prove an evidence source
  exists; they do not prove the control operated, which still needs the
  referenced CI run history, logs, and metrics.
- Retention target is 400 days; Actions artifacts are kept 90 days (open finding
  `FIND-RETENTION-1`).
- Tests: `cargo test -p xtask compliance` (schema validation, every non-gap
  control resolves to existing evidence, failure injection for a missing source).
