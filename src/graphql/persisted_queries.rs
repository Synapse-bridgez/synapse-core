use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use tracing::{error, info, warn};

/// Configuration for persisted query enforcement
#[derive(Clone, Debug)]
pub struct PersistedQueryConfig {
    /// Enable persisted query enforcement
    pub enabled: bool,
    /// Allow non-persisted queries in development mode
    pub allow_unpersisted_in_dev: bool,
    /// Environment (dev, staging, prod)
    pub environment: String,
}

impl Default for PersistedQueryConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            allow_unpersisted_in_dev: true,
            environment: "development".to_string(),
        }
    }
}

/// Request that may contain either a persisted query hash or full query text
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GraphQLRequest {
    /// Optional persisted query hash (SHA256)
    pub persisted_query_hash: Option<String>,
    /// Optional full query text (for non-persisted queries)
    pub query: Option<String>,
    /// Optional operation name
    pub operation_name: Option<String>,
    /// Optional variables
    pub variables: Option<serde_json::Value>,
}

impl GraphQLRequest {
    /// Check if this request uses a persisted query
    pub fn is_persisted(&self) -> bool {
        self.persisted_query_hash.is_some()
    }
}

/// Registry of allowed persisted queries, mapping hash to full query text
pub struct PersistedQueryRegistry {
    config: PersistedQueryConfig,
    // SHA256 hash -> full query text
    queries: Arc<HashMap<String, String>>,
}

impl PersistedQueryRegistry {
    /// Create a new persisted query registry
    pub fn new(config: PersistedQueryConfig, queries: HashMap<String, String>) -> Self {
        info!(
            query_count = queries.len(),
            enabled = config.enabled,
            environment = &config.environment,
            "Initializing persisted query registry"
        );
        Self {
            config,
            queries: Arc::new(queries),
        }
    }

    /// Create an empty registry (for development)
    pub fn empty(config: PersistedQueryConfig) -> Self {
        Self {
            config,
            queries: Arc::new(HashMap::new()),
        }
    }

    /// Create a registry from a built-in set of queries
    pub fn from_builtin(config: PersistedQueryConfig) -> Self {
        // In a real implementation, this would load from a generated file
        // containing SDK queries. For now, we provide an empty set.
        let queries = HashMap::new();
        Self::new(config, queries)
    }

    /// Register a new persisted query
    pub fn register(&mut self, hash: String, query: String) {
        // Note: In production, this should not be called at runtime.
        // This is mainly for testing.
        info!(hash = &hash, "Registering persisted query");
        Arc::get_mut(&mut self.queries)
            .expect("Cannot register query while registry is in use")
            .insert(hash, query);
    }

    /// Resolve a persisted query hash to its full query text
    pub fn resolve_persisted_query(&self, hash: &str) -> Option<String> {
        self.queries.get(hash).cloned()
    }

    /// Validate and resolve a GraphQL request
    ///
    /// Returns:
    /// - Ok(query_text) if the request is valid
    /// - Err(reason) if the request violates policy
    pub fn validate_and_resolve(&self, req: &GraphQLRequest) -> Result<String, String> {
        match (&req.persisted_query_hash, &req.query) {
            // Request uses persisted query hash
            (Some(hash), _) => {
                match self.resolve_persisted_query(hash) {
                    Some(query) => {
                        info!(hash = hash, "Resolved persisted query");
                        Ok(query)
                    }
                    None => {
                        error!(hash = hash, "Unrecognized persisted query hash");
                        Err(format!(
                            "Persisted query not found: {}. This hash is not registered on this server.",
                            hash
                        ))
                    }
                }
            }

            // Request provides full query text
            (None, Some(query)) => {
                if !self.config.enabled {
                    // Persisted queries not enforced, allow any query
                    info!("Allowing non-persisted query (enforcement disabled)");
                    return Ok(query.clone());
                }

                // Persisted query enforcement is enabled
                if self.config.allow_unpersisted_in_dev && self.config.environment == "development" {
                    warn!("Allowing non-persisted query in development mode");
                    return Ok(query.clone());
                }

                // Persisted query enforcement is enabled and strict
                error!("Non-persisted query rejected (enforcement enabled)");
                Err(
                    "Persisted query enforcement is enabled. Submit queries as a persisted query hash instead.".to_string()
                )
            }

            // Request has neither hash nor query text
            (None, None) => {
                error!("GraphQL request missing both persisted_query_hash and query");
                Err("Request must include either 'persisted_query_hash' or 'query'".to_string())
            }
        }
    }
}

