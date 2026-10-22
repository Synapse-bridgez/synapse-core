//! Persistence helpers for the swap engine.
//!
//! - [`retry`]: bounded, jittered retry of write transactions on transient
//!   PostgreSQL concurrency failures (serialization, deadlock, lock timeout).
//! - [`queries`]: the named swap lookup access patterns and the SQL that serves
//!   them, kept in step with the indexes in
//!   `migrations/20261021000000_swap_lookup_indexes.sql`.

pub mod queries;
pub mod retry;
