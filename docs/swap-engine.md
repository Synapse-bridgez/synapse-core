# Swap Engine

The Swap Engine lives in [`src/swap/`](../src/swap). This page documents the
contracts its modules share. The Phase 1 → Phase 2 hand-off boundary is
described in [`docs/state-machine.md`](state-machine.md) (`### swap_ready`).

## Arithmetic and rounding

All quoting, fee, allocation and fill math goes through one module,
[`swap::decimal`](../src/swap/decimal.rs), and one rounding policy,
[`swap::rounding`](../src/swap/rounding.rs). Native floating point is never
used for money: a unit test (`swap_engine_source_contains_no_float_types`)
fails if `f32`/`f64` appears anywhere under `src/swap/`.

### `Decimal`

`Decimal` is an exact fixed-point value: an arbitrary-precision integer
number of units plus a scale (`value = units / 10^scale`). Addition and
subtraction are exact. Multiplication, division, basis-point scaling and
rescaling take an explicit `RoundingMode`, so every lossy step is visible in
the code.

### Wire format

Amounts and rates are **JSON strings**, never JSON numbers.

| Rule | Value |
|------|-------|
| Grammar | `-?[0-9]+(\.[0-9]+)?` (no exponent, no `+`, no `.5`, no separators) |
| Max length | 80 characters |
| Max fractional digits | 36 |
| Asset amounts | the asset's own precision (`Asset::decimals`; 7 for Stellar assets) |
| Exchange rates | 18 fractional digits (`RATE_SCALE`) |
| Basis-point figures | 2 fractional digits (`BPS_SCALE`) |

Serialisation always prints exactly `scale` fractional digits
(`"10.0000000"`), so precision round-trips. A client amount with more
fractional digits than the asset supports is **rejected**, not rounded.

### Rounding policy

Each rounding call names its purpose and the shared `RoundingPolicy` picks
the mode. The standard policy never promises or pays the client more than
the exact value, never undercharges a fee, and makes protection checks fail
safe:

| Purpose | Mode | Why |
|---------|------|-----|
| Quote output | Down | A quote never promises more than the route can deliver |
| Quote input | Up | A required input is never understated |
| Fee | Up | Fees are never undercharged |
| Fill | Down | A fill never credits more than was received |
| Allocation | Down + largest remainder | Shares always sum exactly to the total |
| Rate | Half-even | Displayed rates are unbiased; they never move money |
| Price impact | Ceiling (toward +∞) | Overstating impact makes the tolerance check fail safe |
| Protection floor | Ceiling (toward +∞) | The minimum acceptable output is never loosened |

### Allocation

`swap::allocation::allocate` splits an amount across weighted parts: each
share rounds down, and the leftover units go one at a time to the largest
remainders (ties to the lowest index). The parts always sum to the input and
the split is deterministic.

## Routing

`swap::routing::Router` lets the engine quote pairs that no single provider
trades directly. For each quote it:

1. **Builds a graph** of directed pairs from every registered
   `LiquidityProvider` (one edge per provider per pair; assets unknown to
   the `AssetRegistry` are ignored; a provider whose pair discovery fails is
   skipped).
2. **Discovers** simple paths (no asset visited twice), shortest first, up
   to `max_hops` legs (default 3) and `max_candidates` paths (default 32).
   Assets and providers are visited in sorted order, so enumeration is
   deterministic.
3. **Prices** each candidate by quoting each leg with the previous leg's
   output. A candidate with an unpriceable leg is dropped.
4. **Scores** a route as its expected output reduced by `hop_penalty_bps`
   (default 5) per leg beyond the first, a stand-in for the extra
   execution risk of every additional leg.
5. **Selects** the highest score; ties go to fewer hops, then to the
   smallest route key (`USDC>XLM@lp-a|XLM>EURC@lp-b`).

### Quote → execution

`swap::engine::SwapService` returns the chosen route inside the quote
(`route.hops[]` with provider, assets, per-leg amounts and fees; `route.path`;
`route.score`) together with `candidates_considered` / `candidates_priced`.

`execute` runs **exactly that route** — it never re-routes. Each leg's
actual fill becomes the next leg's input. If a leg fails, every filled leg is
reversed newest-first and the swap ends `compensated` (or
`compensation_failed`, which needs an operator). Executing a quote twice
returns the original swap; an expired quote is rejected with `410`.

## Swap protection

Clients can attach `protection` to a quote request and/or an execution
request:

```json
{ "min_received": "89.5000000", "max_slippage_bps": 50 }
```

| Field | Meaning |
|-------|---------|
| `min_received` | Absolute minimum of the destination asset (destination precision; more digits is a `400`). |
| `max_slippage_bps` | Maximum shortfall of the realized output versus the **reference rate** from the configured `PriceOracle`, `0`–`5000`. |

Both collapse into one **floor**:

```text
reference_out = amount_in × reference_rate
floor         = max(min_received, reference_out × (1 − max_slippage_bps / 10 000))   (rounded up)
```

Execution-time parameters can only **tighten** the quote's (higher
`min_received`, lower `max_slippage_bps`).

**Price impact** is `(reference_out − out) / reference_out × 10 000` in
signed bps (positive = worse than reference), rounded toward +∞. Quotes
report `reference_rate`, `price_impact_bps` and `min_out` (the floor);
settled swaps report the realized `price_impact_bps`.

Enforcement:

| Stage | Check | Outcome |
|-------|-------|---------|
| Quote | expected output < floor | `422 min_received_unattainable` or `422 price_impact_exceeded`; nothing stored |
| Execution start | reference re-read, floor recomputed; quoted output < floor | same `422`; no leg runs, no swap record |
| Intermediate leg | sent with a proportional `min_out`; after the fill the final output is projected along the remaining quoted legs | projection < floor → `protection.tripped` event, stop before the next leg, compensate |
| Final leg | sent with `min_out = floor`; realized < floor | compensate |

Aborted swaps end `compensated` with `failure.code` `slippage_exceeded` or
`min_received_not_met` (whichever parameter set the floor).

`max_slippage_bps` needs a reference price: with no oracle configured, or
the oracle failing, the request fails closed with
`503 reference_price_unavailable`. Without slippage protection an oracle
failure only drops `price_impact_bps` from the quote.

### HTTP

`swap::http::router(service)` exposes:

| Method | Path | Notes |
|--------|------|-------|
| POST | `/swap/quotes` | `{"source_asset","destination_asset","amount_in"}` |
| GET | `/swap/quotes/:id` | |
| POST | `/swap/executions` | `{"quote_id"}` |
| GET | `/swap/executions/:id` | swap record with per-leg status |
| GET | `/swap/executions/:id/events` | ordered event log |

Errors are `{"error":{"code","message"}}`: `400` invalid request / amount /
protection / unknown asset, `404` unknown quote or swap, `410` expired quote,
`422` no route or protection not attainable, `503` reference price
unavailable. The router is not mounted in `create_app` yet; Phase 1 still
defaults to `NoopSwapEngine`.
