use std::sync::Arc;
use sqlx::PgPool;

use crate::config::assets::AssetCache;

#[derive(Clone)]
pub struct TransactionProcessor {
    pool: PgPool,
    assets: Arc<AssetCache>,
}

impl TransactionProcessor {
    pub fn new(pool: PgPool, assets: Arc<AssetCache>) -> Self {
        Self { pool, assets }
    }

    // Placeholder: real processing will use `self.assets.get(code)` when handling transactions
    pub async fn process_stub(&self) {
        // stub for future processing
        let _ = &self.pool; // keep unused warning away
    }
}