/// Compute SHA256 hash of a query string (for client-side query registration)
pub fn compute_query_hash(query: &str) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(query.as_bytes());
    format!("{:x}", hasher.finalize())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_query_hash_computation() {
        let query = "query { transactions { id } }";
        let hash = compute_query_hash(query);
        // Hash should be consistent
        let hash2 = compute_query_hash(query);
        assert_eq!(hash, hash2);
        // Hash should be a valid hex string
        assert!(hash.chars().all(|c| c.is_ascii_hexdigit()));
        assert_eq!(hash.len(), 64); // SHA256 produces 256 bits = 64 hex chars
    }

    #[test]
    fn test_persisted_query_resolution() {
        let mut config = PersistedQueryConfig::default();
        config.enabled = true;

        let query = "query { transactions { id } }";
        let hash = compute_query_hash(query);
        let mut queries = HashMap::new();
        queries.insert(hash.clone(), query.to_string());

        let registry = PersistedQueryRegistry::new(config, queries);

        let req = GraphQLRequest {
            persisted_query_hash: Some(hash.clone()),
            query: None,
            operation_name: None,
            variables: None,
        };

        assert!(registry.validate_and_resolve(&req).is_ok());
    }

    #[test]
    fn test_unregistered_persisted_query_rejected() {
        let mut config = PersistedQueryConfig::default();
        config.enabled = true;

        let registry = PersistedQueryRegistry::new(config, HashMap::new());

        let req = GraphQLRequest {
            persisted_query_hash: Some("invalid_hash".to_string()),
            query: None,
            operation_name: None,
            variables: None,
        };

        let result = registry.validate_and_resolve(&req);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("not found"));
    }

    #[test]
    fn test_unpersisted_query_in_production_rejected() {
        let mut config = PersistedQueryConfig::default();
        config.enabled = true;
        config.environment = "production".to_string();
        config.allow_unpersisted_in_dev = false;

        let registry = PersistedQueryRegistry::new(config, HashMap::new());

        let req = GraphQLRequest {
            persisted_query_hash: None,
            query: Some("query { transactions { id } }".to_string()),
            operation_name: None,
            variables: None,
        };

        let result = registry.validate_and_resolve(&req);
        assert!(result.is_err());
    }

    #[test]
    fn test_unpersisted_query_in_development_allowed() {
        let mut config = PersistedQueryConfig::default();
        config.enabled = true;
        config.environment = "development".to_string();
        config.allow_unpersisted_in_dev = true;

        let registry = PersistedQueryRegistry::new(config, HashMap::new());

        let query = "query { transactions { id } }";
        let req = GraphQLRequest {
            persisted_query_hash: None,
            query: Some(query.to_string()),
            operation_name: None,
            variables: None,
        };

        let result = registry.validate_and_resolve(&req);
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), query);
    }

    #[test]
    fn test_persisted_query_enforcement_disabled() {
        let mut config = PersistedQueryConfig::default();
        config.enabled = false;

        let registry = PersistedQueryRegistry::new(config, HashMap::new());

        let query = "query { transactions { id } }";
        let req = GraphQLRequest {
            persisted_query_hash: None,
            query: Some(query.to_string()),
            operation_name: None,
            variables: None,
        };

        let result = registry.validate_and_resolve(&req);
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), query);
    }

    #[test]
    fn test_missing_query_and_hash_rejected() {
        let config = PersistedQueryConfig::default();
        let registry = PersistedQueryRegistry::new(config, HashMap::new());

        let req = GraphQLRequest {
            persisted_query_hash: None,
            query: None,
            operation_name: None,
            variables: None,
        };

        let result = registry.validate_and_resolve(&req);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("must include"));
    }
}
