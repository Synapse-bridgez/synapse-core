# Maintainer Rotation & On-Call Governance

This document defines how maintainer triage and incident-response duties are
rotated, and how the current and upcoming rotation is published so contributors
know who is responsible without having to ask.

## Schedule format

The rotation is stored as a simple, version-controlled structured file at
[`docs/governance/rotation.yml`](./rotation.yml). Because the schedule is just a
file in the repo, **swaps and coverage requests are ordinary pull requests** —
open a PR that edits the relevant entry and get a maintainer to approve it.

Each entry describes *who*, *what role*, and *what period*:

```yaml
- maintainer: octocat          # GitHub handle of the person on rotation
  role: triage                 # triage | review | incident
  start: 2024-06-03            # inclusive, ISO-8601 date
  end: 2024-06-09              # inclusive, ISO-8601 date
```

- `maintainer` — GitHub handle responsible for the period.
- `role` — one of `triage`, `review`, or `incident`.
- `start` / `end` — inclusive ISO-8601 dates (`YYYY-MM-DD`).

## Resolving the current rotation

The publishing tool reads `rotation.yml` and resolves, for each role, the entry
whose period contains today's date (the *current* assignee) and the next entry
that starts after today (the *upcoming* assignee).

### Rotation gaps

If no entry covers today for a given role, the tool **surfaces the gap
explicitly** rather than silently defaulting to an unspecified fallback. The
published view shows `⚠️ UNASSIGNED — rotation gap` for that role so the missing
coverage is visible and can be filled by a PR.

## Publishing

The current and upcoming rotation is published to a visible location:

- This docs page (regenerated from `rotation.yml`).
- A scheduled GitHub Actions job (`.github/workflows/`) that posts the current
  rotation status and flags any gaps.

## Requesting coverage / swaps

1. Fork or branch the repo.
2. Edit the relevant entry in `docs/governance/rotation.yml` (change the
   `maintainer`, or adjust `start`/`end` to hand off coverage).
3. Open a PR describing the swap and request review from the affected
   maintainers.

Once merged, the next scheduled publish reflects the change automatically.

## Current rotation

<!-- This table is generated from rotation.yml by the publishing tool. -->

| Role | Current | Upcoming |
| --- | --- | --- |
| triage | _see published status_ | _see published status_ |
| review | _see published status_ | _see published status_ |
| incident | _see published status_ | _see published status_ |
