//! Rust client SDK for the Synapse API.
//!
//! Build a [`SynapseClient`] with [`SynapseClient::builder`] or the
//! [`SynapseClient::new`] convenience constructor, then access resources via
//! the accessor methods on the client (e.g. [`SynapseClient::transactions`]).
//!
//! # License
//! This crate is distributed under the terms of the MIT license.

pub mod client;
pub mod error;
pub mod graphql_builder;
pub mod models;
pub mod pagination;
pub mod resources;
pub mod retry;
#[cfg(feature = "testing-support")]
pub mod testing;

pub use client::{AdminSynapseClient, SynapseClient};
pub use error::{ErrorCode, SynapseError};
pub use graphql_builder::{
    SettlementField, SettlementQueryBuilder, TransactionField, TransactionQueryBuilder,
    TransactionQueryFilter,
};
pub use models::*;
pub use pagination::{auto_follow, PageIter};
