# Swap operations console

`src/handlers/admin/swap_console.rs`; audit table `swap_console_audit_log` (update/delete rejected by trigger, hash-chained).

| Role | Read views | RequeueDeadLetter / AckAlert / ResumePair |
|---|---|---|
| read_only | yes | denied (denial is audited) |
| approver | yes | allowed with reason (>=10 chars) + explicit confirm |

Writes delegate to `ConsoleOps` (the same service layer automated paths use), are idempotent on `idempotency_key`, and are tenant-scoped via `visible_to`. Operators: confirm, give a reason, and check `verify_chain()` when auditing.
