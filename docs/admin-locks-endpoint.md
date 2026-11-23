# Admin Locks Endpoint

The `/admin/locks` endpoint exposes the current lock state for the admin
dashboard. It is served by the admin handlers in `src/handlers/admin/`.

## Keyboard navigation

The admin dashboard surfaces three endpoints — `/admin/audit`,
`/admin/locks`, and `/admin/quota` — and each is reachable without a
pointing device. The guidance below applies to the dashboard views that
render these endpoints; the JSON payloads themselves are unchanged.

### Focus order

Focus moves in a single, predictable sequence so screen-reader users can
build a mental model of the page:

1. **Page heading** — announces the endpoint name (`Audit`, `Locks`, or
   `Quota`) and its current status.
2. **Summary region** — the live status line for the endpoint
   (`/admin/audit`: recent event count; `/admin/locks`: number of active
   locks; `/admin/quota`: current usage against the limit).
3. **Primary table or list** — the records returned by the endpoint.
   Rows are focusable and announced with their column headers.
4. **Row actions** — per-row controls (for example, releasing a lock on
   `/admin/locks` or refreshing a quota entry on `/admin/quota`).
5. **Pagination / refresh controls** — the trailing controls for the
   region.

Focus never jumps backwards within a region, and each region is wrapped
in a landmark so it can be skipped with the screen reader's landmark
navigation.

### Shortcuts

| Key | Action |
| --- | --- |
| `Tab` / `Shift+Tab` | Move forward / backward through the focus order above. |
| `Enter` | Activate the focused control (open a row, release a lock, refresh). |
| `Space` | Toggle selection on a focusable row. |
| `Arrow Up` / `Arrow Down` | Move between rows inside the primary table or list. |
| `Home` / `End` | Jump to the first / last row in the current table or list. |
| `Esc` | Close an open row detail or dismiss a confirmation dialog and return focus to the originating row. |

### Per-endpoint notes

- **`/admin/audit`** — the audit table is read-only; `Enter` on a row
  expands the full event detail. Focus returns to the row on `Esc`.
- **`/admin/locks`** — releasing a lock opens a confirmation dialog;
  focus is trapped in the dialog until it is confirmed or dismissed, then
  restored to the lock's row.
- **`/admin/quota`** — quota rows expose an inline refresh action; after
  it completes, the status line is announced via a live region without
  moving focus.

All shortcuts are documented in the dashboard's help panel, which is
itself reachable as the final stop in the focus order.
