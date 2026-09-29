/// Governance and Compliance Modules
///
/// This module contains enforcement of architectural governance decisions
/// documented in ADRs, ensuring that sensitive operations route through
/// their designated authorities.

pub mod payment_matching_authority;

pub use payment_matching_authority::PaymentMatchingAuthority;
pub use payment_matching_authority::PaymentMatchingAuthorityError;
