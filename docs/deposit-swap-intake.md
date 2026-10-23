# Automatic deposit swap intake

The signed `/callback` and `/callback/transaction` endpoints atomically persist
the Phase 1 transaction, its creation audit, a durable callback receipt, and a
Phase 2 `swap_orders` row with status `swap_ready`. Phase 1 processing remains
pending; a successful anchor callback does not bypass its verification pipeline.
There is no execution, liquidity lookup, settlement, or public order API.

Enable the existing `swap_engine` flag and configure its `rollout_percentage` to
opt tenants into order preparation. The migration seeds it disabled. Database
flag lookup errors fail the callback so delivery can be retried.

The callback must have `callback_type: "deposit"`, `callback_status: "completed"`,
and a nonempty `anchor_transaction_id`. Its signed `stellar_account` must match
exactly one active tenant. Unknown or ambiguous accounts remain Phase 1 only;
tenant headers and metadata never determine ownership. Code-only callbacks must
resolve to exactly one enabled asset registry entry; ambiguous issuers and
unregistered/disabled assets do not create orders.

`SWAP_DEPOSIT_MIN_AMOUNT` (default `1`) and `SWAP_DEPOSIT_MAX_AMOUNT` (default
`1000000`) are positive decimal strings, compared exactly and inclusively.
Registry `assets.min_amount` and `assets.max_amount` further restrict those
bounds. Invalid process bounds fail intake for opted-in tenants. Out-of-range
deposits persist without creating orders.

Scoped callbacks with an anchor identity use the durable key
`(tenant_id, anchor_transaction_id, callback_type)`, independent of HTTP
idempotency headers. Concurrent retries serialize on a transaction-scoped
Postgres advisory lock. A retry returns the original Phase 1 ID; changes to
amount, asset, or account are rejected. A later completed callback can upgrade
an earlier pending callback without regressing the Phase 1 processing status.
An older pending retry cannot regress a completed callback. Receipts remain
active while the feature is off, so re-delivery after opt-in is safe.

`swap_orders.source_transaction_id` is unique. The source reference includes
`created_at` for the partitioned transaction key and `tenant_id` to enforce
ownership in the database. The same row supports traversal in both directions:
load an order to obtain its source, or use `order_for_transaction` to find the
order by Phase 1 ID. Both new tables force RLS, and scoped intake uses local
tenant settings that clear at commit/rollback. Any order-write failure rolls
back the transaction, receipt, and audit together. Missing monthly partitions
use the existing self-healing function before retrying the complete unit.

Source foreign keys intentionally retain linked transaction partitions. The
scheduled partition archive defers a partition when PostgreSQL reports retained
references and continues with other partitions. Archive linked receipts/orders
and their source together before removing them; deleting receipts alone loses
callback replay protection. Other retention/export workflows must respect these
foreign keys as well. Monitor archive deferral logs and storage growth.

Unit tests cover completion/identity requirements and exact decimal bounds.
`deposit_swap_intake_test` exercises signed HTTP callbacks, duplicate and
concurrent delivery, feature gating, registry/amount exclusions, retry mutation,
and pending-to-completed delivery against Postgres. Tests are authored but were
not executed as requested.
