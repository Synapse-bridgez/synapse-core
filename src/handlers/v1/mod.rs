//! v1 API surface.
//!
//! Deprecation policy (see `docs/deprecation-policy.md`):
//! - Minimum notice period before removal: 6 months from the first release
//!   that emits a `Deprecation` header for the surface.
//! - Required warning channels: `Deprecation`/`Sunset` HTTP response headers
//!   for API surfaces, `#[deprecated]` attributes for SDK surfaces, and
//!   stderr warnings for CLI surfaces.
//! - Tracking: every deprecated surface must be listed in the deprecation
//!   registry so removal can be scheduled and audited.
//!
//! The v1 handlers below are the canonical example of a deprecated surface.
//! They are wired through [`crate::middleware::versioning`], which emits the
//! `Deprecation`/`Sunset` headers automatically for any request routed here.

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

/// Marks this module tree as a deprecated API surface.
///
/// The versioning middleware consults this constant to decide whether to
/// emit `Deprecation`/`Sunset` headers on responses produced by v1 handlers.
/// Future deprecations should follow the same pattern: expose a
/// `DEPRECATED` marker (or register the surface in the deprecation registry)
/// and let the middleware handle header emission.
pub const DEPRECATED: bool = true;

/// RFC 8594 `Sunset` value for the v1 surface.
///
/// Kept in sync with the minimum notice period defined in the deprecation
/// policy. Update this when the removal date is formally scheduled.
pub const SUNSET: &str = "Wed, 31 Dec 2025 23:59:59 GMT";
