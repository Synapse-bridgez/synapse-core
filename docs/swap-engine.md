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
| Price impact | Up | Overstating impact makes the tolerance check fail safe |
| Protection floor | Up | The minimum acceptable output is never loosened |

### Allocation

`swap::allocation::allocate` splits an amount across weighted parts: each
share rounds down, and the leftover units go one at a time to the largest
remainders (ties to the lowest index). The parts always sum to the input and
the split is deterministic.
