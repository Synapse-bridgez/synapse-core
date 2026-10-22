//! Volume-tiered, versioned swap fee schedules and tenant pricing plans.
//!
//! Plan resolution precedence (highest first):
//! 1. the tenant's assignment whose `effective_from <= at` (latest wins);
//! 2. the platform default plan ([`default_plan`]) - unassigned tenants are
//!    never charged zero.
//!
//! Tier selection uses the tenant's trailing [`VOLUME_WINDOW_DAYS`]-day settled
//! notional. Boundaries are **inclusive lower bounds**: a volume exactly equal
//! to a tier's `min_volume` selects that tier.
//!
//! Fees use integer minor units (stroops) and round half up, so they are
//! deterministic and monotonic non-decreasing in notional within a plan.
//! Plans are never applied retroactively: callers persist the returned
//! [`FeeQuote::plan_id`]/[`FeeQuote::plan_version`] on the order.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// Rolling window (days) of settled notional used for tier selection.
pub const VOLUME_WINDOW_DAYS: i64 = 30;
const BPS_DENOMINATOR: u128 = 10_000;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Tier {
    /// Inclusive lower bound of trailing volume for this tier.
    pub min_volume: u128,
    pub fee_bps: u32,
    /// Flat component added to the proportional fee.
    pub flat_fee: u128,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PricingPlan {
    pub id: uuid::Uuid,
    pub name: String,
    pub version: i32,
    pub effective_from: DateTime<Utc>,
    pub effective_to: Option<DateTime<Utc>>,
    /// Sorted ascending by `min_volume`; first tier must start at 0.
    pub tiers: Vec<Tier>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PricingError {
    InvalidTiers(&'static str),
    NoActivePlan,
    /// The plan resolved at execution differs from the quoted one.
    PlanChanged {
        quoted: i32,
        current: i32,
    },
}

impl std::fmt::Display for PricingError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PricingError::InvalidTiers(m) => write!(f, "invalid pricing tiers: {m}"),
            PricingError::NoActivePlan => write!(f, "no active pricing plan"),
            PricingError::PlanChanged { quoted, current } => {
                write!(
                    f,
                    "pricing plan changed since quote (v{quoted} -> v{current})"
                )
            }
        }
    }
}

impl std::error::Error for PricingError {}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FeeQuote {
    pub fee: u128,
    pub plan_id: uuid::Uuid,
    pub plan_version: i32,
}

impl PricingPlan {
    pub fn validate(&self) -> Result<(), PricingError> {
        match self.tiers.first() {
            None => return Err(PricingError::InvalidTiers("no tiers")),
            Some(t) if t.min_volume != 0 => {
                return Err(PricingError::InvalidTiers("first tier must start at 0"))
            }
            _ => {}
        }
        if self
            .tiers
            .windows(2)
            .any(|w| w[0].min_volume >= w[1].min_volume)
        {
            return Err(PricingError::InvalidTiers(
                "tiers must be strictly ascending",
            ));
        }
        Ok(())
    }

    pub fn is_active(&self, at: DateTime<Utc>) -> bool {
        self.effective_from <= at && self.effective_to.map_or(true, |to| at < to)
    }

    /// Pure fee computation: tier by `trailing_volume`, fee on `notional`.
    pub fn compute_fee(
        &self,
        notional: u128,
        trailing_volume: u128,
    ) -> Result<FeeQuote, PricingError> {
        self.validate()?;
        let tier = self
            .tiers
            .iter()
            .rev()
            .find(|t| trailing_volume >= t.min_volume)
            .expect("validated: first tier starts at 0");
        let proportional =
            (notional * tier.fee_bps as u128 + BPS_DENOMINATOR / 2) / BPS_DENOMINATOR;
        Ok(FeeQuote {
            fee: proportional + tier.flat_fee,
            plan_id: self.id,
            plan_version: self.version,
        })
    }
}

/// Documented platform default for tenants with no assignment (never zero).
pub fn default_plan() -> PricingPlan {
    PricingPlan {
        id: uuid::Uuid::nil(),
        name: "default".into(),
        version: 1,
        effective_from: DateTime::<Utc>::MIN_UTC,
        effective_to: None,
        tiers: vec![Tier {
            min_volume: 0,
            fee_bps: 30,
            flat_fee: 0,
        }],
    }
}

/// Source of plans; a future pricing service can replace the DB lookup.
pub trait PlanResolver {
    /// Plan applying to `tenant_id` at instant `at`, or `None` if unassigned.
    fn plan_for(&self, tenant_id: uuid::Uuid, at: DateTime<Utc>) -> Option<PricingPlan>;
}

/// Resolve a plan, falling back to the default plan for unassigned tenants.
pub fn resolve_plan<R: PlanResolver>(
    r: &R,
    tenant_id: uuid::Uuid,
    at: DateTime<Utc>,
) -> PricingPlan {
    r.plan_for(tenant_id, at).unwrap_or_else(default_plan)
}

