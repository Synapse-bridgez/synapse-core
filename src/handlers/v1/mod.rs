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

/// API version served by this handler module.
///
/// Kept in sync with the single source of truth in
/// [`crate::middleware::versioning::SUPPORTED_VERSIONS`]; the versioning
/// middleware routes requests to this module based on that constant.
pub const API_VERSION: &str = crate::middleware::versioning::V1;
