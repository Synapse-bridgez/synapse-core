//! End-to-end Swap Engine lifecycles, quote request → settlement or
//! compensation, driven through the HTTP API against mock liquidity
//! providers, a mock price oracle and a test database.
//!
//! Everything is deterministic: a fixed seed (override with
//! `SWAP_E2E_SEED`) drives every random choice, and the clock and ids are
//! simulated. A failing test prints its seed and a repro command.
//!
//! The Postgres-backed variant needs Docker and is `#[ignore]`d like the
//! other testcontainers suites; CI runs it in the `--ignored` step.

mod swap_e2e;

use serde_json::{json, Value};
use swap_e2e::*;
use synapse_core::swap::decimal::Decimal;
use synapse_core::swap::rounding::RoundingMode;

async fn harness(cfg: HarnessConfig) -> SwapHarness {
    SwapHarness::start(cfg).await
}

fn standard() -> HarnessConfig {
    HarnessConfig::standard(seed_from_env())
}

fn kinds(events: &[Value]) -> Vec<String> {
    events
        .iter()
        .map(|e| e["kind"].as_str().unwrap().to_string())
        .collect()
}

fn route_key(v: &Value) -> String {
    v["route"]["hops"]
        .as_array()
        .unwrap()
        .iter()
        .map(|h| {
            format!(
                "{}>{}@{}",
                h["from"]["code"].as_str().unwrap(),
                h["to"]["code"].as_str().unwrap(),
                h["provider"].as_str().unwrap()
            )
        })
        .collect::<Vec<_>>()
        .join("|")
}

// ---------------------------------------------------------------------------
// Scripted lifecycles
// ---------------------------------------------------------------------------

#[tokio::test]
async fn direct_swap_quotes_executes_and_settles() {
    let h = harness(standard()).await;
    let _g = h.guard("direct_swap_quotes_executes_and_settles");

    let (status, quote) = h.quote("USDC", "XLM", "100", None).await;
    assert_eq!(status, 200, "{quote}");
    assert_eq!(route_key(&quote), "USDC>XLM@lp-alpha");
    // 100 USDC - 30 bps fee = 99.7 × 9.98, rounded down at 7 decimals.
    assert_eq!(quote["expected_out"], "995.0060000");
    assert_eq!(quote["amount_in"], "100.0000000");

    let (status, swap) = h.execute(quote["quote_id"].as_str().unwrap(), None).await;
    assert_eq!(status, 200, "{swap}");
    assert_eq!(swap["status"], "settled");
    assert_eq!(swap["amount_out"], "995.0060000");
    let events = h.events(swap["swap_id"].as_str().unwrap()).await;
    assert_eq!(kinds(&events), ["swap.executing", "leg.filled", "swap.settled"]);
    assert_swap_invariants(&quote, &swap, &events);
    assert_inventory_conserved(&h, &[swap]);
}

#[tokio::test]
async fn multi_hop_route_beats_direct_pair_and_is_carried_through_execution() {
    let h = harness(standard()).await;
    let _g = h.guard("multi_hop_route_beats_direct_pair_and_is_carried_through_execution");

    let (status, quote) = h.quote("USDC", "EURC", "100", None).await;
    assert_eq!(status, 200, "{quote}");
    // lp-gamma trades USDC/EURC directly but at 80 bps; two cheaper hops win.
    assert_eq!(route_key(&quote), "USDC>XLM@lp-alpha|XLM>EURC@lp-beta");
    assert_eq!(quote["candidates_priced"], 2);

    let (_, swap) = h.execute(quote["quote_id"].as_str().unwrap(), None).await;
    assert_eq!(swap["status"], "settled", "{swap}");
    // Providers saw exactly the quoted route, in order.
    assert_eq!(h.lp("lp-alpha").execution_count(), 1);
    assert_eq!(h.lp("lp-beta").execution_count(), 1);
    assert_eq!(h.lp("lp-gamma").execution_count(), 0);
    let events = h.events(swap["swap_id"].as_str().unwrap()).await;
    assert_swap_invariants(&quote, &swap, &events);
    assert_inventory_conserved(&h, &[swap]);
}

