# Audit & Compliance Admin Endpoints

This document describes the admin endpoints used to inspect audit and
compliance data.

## `GET /admin/compliance`

Returns compliance records, optionally filtered by status.

### Query parameters

| Parameter | Type   | Required | Description                                              |
|-----------|--------|----------|----------------------------------------------------------|
| `status`  | string | No       | Filter records by compliance status. See accepted values. |
| `limit`   | number | No       | Maximum number of records to return.                     |
| `offset`  | number | No       | Number of records to skip.                               |

### Accepted `status` values

The `status` filter is validated against the known compliance statuses. Any
other value is rejected with `400 Bad Request` and an error message listing
the accepted values.

- `pending`
- `in_review`
- `approved`
- `rejected`
- `expired`

### Responses

- `200 OK` — a list of compliance records matching the filter.
- `400 Bad Request` — the `status` value is not one of the accepted values
  above. The error message names the allowed values.
