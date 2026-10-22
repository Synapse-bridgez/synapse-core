//! Swap-engine capacity model (#1539). Tier definitions live in
//! `src/swap/capacity_tiers.json` so the same data drives docs and autoscaling.

use serde::Deserialize;

#[derive(Debug, Clone, Deserialize)]
pub struct PerInstance {
    pub orders_per_sec: f64,
    pub db_connections: u32,
    pub cost_ms_per_order: f64,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Postgres {
    pub max_connections: u32,
    pub reserved_connections: u32,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Tier {
    pub name: String,
    pub peak_orders_per_sec: f64,
    pub target_utilization: f64,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CapacityModel {
    pub per_instance: PerInstance,
    pub postgres: Postgres,
    pub min_replicas: u32,
    pub tiers: Vec<Tier>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Sizing {
    pub tier: String,
    /// Replicas wanted by throughput, floored at `min_replicas`.
    pub wanted_replicas: u32,
    /// Replicas allowed by the Postgres connection budget.
    pub max_replicas: u32,
    /// Effective replicas = min(wanted, max).
    pub replicas: u32,
    pub total_connections: u32,
    /// True when the DB budget, not throughput, limits scale-out.
    pub db_bound: bool,
}

impl CapacityModel {
    pub fn from_json(s: &str) -> Result<Self, serde_json::Error> {
        serde_json::from_str(s)
    }

    /// Maximum replicas such that `replicas * pool` stays within the budget.
    pub fn max_replicas(&self) -> u32 {
        let budget = self
            .postgres
            .max_connections
            .saturating_sub(self.postgres.reserved_connections);
        budget / self.per_instance.db_connections.max(1)
    }

    pub fn size(&self, tier: &Tier) -> Sizing {
        let per = self.per_instance.orders_per_sec * tier.target_utilization;
        let wanted = ((tier.peak_orders_per_sec / per).ceil() as u32).max(self.min_replicas);
        let max = self.max_replicas();
        let replicas = wanted.min(max);
        Sizing {
            tier: tier.name.clone(),
            wanted_replicas: wanted,
            max_replicas: max,
            replicas,
            total_connections: replicas * self.per_instance.db_connections,
            db_bound: wanted > max,
        }
    }

    pub fn size_all(&self) -> Vec<Sizing> {
        self.tiers.iter().map(|t| self.size(t)).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn model() -> CapacityModel {
        CapacityModel::from_json(include_str!("capacity_tiers.json")).unwrap()
    }

    #[test]
    fn pool_sizing_never_exceeds_postgres_budget() {
        let m = model();
        let budget = m.postgres.max_connections - m.postgres.reserved_connections;
        for s in m.size_all() {
            assert!(s.total_connections <= budget, "{s:?}");
        }
        assert!(m.max_replicas() * m.per_instance.db_connections <= budget);
    }

    #[test]
    fn tiers_respect_failover_floor_and_scale_monotonically() {
        let m = model();
        let sizes = m.size_all();
        assert!(sizes
            .iter()
            .all(|s| s.replicas >= m.min_replicas.min(s.max_replicas)));
        assert!(sizes
            .windows(2)
            .all(|w| w[0].wanted_replicas <= w[1].wanted_replicas));
    }

    #[test]
    fn db_bound_caps_scale_out() {
        let mut m = model();
        m.postgres.max_connections = 60; // budget 20 -> 2 replicas
        let s = m.size(&m.tiers[2].clone());
        assert!(s.db_bound);
        assert_eq!(s.replicas, 2);
    }
}
