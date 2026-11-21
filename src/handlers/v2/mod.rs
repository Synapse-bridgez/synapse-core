pub mod webhook {
    pub use crate::handlers::webhook::*;
}
pub mod settlements {
    pub use crate::handlers::settlements::*;
}
pub mod admin {
    pub use crate::handlers::admin::*;
}
pub mod dlq {
    pub use crate::handlers::dlq::*;
}
pub use crate::handlers::health;

/// API version served by this module, sourced from the shared version list in
/// [`crate::middleware::versioning`] so the middleware and handlers cannot drift.
pub const API_VERSION: &str = crate::middleware::versioning::V2;
