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
pub mod models;
pub mod pagination;
pub mod resources;
pub mod retry;

pub use client::{AdminSynapseClient, SynapseClient};
pub use error::SynapseError;
pub use models::*;
pub use pagination::PageIter;
