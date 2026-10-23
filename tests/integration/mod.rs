//! End-to-end integration suite against Stellar testnet (quickstart or hosted).
//!
//! This module is intentionally opt-in: it is gated behind the `integration`
//! feature and the `INTEGRATION_TESTS` environment variable so that the normal
//! unit-test run never touches the network. See `tests/integration/README.md`
//! for how to run it locally and in CI.
//!
//! Layout:
//! - [`env`]: environment bring-up (quickstart container or hosted testnet),
//!   self-check, and configuration.
//! - [`helpers`]: friendbot funding with backoff, asset issuance, unique
//!   per-run accounts/assets, and bounded ledger polling.
//! - [`scenarios`]: scenario execution (successful settlement, on-chain
//!   rejection, retry after transient network error).
//!
//! Environment bring-up is deliberately separated from scenario execution so
//! the same scenarios can run against a local quickstart container or the
//! hosted testnet purely via configuration.

#![cfg(feature = "integration")]

pub mod env;
pub mod helpers;
pub mod scenarios;

use std::time::Duration;

/// Default timeout for a single Horizon HTTP request.
pub const DEFAULT_HTTP_TIMEOUT: Duration = Duration::from_secs(30);

/// Default timeout for waiting on a ledger close / transaction inclusion.
pub const DEFAULT_LEDGER_TIMEOUT: Duration = Duration::from_secs(60);

/// Default polling interval when waiting on ledger state.
pub const DEFAULT_POLL_INTERVAL: Duration = Duration::from_secs(2);

/// Default number of retries for transient network errors.
pub const DEFAULT_TRANSIENT_RETRIES: u32 = 5;

/// Returns `true` when the integration suite has been explicitly enabled.
///
/// The suite is opt-in so that a plain `cargo test` never hits the network.
/// CI sets `INTEGRATION_TESTS=1` for the dedicated integration job.
pub fn integration_enabled() -> bool {
    matches!(
        std::env::var("INTEGRATION_TESTS").as_deref(),
        Ok("1") | Ok("true") | Ok("TRUE")
    )
}

/// Guard used by every scenario entry point.
///
/// Unlike a silent skip, this fails loudly when the suite is expected to run
/// but the environment is not configured, so CI cannot pass by accident.
pub fn require_integration_enabled() {
    if !integration_enabled() {
        panic!(
            "integration suite invoked but INTEGRATION_TESTS is not set; \
             set INTEGRATION_TESTS=1 to run against testnet (see tests/integration/README.md)"
        );
    }
}