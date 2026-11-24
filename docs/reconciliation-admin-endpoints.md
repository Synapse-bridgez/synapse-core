# Reconciliation admin endpoints

## `GET /admin/reconciliation/reports`

Lists stored reconciliation report summaries, newest first.

| Query parameter | Default | Meaning |
|-----------------|---------|---------|
| `limit` | `20` | Page size. |
| `offset` | `0` | Rows to skip. |
| `with_total` | `false` | When `true`, also run a `COUNT(*)` and return the total number of reports in `total`. |

The count is the expensive half of the request, so it is only run when `with_total=true`. Without it the handler issues one query (the page) and `total` is `null`. The response keeps the same shape either way:

```json
{ "reports": [ ... ], "total": null, "limit": 20, "offset": 0 }
```

```json
{ "reports": [ ... ], "total": 42, "limit": 20, "offset": 0 }
```

Clients that need the total once (to render a page count) should request it on the first page and reuse it for later pages.