#[tokio::test]
async fn pair_with_no_direct_market_is_served_via_multi_hop() {
    let h = harness(standard()).await;
    let _g = h.guard("pair_with_no_direct_market_is_served_via_multi_hop");

    // Nobody trades XLM/BTC; BTC also has 8 decimals vs XLM's 7.
    let (status, quote) = h.quote("XLM", "BTC", "50000", None).await;
    assert_eq!(status, 200, "{quote}");
    assert_eq!(route_key(&quote), "XLM>USDC@lp-alpha|USDC>BTC@lp-gamma");
    assert_eq!(quote["expected_out"].as_str().unwrap().split('.').nth(1).unwrap().len(), 8);

    let (_, swap) = h.execute(quote["quote_id"].as_str().unwrap(), None).await;
    assert_eq!(swap["status"], "settled", "{swap}");
    let events = h.events(swap["swap_id"].as_str().unwrap()).await;
    assert_swap_invariants(&quote, &swap, &events);
}

#[tokio::test]
async fn failed_second_leg_is_compensated_and_inventory_unwound() {
    let h = harness(standard()).await;
    let _g = h.guard("failed_second_leg_is_compensated_and_inventory_unwound");
    h.lp("lp-beta").script(1, Fault::Fail);

    let (_, quote) = h.quote("USDC", "EURC", "250", None).await;
    let (status, swap) = h.execute(quote["quote_id"].as_str().unwrap(), None).await;
    assert_eq!(status, 200);
    assert_eq!(swap["status"], "compensated", "{swap}");
    assert_eq!(swap["failure"]["code"], "leg_failed");
    assert_eq!(swap["failure"]["leg_index"], 1);
    assert_eq!(swap["legs"][0]["status"], "compensated");
    assert_eq!(swap["legs"][1]["status"], "failed");

    let events = h.events(swap["swap_id"].as_str().unwrap()).await;
    assert_eq!(
        kinds(&events),
        [
            "swap.executing",
            "leg.filled",
            "leg.failed",
            "swap.compensating",
            "leg.compensated",
            "swap.compensated"
        ]
    );
    assert_swap_invariants(&quote, &swap, &events);
    // Nothing settled, so every provider is back where it started.
    assert_inventory_conserved(&h, &[]);
}

#[tokio::test]
async fn failed_reversal_ends_compensation_failed() {
    let h = harness(standard()).await;
    let _g = h.guard("failed_reversal_ends_compensation_failed");
    h.lp("lp-beta").script(1, Fault::Fail);
    h.lp("lp-alpha").fail_compensations(true);

    let (_, quote) = h.quote("USDC", "EURC", "10", None).await;
    let (_, swap) = h.execute(quote["quote_id"].as_str().unwrap(), None).await;
    assert_eq!(swap["status"], "compensation_failed", "{swap}");
    assert_eq!(swap["legs"][0]["status"], "compensation_failed");
    let events = h.events(swap["swap_id"].as_str().unwrap()).await;
    assert_eq!(events.last().unwrap()["kind"], "swap.compensation_failed");
}

// ---------------------------------------------------------------------------
// Protection
// ---------------------------------------------------------------------------

#[tokio::test]
async fn quote_beyond_slippage_tolerance_is_short_circuited() {
    let h = harness(standard()).await;
    let _g = h.guard("quote_beyond_slippage_tolerance_is_short_circuited");

    // Best route is ~90 bps worse than the oracle's 1/1.08.
    let (status, body) = h
        .quote("USDC", "EURC", "100", Some(json!({ "max_slippage_bps": 50 })))
        .await;
    assert_eq!(status, 422, "{body}");
    assert_eq!(body["error"]["code"], "price_impact_exceeded");

    let (status, quote) = h
        .quote("USDC", "EURC", "100", Some(json!({ "max_slippage_bps": 150 })))
        .await;
    assert_eq!(status, 200, "{quote}");
    let impact = dec(&quote["price_impact_bps"]);
    assert!(impact > Decimal::parse("50").unwrap() && impact < Decimal::parse("150").unwrap());
    assert!(dec(&quote["expected_out"]) >= dec(&quote["min_out"]));
    // Nothing executed for the refused quote.
    assert!(h.lps.values().all(|lp| lp.execution_count() == 0));
}

#[tokio::test]
async fn unattainable_min_received_is_refused_at_quote_time() {
    let h = harness(standard()).await;
    let _g = h.guard("unattainable_min_received_is_refused_at_quote_time");
    let (status, body) = h
        .quote("USDC", "XLM", "100", Some(json!({ "min_received": "995.0060001" })))
        .await;
    assert_eq!(status, 422, "{body}");
    assert_eq!(body["error"]["code"], "min_received_unattainable");
    let (status, _) = h
        .quote("USDC", "XLM", "100", Some(json!({ "min_received": "995.006" })))
        .await;
    assert_eq!(status, 200);
}

