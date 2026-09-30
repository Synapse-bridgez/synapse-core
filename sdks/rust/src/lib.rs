//! Rust client SDK for the Synapse API.
//!
//! Build a [`SynapseClient`] with [`SynapseClient::builder`] or the
//! [`SynapseClient::new`] convenience constructor, then access resources via
//! the accessor methods on the client (e.g. [`SynapseClient::transactions`]).
//!
//! # Deprecation policy
//!
//! This SDK follows the project-wide deprecation policy (see
//! `docs/deprecation-policy.md`). In short:
//!
//! * Deprecated items are marked with Rust's native `#[deprecated]` attribute
//!   and carry a message following the convention
//!   `"deprecated since <version>; <replacement>; removal no earlier than <version>"`.
//! * Deprecated items remain available for at least the documented minimum
//!   notice period before removal.
//! * The [`deprecation`] module provides helpers so new deprecations can adopt
//!   the same convention without reinventing the process.
//!
//! # License
//! This crate is distributed under the terms of the MIT license.

pub mod client;
pub mod deprecation;
pub mod error;
pub mod graphql_builder;
pub mod models;
pub mod pagination;
pub mod resources;
pub mod retry;
#[cfg(feature = "testing-support")]
pub mod testing;

pub use client::{AdminSynapseClient, SynapseClient};
pub use deprecation::{deprecation_message, DeprecationInfo, DEPRECATION_MESSAGE_CONVENTION};
pub use error::{ErrorCode, SynapseError};
pub use graphql_builder::{
    SettlementField, SettlementQueryBuilder, TransactionField, TransactionQueryBuilder,
    TransactionQueryFilter,
};
pub use models::*;
pub use pagination::{auto_follow, PageIter};