/// Re-resolve at execution time; fail if the plan version moved since quote.
pub fn verify_at_execution<R: PlanResolver>(
    r: &R,
    tenant_id: uuid::Uuid,
    quoted: &FeeQuote,
    now: DateTime<Utc>,
) -> Result<PricingPlan, PricingError> {
    let plan = resolve_plan(r, tenant_id, now);
    if plan.id != quoted.plan_id || plan.version != quoted.plan_version {
        return Err(PricingError::PlanChanged {
            quoted: quoted.plan_version,
            current: plan.version,
        });
    }
    Ok(plan)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;
    use proptest::prelude::*;

    fn plan(version: i32, from: i64, to: Option<i64>, bps: [u32; 2]) -> PricingPlan {
        PricingPlan {
            id: uuid::Uuid::from_u128(version as u128),
            name: "p".into(),
            version,
            effective_from: Utc.timestamp_opt(from, 0).unwrap(),
            effective_to: to.map(|t| Utc.timestamp_opt(t, 0).unwrap()),
            tiers: vec![
                Tier {
                    min_volume: 0,
                    fee_bps: bps[0],
                    flat_fee: 5,
                },
                Tier {
                    min_volume: 1_000,
                    fee_bps: bps[1],
                    flat_fee: 0,
                },
            ],
        }
    }

    #[test]
    fn tier_boundaries_are_inclusive_lower_bounds() {
        let p = plan(1, 0, None, [100, 50]);
        // (trailing volume, expected fee on notional 10_000)
        for (vol, fee) in [(0, 105), (999, 105), (1_000, 50), (5_000, 50)] {
            assert_eq!(p.compute_fee(10_000, vol).unwrap().fee, fee, "vol {vol}");
        }
    }

    #[test]
    fn rounds_half_up() {
        let p = plan(1, 0, None, [100, 100]);
        // 50 * 1% = 0.5 -> 1, + flat 5
        assert_eq!(p.compute_fee(50, 0).unwrap().fee, 6);
        assert_eq!(p.compute_fee(49, 0).unwrap().fee, 5);
    }

    #[test]
    fn invalid_tiers_rejected() {
        let mut p = plan(1, 0, None, [1, 1]);
        p.tiers[1].min_volume = 0;
        assert!(p.compute_fee(1, 0).is_err());
        p.tiers[0].min_volume = 1;
        assert!(p.validate().is_err());
        p.tiers.clear();
        assert!(p.validate().is_err());
    }

    #[test]
    fn effective_window_rollover() {
        let p = plan(1, 100, Some(200), [1, 1]);
        let at = |s| Utc.timestamp_opt(s, 0).unwrap();
        assert!(!p.is_active(at(99)));
        assert!(p.is_active(at(100)));
        assert!(p.is_active(at(199)));
        assert!(!p.is_active(at(200)));
    }

    struct Sched(Vec<PricingPlan>);
    impl PlanResolver for Sched {
        fn plan_for(&self, _t: uuid::Uuid, at: DateTime<Utc>) -> Option<PricingPlan> {
            self.0
                .iter()
                .filter(|p| p.is_active(at))
                .max_by_key(|p| p.version)
                .cloned()
        }
    }

    #[test]
    fn scheduled_change_takes_effect_at_instant_and_invalidates_quote() {
        let s = Sched(vec![
            plan(1, 0, Some(100), [100, 50]),
            plan(2, 100, None, [200, 100]),
        ]);
        let t = uuid::Uuid::new_v4();
        let at = |s| Utc.timestamp_opt(s, 0).unwrap();
        let q = resolve_plan(&s, t, at(99)).compute_fee(10_000, 0).unwrap();
        assert_eq!(q.plan_version, 1);
        assert!(verify_at_execution(&s, t, &q, at(99)).is_ok());
        assert_eq!(
            verify_at_execution(&s, t, &q, at(100)),
            Err(PricingError::PlanChanged {
                quoted: 1,
                current: 2
            })
        );
    }

    #[test]
    fn unassigned_tenant_gets_nonzero_default() {
        let s = Sched(vec![]);
        let p = resolve_plan(&s, uuid::Uuid::new_v4(), Utc::now());
        assert!(p.compute_fee(10_000, 0).unwrap().fee > 0);
    }

    proptest! {
        #[test]
        fn fee_monotonic_in_notional(a in 0u128..1_000_000_000, b in 0u128..1_000_000_000, vol in 0u128..5_000) {
            let p = plan(1, 0, None, [100, 50]);
            let (lo, hi) = if a <= b { (a, b) } else { (b, a) };
            prop_assert!(p.compute_fee(lo, vol).unwrap().fee <= p.compute_fee(hi, vol).unwrap().fee);
        }
    }
}
