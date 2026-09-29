# Maintainer Office Hours

Recurring, topic-tagged office hours for Wave contributors. This directory is the
version-controlled, searchable archive of every session — so contributors who
can't attend live still benefit from what was discussed.

## Schedule

Office hours run on a recurring cadence. The canonical calendar entry is
generated from [`schedule.yml`](./schedule.yml) and published by the
[`office-hours-reminder`](../../../.github/workflows/office-hours-reminder.yml)
workflow, which posts a reminder before each session.

| Field | Value |
| --- | --- |
| Cadence | Weekly (see `schedule.yml` for the current day/time) |
| Duration | 60 minutes |
| Host | Rotating maintainer (see `schedule.yml`) |
| Call tooling | Whatever the maintainers already use (out of scope here) |

To change the cadence, host rotation, or skip a session, edit
[`schedule.yml`](./schedule.yml) — do not hand-edit the generated calendar.

## Archive layout

```
docs/community/office-hours/
├── README.md          # this file
├── schedule.yml       # recurring schedule + skipped sessions
├── template.md        # structured notes template
├── index.md           # generated, topic-tagged index
└── sessions/
    ├── 2024-01-15-architecture.md
    └── 2024-01-22-skipped.md
```

Each session is a plain markdown file with consistent front matter so the same
indexing approach used for [ADRs](../../adr/README.md) can be reused here.

## Front matter

```yaml
---
date: 2024-01-15
topics: [architecture, governance]
attendees: 7
recording: https://example.com/recording
status: held   # held | skipped
---
```

- `topics` — lowercase tags used to build the searchable index.
- `attendees` — head count (use `0` for a skipped session).
- `status` — `held` for a normal session, `skipped` for a cancelled one.

## Adding a session

1. Copy [`template.md`](./template.md) into `sessions/` as
   `YYYY-MM-DD-<slug>.md`.
2. Fill in the front matter and notes.
3. Run the archiver to regenerate the topic-tagged index:

   ```sh
   node scripts/office-hours/archive.mjs
   ```

   The archiver reads every file in `sessions/`, validates the front matter,
   and rewrites [`index.md`](./index.md) grouped by topic. It fails loudly on
   malformed front matter so the archive never silently drifts.

## Cancelled / skipped sessions

A cancelled session is **not** an ambiguous gap. Add a session file with
`status: skipped` (see `sessions/2024-01-22-skipped.md`) and mark the date as
skipped in `schedule.yml`. The archiver lists skipped sessions separately so the
schedule stays continuous and auditable.

## Finding past discussions

Browse [`index.md`](./index.md) and search by topic tag, or grep the archive:

```sh
grep -rl 'topics:.*architecture' docs/community/office-hours/sessions/
```