#[tokio::test]
async fn drift_on_final_leg_beyond_tolerance_aborts_and_compensates() {
    let h = harness(standard()).await;
    let _g = h.guard("drift_on_final_leg_beyond_tolerance_aborts_and_compensates");
    h.lp("lp-beta").script(1, Fault::Drift(100));

    let (_, quote) = h
        .quote("USDC", "EURC", "100", Some(json!({ "max_slippage_bps": 150 })))
        .await;
    let (_, swap) = h.execute(quote["quote_id"].as_str().unwrap(), None).await;
    assert_eq!(swap["status"], "compensated", "{swap}");
    assert_eq!(swap["failure"]["code"], "slippage_exceeded");
    assert_eq!(swap["legs"][0]["status"], "compensated");
    let events = h.events(swap["swap_id"].as_str().unwrap()).await;
    assert_swap_invariants(&quote, &swap, &events);
    assert_inventory_conserved(&h, &[]);
}

#[tokio::test]
async fn intermediate_drift_short_circuits_before_the_next_leg() {
    // lp-alpha ignores min_out, so the engine's own projection must catch it.
    let cfg = standard().map_lp("lp-alpha", |mut c| {
        c.honours_min_out = false;
        c
    });
    let h = harness(cfg).await;
    let _g = h.guard("intermediate_drift_short_circuits_before_the_next_leg");
    h.lp("lp-alpha").script(1, Fault::Drift(300));

    let (_, quote) = h
        .quote("USDC", "EURC", "100", Some(json!({ "max_slippage_bps": 150 })))
        .await;
    let (_, swap) = h.execute(quote["quote_id"].as_str().unwrap(), None).await;
    assert_eq!(swap["status"], "compensated", "{swap}");
    assert_eq!(swap["legs"][1]["status"], "skipped");
    assert_eq!(h.lp("lp-beta").execution_count(), 0, "second leg must not run");
    let events = h.events(swap["swap_id"].as_str().unwrap()).await;
    assert!(kinds(&events).contains(&"protection.tripped".to_string()));
    assert_inventory_conserved(&h, &[]);
}

#[tokio::test]
async fn reference_move_between_quote_and_execution_short_circuits() {
    let h = harness(standard()).await;
    let _g = h.guard("reference_move_between_quote_and_execution_short_circuits");
    let (_, quote) = h
        .quote("USDC", "EURC", "100", Some(json!({ "max_slippage_bps": 150 })))
        .await;
    // EURC weakens 2% on the oracle: the fair output rises and the quoted
    // route no longer clears the floor.
    h.oracle.set_price("EURC", "1.0584");
    let (status, body) = h.execute(quote["quote_id"].as_str().unwrap(), None).await;
    assert_eq!(status, 422, "{body}");
    assert_eq!(body["error"]["code"], "price_impact_exceeded");
    assert!(h.lps.values().all(|lp| lp.execution_count() == 0));
}

#[tokio::test]
async fn oracle_outage_fails_closed_for_slippage_protected_orders() {
    let h = harness(standard()).await;
    let _g = h.guard("oracle_outage_fails_closed_for_slippage_protected_orders");
    h.oracle.set_down(true);
    let (status, body) = h
        .quote("USDC", "XLM", "1", Some(json!({ "max_slippage_bps": 100 })))
        .await;
    assert_eq!(status, 503, "{body}");
    assert_eq!(body["error"]["code"], "reference_price_unavailable");
    // Unprotected quotes still work, just without price impact.
    let (status, quote) = h.quote("USDC", "XLM", "1", None).await;
    assert_eq!(status, 200);
    assert!(quote["price_impact_bps"].is_null());
}

// ---------------------------------------------------------------------------
// Lifecycle edges
// ---------------------------------------------------------------------------

#[tokio::test]
async fn expired_quote_is_rejected_and_never_executes() {
    let h = harness(standard()).await;
    let _g = h.guard("expired_quote_is_rejected_and_never_executes");
    let (_, quote) = h.quote("USDC", "XLM", "5", None).await;
    h.advance(30);
    let (status, body) = h.execute(quote["quote_id"].as_str().unwrap(), None).await;
    assert_eq!(status, 410, "{body}");
    assert_eq!(body["error"]["code"], "quote_expired");
    assert_eq!(h.lp("lp-alpha").execution_count(), 0);
}

#[tokio::test]
async fn re_executing_a_quote_returns_the_original_swap() {
    let h = harness(standard()).await;
    let _g = h.guard("re_executing_a_quote_returns_the_original_swap");
    let (_, quote) = h.quote("USDC", "EURC", "7", None).await;
    let id = quote["quote_id"].as_str().unwrap();
    let (_, first) = h.execute(id, None).await;
    let (_, second) = h.execute(id, None).await;
    assert_eq!(first, second);
    assert_eq!(h.lp("lp-alpha").execution_count(), 1);
    assert_eq!(h.lp("lp-beta").execution_count(), 1);
}

