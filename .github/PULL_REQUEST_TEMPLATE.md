## Description

<!-- Describe the change and the motivation behind it. -->

## Checklist

- [ ] Tests added/updated for the change
- [ ] Documentation updated (if applicable)
- [ ] **OpenAPI spec drift acknowledged** — if this PR changes `utoipa` route annotations in `src/handlers/` (or any other spec-producing surface), the generated OpenAPI spec will drift from the checked-in baseline. Confirm one of the following:
  - [ ] No spec-affecting changes were made (no route/annotation/schema changes).
  - [ ] Spec changes are **additive only** (new fields, new endpoints, new optional params). The SDK surface has been updated to reflect them, or a follow-up issue has been filed and linked here.
  - [ ] Spec changes are **breaking** (removed/renamed fields, changed types, removed endpoints). The SDK surface has been updated in this PR and the breaking change is called out in the description above.
- [ ] Internal/admin routes that are intentionally undocumented are excluded from the drift check (see `scripts/openapi-drift/`), so their changes are not flagged as SDK-relevant drift.

## Related Issues

<!-- Link the issue(s) this PR closes, e.g. Closes #1342 -->
