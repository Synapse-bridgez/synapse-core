//! Persistence for quotes, swap executions and their event logs.

use async_trait::async_trait;
use std::collections::BTreeMap;
use std::sync::Mutex;

use crate::swap::engine::{Quote, SwapEvent, SwapRecord};

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum StoreError {
    /// A swap already exists for this quote (idempotency guard).
    #[error("a swap already exists for quote {0}")]
    DuplicateSwapForQuote(String),
    #[error("not found: {0}")]
    NotFound(String),
    #[error("store backend error: {0}")]
    Backend(String),
}

#[async_trait]
pub trait SwapStore: Send + Sync {
    async fn insert_quote(&self, quote: &Quote) -> Result<(), StoreError>;
    async fn get_quote(&self, quote_id: &str) -> Result<Option<Quote>, StoreError>;
    /// Insert a new swap. Fails with [`StoreError::DuplicateSwapForQuote`]
    /// if one already exists for `swap.quote_id` — a quote executes at most
    /// once.
    async fn insert_swap(&self, swap: &SwapRecord) -> Result<(), StoreError>;
    async fn update_swap(&self, swap: &SwapRecord) -> Result<(), StoreError>;
    async fn get_swap(&self, swap_id: &str) -> Result<Option<SwapRecord>, StoreError>;
    async fn get_swap_by_quote(&self, quote_id: &str) -> Result<Option<SwapRecord>, StoreError>;
    /// Append an event; the store assigns `seq` (1-based, per swap).
    async fn append_event(&self, event: SwapEvent) -> Result<SwapEvent, StoreError>;
    async fn events(&self, swap_id: &str) -> Result<Vec<SwapEvent>, StoreError>;
}

#[derive(Default)]
struct Inner {
    quotes: BTreeMap<String, Quote>,
    swaps: BTreeMap<String, SwapRecord>,
    swap_by_quote: BTreeMap<String, String>,
    events: BTreeMap<String, Vec<SwapEvent>>,
}

/// In-process store. Deterministic iteration order.
#[derive(Default)]
pub struct InMemorySwapStore {
    inner: Mutex<Inner>,
}

impl InMemorySwapStore {
    pub fn new() -> Self {
        Self::default()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }
}

#[async_trait]
impl SwapStore for InMemorySwapStore {
    async fn insert_quote(&self, quote: &Quote) -> Result<(), StoreError> {
        self.lock()
            .quotes
            .insert(quote.quote_id.clone(), quote.clone());
        Ok(())
    }

    async fn get_quote(&self, quote_id: &str) -> Result<Option<Quote>, StoreError> {
        Ok(self.lock().quotes.get(quote_id).cloned())
    }

    async fn insert_swap(&self, swap: &SwapRecord) -> Result<(), StoreError> {
        let mut inner = self.lock();
        if inner.swap_by_quote.contains_key(&swap.quote_id) {
            return Err(StoreError::DuplicateSwapForQuote(swap.quote_id.clone()));
        }
        inner
            .swap_by_quote
            .insert(swap.quote_id.clone(), swap.swap_id.clone());
        inner.swaps.insert(swap.swap_id.clone(), swap.clone());
        Ok(())
    }

    async fn update_swap(&self, swap: &SwapRecord) -> Result<(), StoreError> {
        let mut inner = self.lock();
        match inner.swaps.get_mut(&swap.swap_id) {
            Some(existing) => {
                *existing = swap.clone();
                Ok(())
            }
            None => Err(StoreError::NotFound(swap.swap_id.clone())),
        }
    }

    async fn get_swap(&self, swap_id: &str) -> Result<Option<SwapRecord>, StoreError> {
        Ok(self.lock().swaps.get(swap_id).cloned())
    }

    async fn get_swap_by_quote(&self, quote_id: &str) -> Result<Option<SwapRecord>, StoreError> {
        let inner = self.lock();
        Ok(inner
            .swap_by_quote
            .get(quote_id)
            .and_then(|id| inner.swaps.get(id))
            .cloned())
    }

    async fn append_event(&self, mut event: SwapEvent) -> Result<SwapEvent, StoreError> {
        let mut inner = self.lock();
        let log = inner.events.entry(event.swap_id.clone()).or_default();
        event.seq = log.len() as u64 + 1;
        log.push(event.clone());
        Ok(event)
    }

    async fn events(&self, swap_id: &str) -> Result<Vec<SwapEvent>, StoreError> {
        Ok(self.lock().events.get(swap_id).cloned().unwrap_or_default())
    }
}