#[tokio::test]
async fn amounts_must_be_strings_with_supported_precision() {
    let h = harness(standard()).await;
    let _g = h.guard("amounts_must_be_strings_with_supported_precision");
    let (status, _) = h
        .post(
            "/swap/quotes",
            json!({ "source_asset": "USDC", "destination_asset": "XLM", "amount_in": 1.5 }),
        )
        .await;
    assert_eq!(status, 400);
    let (status, body) = h.quote("USDC", "XLM", "1.00000001", None).await;
    assert_eq!(status, 400);
    assert_eq!(body["error"]["code"], "invalid_amount");
}

// ---------------------------------------------------------------------------
// Seeded randomized lifecycles
// ---------------------------------------------------------------------------

#[derive(Debug, Default, Clone, PartialEq, Eq)]
struct RunStats {
    quoted: u32,
    quote_rejected: u32,
    settled: u32,
    compensated: u32,
    expired: u32,
    refused_at_execution: u32,
    replays: u32,
}

/// `n` random lifecycles over a noisy market: providers jitter fills by up
/// to 40 bps and fail 3% of the time; one provider ignores `min_out`; the
/// oracle occasionally goes down; some quotes expire; some executions are
/// replayed. Every terminal swap is checked against the invariants, and the
/// providers' inventories must match what settled.
async fn run_random_lifecycles(seed: u64, db: DbKind, n: u32) -> (String, RunStats) {
    let cfg = HarnessConfig::standard(seed)
        .with_db(db)
        .map_lp("lp-alpha", |mut c| {
            c.jitter_bps = 40;
            c.fail_bps = 300;
            c
        })
        .map_lp("lp-beta", |mut c| {
            c.jitter_bps = 40;
            c.fail_bps = 300;
            c
        })
        .map_lp("lp-gamma", |mut c| {
            c.jitter_bps = 25;
            c.fail_bps = 200;
            c
        })
        .map_lp("lp-delta", |mut c| {
            c.jitter_bps = 60;
            c.fail_bps = 300;
            c.honours_min_out = false;
            c
        });
    let mut h = harness(cfg).await;
    let _g = h.guard("seeded_random_lifecycles_hold_invariants");
    let assets = ["USDC", "XLM", "EURC", "NGN", "BTC"];
    let mut stats = RunStats::default();
    let mut settled = Vec::new();

    for _ in 0..n {
        let src = *h.rng.pick(&assets);
        let dst = loop {
            let d = *h.rng.pick(&assets);
            if d != src {
                break d;
            }
        };
        let decimals = if src == "BTC" { 8 } else { 7 };
        let units = h.rng.range(100_000, 10_000_000_000);
        let amount = Decimal::from_units(units, decimals).to_string();
        let mode = h.rng.range(0, 3);
        let quote_protection = match mode {
            1 | 3 => Some(json!({ "max_slippage_bps": h.rng.range(25, 400) })),
            _ => None,
        };

        let (status, quote) = h.quote(src, dst, &amount, quote_protection).await;
        if status != 200 {
            assert!(
                matches!(
                    quote["error"]["code"].as_str(),
                    Some("price_impact_exceeded" | "no_route")
                ),
                "unexpected quote failure {status}: {quote}"
            );
            stats.quote_rejected += 1;
            continue;
        }
        stats.quoted += 1;

        let order_protection = match mode {
            2 => {
                // Ask for at least the quote minus up to 60 bps.
                let expected = dec(&quote["expected_out"]);
                let bps = h.rng.range(0, 60) as u32;
                let min = expected.mul_bps(10_000 - bps, expected.scale(), RoundingMode::Ceiling);
                Some(json!({ "min_received": min.to_string() }))
            }
            3 => Some(json!({ "max_slippage_bps": h.rng.range(25, 400) })),
            _ => None,
        };

        let expire = h.rng.chance_bps(500);
        h.advance(if expire { 31 } else { h.rng.range(0, 5) as i64 });
        let oracle_down = h.rng.chance_bps(300);
        h.oracle.set_down(oracle_down);
        let (status, swap) = h
            .execute(quote["quote_id"].as_str().unwrap(), order_protection)
            .await;
        h.oracle.set_down(false);

        match status {
            200 => {}
            410 => {
                assert!(expire, "only expired quotes may 410: {swap}");
                stats.expired += 1;
                continue;
            }
            422 | 503 => {
                assert!(
                    matches!(
                        swap["error"]["code"].as_str(),
                        Some("price_impact_exceeded" | "min_received_unattainable" | "reference_price_unavailable")
                    ),
                    "unexpected execution refusal {status}: {swap}"
                );
                stats.refused_at_execution += 1;
                continue;
            }
            other => panic!("unexpected execute status {other}: {swap}"),
        }

        let events = h.events(swap["swap_id"].as_str().unwrap()).await;
        assert_swap_invariants(&quote, &swap, &events);
        match swap["status"].as_str().unwrap() {
            "settled" => {
                stats.settled += 1;
                settled.push(swap.clone());
            }
            "compensated" => stats.compensated += 1,
            other => panic!("unexpected terminal status {other}"),
        }

        if h.rng.chance_bps(1_000) {
            let (status, again) = h.execute(quote["quote_id"].as_str().unwrap(), None).await;
            assert_eq!(status, 200);
            assert_eq!(again, swap, "replay must return the original swap");
            stats.replays += 1;
        }
    }

    assert_inventory_conserved(&h, &settled);
    h.transcript.push(format!("stats {stats:?}"));
    let digest = h.digest();
    drop(_g);
    // Keep the rng borrow checker happy across awaits.
    let _ = &mut h;
    (digest, stats)
}

