# Swap directions (on-ramp / off-ramp)

`direction` is data on `swap_orders` and `swap_quotes` (`fiat_to_stellar` default, `stellar_to_fiat`). Code: `src/swap/direction.rs`.

| Concern | fiat_to_stellar | stellar_to_fiat |
|---|---|---|
| Quote / execution / risk / tenant limits | shared | shared |
| Anchor interaction | on-ramp (existing) | off-ramp callback/redirect |
| Settlement confirmation | Stellar credit | anchor `completed` callback |
| Failure after debit | existing compensation | `Compensating` -> `Refunded` |
| Cancel | existing | `Quoted` -> `Cancelled`; after debit -> compensation |

Callbacks are idempotent on `(order_id, callback_id)` (`off_ramp_callbacks`). Amounts below the anchor minimum and unsupported asset/currency pairs are rejected by `validate_offramp`.
