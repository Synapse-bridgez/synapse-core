//! Per-tenant data residency and regional routing.
//!
//! A tenant is pinned to one [`Region`]. Region resolution fails closed: an
//! unknown or unconfigured region is an error, never a default to a primary
//! region. Datastore selection is a function of the resolved [`RegionContext`].
//!
//! # Regional health and failover
//! A degraded region is marked via [`RegionRouter::set_health`]. Requests for
//! that region's tenants are rejected with [`ResidencyError::RegionDegraded`]
//! (never served from another region). In-flight swaps are not migrated: they
//! stay owned by their region and resume from persisted state (outbox/state
//! machine) when it recovers. See `docs/data-residency.md`.

use std::collections::HashMap;
use std::sync::RwLock;

/// Versioned region identifier, e.g. `eu-1`.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Region(String);

impl Region {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ResidencyError {
    #[error("tenant has no region configured")]
    MissingRegion,
    #[error("unknown region: {0}")]
    UnknownRegion(String),
    #[error("cross-region request rejected: tenant in {tenant}, served by {serving}")]
    CrossRegion { tenant: String, serving: String },
    #[error("region {0} is degraded")]
    RegionDegraded(String),
}

/// Resolved at request ingress and attached to the request context.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RegionContext {
    pub region: Region,
    pub database_url: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RegionHealth {
    Healthy,
    Degraded,
}

/// Region id -> datastore, for the regions configured in this deployment.
pub struct RegionRouter {
    /// Region this deployment serves.
    local: Region,
    datastores: HashMap<Region, String>,
    health: RwLock<HashMap<Region, RegionHealth>>,
}

impl RegionRouter {
    pub fn new(local: &str, datastores: HashMap<String, String>) -> Result<Self, ResidencyError> {
        let datastores: HashMap<Region, String> = datastores
            .into_iter()
            .map(|(k, v)| (Region(k), v))
            .collect();
        let local = Region(local.to_string());
        if !datastores.contains_key(&local) {
            return Err(ResidencyError::UnknownRegion(local.0));
        }
        Ok(Self {
            local,
            datastores,
            health: RwLock::new(HashMap::new()),
        })
    }

    /// `SWAP_LOCAL_REGION` and `SWAP_REGION_DATABASES` (`eu-1=url,us-1=url`).
    pub fn from_env() -> Result<Self, ResidencyError> {
        let local =
            std::env::var("SWAP_LOCAL_REGION").map_err(|_| ResidencyError::MissingRegion)?;
        let map = std::env::var("SWAP_REGION_DATABASES")
            .unwrap_or_default()
            .split(',')
            .filter_map(|p| p.split_once('='))
            .map(|(k, v)| (k.trim().to_string(), v.trim().to_string()))
            .collect();
        Self::new(&local, map)
    }

    pub fn set_health(&self, region: &str, health: RegionHealth) {
        self.health
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .insert(Region(region.to_string()), health);
    }

    /// Resolve a tenant's region. Fails closed on missing/unknown regions,
    /// regions served elsewhere, and degraded regions.
    pub fn resolve(&self, tenant_region: Option<&str>) -> Result<RegionContext, ResidencyError> {
        let name = tenant_region
            .filter(|r| !r.is_empty())
            .ok_or(ResidencyError::MissingRegion)?;
        let region = Region(name.to_string());
        let database_url = self
            .datastores
            .get(&region)
            .ok_or_else(|| ResidencyError::UnknownRegion(name.to_string()))?
            .clone();
        if region != self.local {
            return Err(ResidencyError::CrossRegion {
                tenant: name.to_string(),
                serving: self.local.0.clone(),
            });
        }
        let degraded = self
            .health
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .get(&region)
            == Some(&RegionHealth::Degraded);
        if degraded {
            return Err(ResidencyError::RegionDegraded(name.to_string()));
        }
        Ok(RegionContext {
            region,
            database_url,
        })
    }

    /// Regions a background job (partition maintenance, outbox relay,
    /// reconciliation) may touch: only the local one.
    pub fn job_regions(&self) -> Vec<&Region> {
        vec![&self.local]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn router() -> RegionRouter {
        RegionRouter::new(
            "eu-1",
            HashMap::from([
                ("eu-1".into(), "postgres://eu".into()),
                ("us-1".into(), "postgres://us".into()),
            ]),
        )
        .unwrap()
    }

    #[test]
    fn pinned_tenant_resolves_to_its_region_datastore() {
        assert_eq!(
            router().resolve(Some("eu-1")).unwrap().database_url,
            "postgres://eu"
        );
    }

    #[test]
    fn unknown_and_missing_regions_fail_closed() {
        let r = router();
        assert_eq!(r.resolve(None), Err(ResidencyError::MissingRegion));
        assert_eq!(r.resolve(Some("")), Err(ResidencyError::MissingRegion));
        assert!(matches!(
            r.resolve(Some("mars-1")),
            Err(ResidencyError::UnknownRegion(_))
        ));
    }

    #[test]
    fn cross_region_is_rejected_not_served() {
        assert!(matches!(
            router().resolve(Some("us-1")),
            Err(ResidencyError::CrossRegion { .. })
        ));
    }

    #[test]
    fn degraded_region_rejects_until_recovered() {
        let r = router();
        r.set_health("eu-1", RegionHealth::Degraded);
        assert!(matches!(
            r.resolve(Some("eu-1")),
            Err(ResidencyError::RegionDegraded(_))
        ));
        r.set_health("eu-1", RegionHealth::Healthy);
        assert!(r.resolve(Some("eu-1")).is_ok());
    }

    #[test]
    fn background_jobs_are_region_scoped() {
        assert_eq!(router().job_regions().len(), 1);
    }

    #[test]
    fn local_region_must_be_configured() {
        assert!(RegionRouter::new("eu-1", HashMap::new()).is_err());
    }
}