#[tokio::test]
async fn seeded_random_lifecycles_hold_invariants() {
    let seed = seed_from_env();
    let (digest, stats) = run_random_lifecycles(seed, DbKind::InMemory, 60).await;
    println!("seed {seed:#x}: {stats:?} transcript sha256 {digest}");
    assert_eq!(
        stats.quoted + stats.quote_rejected,
        60,
        "every iteration ends in a quote or a quote rejection"
    );
    if seed == DEFAULT_SEED {
        // The default seed is chosen to exercise every outcome.
        assert!(stats.settled > 0, "{stats:?}");
        assert!(stats.compensated > 0, "{stats:?}");
        assert!(stats.quote_rejected > 0, "{stats:?}");
        assert!(stats.expired + stats.refused_at_execution > 0, "{stats:?}");
    }
}

#[tokio::test]
async fn same_seed_reproduces_the_same_transcript() {
    let seed = seed_from_env();
    let (a, sa) = run_random_lifecycles(seed, DbKind::InMemory, 40).await;
    let (b, sb) = run_random_lifecycles(seed, DbKind::InMemory, 40).await;
    assert_eq!(sa, sb);
    assert_eq!(a, b, "same seed must produce a byte-identical transcript");
    let (c, _) = run_random_lifecycles(seed ^ 0xA5A5, DbKind::InMemory, 40).await;
    assert_ne!(a, c, "a different seed should explore a different run");
}

#[tokio::test]
#[ignore = "Requires Docker for testcontainers"]
async fn postgres_store_reproduces_the_in_memory_transcript() {
    let seed = seed_from_env();
    let (mem, mem_stats) = run_random_lifecycles(seed, DbKind::InMemory, 40).await;
    let (pg, pg_stats) = run_random_lifecycles(seed, DbKind::Postgres, 40).await;
    assert_eq!(mem_stats, pg_stats);
    assert_eq!(mem, pg, "Postgres-backed run diverged from the in-memory run");
}

#[tokio::test]
#[ignore = "Requires Docker for testcontainers"]
async fn postgres_store_full_lifecycle() {
    let h = harness(standard().with_db(DbKind::Postgres)).await;
    let _g = h.guard("postgres_store_full_lifecycle");
    h.lp("lp-beta").script(2, Fault::Fail);

    let (_, q1) = h.quote("USDC", "EURC", "100", None).await;
    let (_, s1) = h.execute(q1["quote_id"].as_str().unwrap(), None).await;
    assert_eq!(s1["status"], "settled", "{s1}");
    let (_, q2) = h.quote("USDC", "EURC", "100", None).await;
    let (_, s2) = h.execute(q2["quote_id"].as_str().unwrap(), None).await;
    assert_eq!(s2["status"], "compensated", "{s2}");

    // Round-trips through JSONB keep decimal strings exact.
    let (_, fetched) = h
        .get(&format!("/swap/executions/{}", s1["swap_id"].as_str().unwrap()))
        .await;
    assert_eq!(fetched, s1);
    let (_, again) = h.execute(q1["quote_id"].as_str().unwrap(), None).await;
    assert_eq!(again, s1);
    for (q, s) in [(&q1, &s1), (&q2, &s2)] {
        let events = h.events(s["swap_id"].as_str().unwrap()).await;
        assert_swap_invariants(q, s, &events);
    }
    assert_inventory_conserved(&h, &[s1]);
}
