pub mod dlq;
pub mod locks;
pub mod reconciliation;
pub mod settlements;

pub use dlq::AdminDlq;
pub use reconciliation::AdminReconciliation;
pub use settlements::AdminSettlements;

use crate::client::SynapseClient;

/// Entry point for admin-scoped resources.
///
/// Obtain via [`SynapseClient::admin`]. All methods on sub-resources require
/// an admin key configured via [`crate::client::SynapseClientBuilder::admin_key`].
pub struct Admin<'a> {
    pub(crate) client: &'a SynapseClient,
}

impl<'a> Admin<'a> {
    /// Access admin lock management endpoints.
    pub fn locks(&self) -> locks::Locks<'_> {
        locks::Locks { client: self.client }
    }

    /// Access admin settlement management endpoints.
    pub fn settlements(&self) -> settlements::Settlements<'_> {
        settlements::Settlements { client: self.client }
    }
}
