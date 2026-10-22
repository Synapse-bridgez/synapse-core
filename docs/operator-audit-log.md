# Operator Audit Log

Durable, tamper-evident record of administrative actions against the swap
engine (#1518). Code: `src/audit/mod.rs`; schema:
`migrations/20261021000001_operator_audit_log.sql`.

## Audited actions

| `action` | High impact (justification required) |
|----------|--------------------------------------|
| `kill_switch_toggle` | yes |
| `circuit_breaker_override` | yes |
| `manual_order_intervention` | yes |
| `config_change` | no |
| `replay_trigger` | no |
| `limit_adjustment` | no |

Each record stores actor, source (IP or client), action, target, before/after
state (JSON), justification, timestamp, `prev_hash` and `hash`.

## Writing records

All administrative mutations go through `audit::dispatch(pool, principal, cmd, action)`.
It validates the command, appends the audit record and runs `action` in the
**same transaction**, so the action and its record commit or roll back together.
Actions that are not transactional (e.g. an in-memory switch) call `dispatch`
first and apply the change only after it returns `Ok`.

- **Failure policy: fail closed.** If the record cannot be written (missing
  justification, database error) the action is not performed and the error is
  returned. Nothing fails silently.
- **Identity** comes from `OperatorPrincipal::authenticated(actor, source)`,
  built from the authenticated admin principal and the trusted-proxy-resolved
  client address - never from a request body field.

## Tamper evidence

- `hash = SHA-256(prev_hash || canonical JSON of the record)`; the first record
  chains from `GENESIS_HASH` (64 zeros). Writers are serialised with a
  transaction-scoped advisory lock, so the chain cannot fork.
- Triggers reject `UPDATE`, `DELETE` and `TRUNCATE`. Edits or deletions that
  bypass them (e.g. a superuser disabling triggers) break the chain and are
  reported by `audit::verify_chain`.
- The table is not partitioned and is not touched by partition retention jobs,
  so records outlive the routine retention window.

## Admin endpoints

Both sit behind `admin_auth`; they are read-only. No HTTP endpoint writes audit records.

- `GET /admin/operator-audit?actor=&action=&from=&to=&after_seq=&limit=` -
  filtered list, oldest first; page with `next_after_seq`.
- `GET /admin/operator-audit/export` - full chain as NDJSON for periodic
  internal-controls review. `X-Audit-Chain-Valid: true|false`; on failure
  `X-Audit-Chain-First-Invalid-Seq` names the first broken record.
