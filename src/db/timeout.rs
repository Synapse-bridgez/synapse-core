/// Per-endpoint-class query timeout enforcement using SET LOCAL.
///
/// This module provides helpers to enforce statement timeouts at the transaction level
/// using PostgreSQL's `SET LOCAL`, which auto-clears on commit/rollback and is therefore
/// safe under connection pooling.
///
/// # Why SET LOCAL is Safe Under Connection Pooling
///
/// - Session-scoped `SET` persists on the pooled connection
/// - Transaction-scoped `SET LOCAL` is auto-cleared on commit/rollback
/// - Even if the same physical connection serves endpoint A then endpoint B,
///   A's timeout setting is guaranteed cleared before B starts its transaction
/// - Failed transactions (rollback) also clear the timeout

use crate::config::EndpointClass;
use sqlx::{PgConnection, Postgres, Result, Transaction};

/// Set statement timeout via SET LOCAL for the current transaction.
///
/// The timeout applies only to this transaction and is automatically cleared
/// on commit/rollback, making it safe under connection pooling.
///
/// # Parameters
/// - `timeout_ms`: Timeout in milliseconds
///
/// # Returns
/// - `Ok(())` if the timeout was set successfully
/// - `Err(sqlx::Error)` if the query failed
pub async fn set_transaction_timeout(
    conn: &mut PgConnection,
    timeout_ms: u64,
) -> Result<()> {
    sqlx::query(&format!("SET LOCAL statement_timeout = {timeout_ms}"))
        .execute(&mut *conn)
        .await?;
    Ok(())
}

/// Set statement timeout via SET LOCAL for a transaction based on endpoint class.
///
/// This is a convenience wrapper that looks up the timeout value from the config
/// and applies it to the transaction.
///
/// # Parameters
/// - `tx`: The transaction to apply the timeout to
/// - `endpoint_class`: The endpoint class (Interactive, AdminReport, or BackgroundJob)
/// - `timeout_config`: The per-class timeout configuration
///
/// # Returns
/// - `Ok(())` if the timeout was set successfully
/// - `Err(sqlx::Error)` if the query failed
pub async fn set_transaction_timeout_for_class(
    tx: &mut Transaction<'_, Postgres>,
    endpoint_class: EndpointClass,
    timeout_config: &crate::config::PerClassTimeoutConfig,
) -> Result<()> {
    let timeout_ms = timeout_config.get_timeout_ms(endpoint_class);
    sqlx::query(&format!("SET LOCAL statement_timeout = {timeout_ms}"))
        .execute(&mut **tx)
        .await?;
    Ok(())
}

/// Verify that a timed-out query's connection is in a clean state for reuse.
///
/// After a statement timeout, the connection is left with an aborted transaction.
/// This function rolls back the transaction to clear that state. This is typically
/// called in error handling code when a timeout is detected.
///
/// # Parameters
/// - `tx`: The transaction that timed out
///
/// # Returns
/// - `Ok(())` if the rollback was successful
/// - `Err(sqlx::Error)` if the rollback failed (shouldn't happen)
pub async fn rollback_after_timeout(
    tx: Transaction<'_, Postgres>,
) -> Result<()> {
    tx.rollback().await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_endpoint_class_timeout_values() {
        let config = crate::config::PerClassTimeoutConfig::default();

        assert_eq!(
            config.get_timeout_ms(EndpointClass::Interactive),
            5000,
            "Interactive timeout should be 5000ms"
        );
        assert_eq!(
            config.get_timeout_ms(EndpointClass::AdminReport),
            30000,
            "AdminReport timeout should be 30000ms"
        );
        assert_eq!(
            config.get_timeout_ms(EndpointClass::BackgroundJob),
            60000,
            "BackgroundJob timeout should be 60000ms"
        );
    }

    #[test]
    fn test_custom_timeout_values() {
        let config = crate::config::PerClassTimeoutConfig {
            interactive_timeout_ms: 3000,
            admin_report_timeout_ms: 20000,
            background_job_timeout_ms: 120000,
        };

        assert_eq!(config.get_timeout_ms(EndpointClass::Interactive), 3000);
        assert_eq!(config.get_timeout_ms(EndpointClass::AdminReport), 20000);
        assert_eq!(config.get_timeout_ms(EndpointClass::BackgroundJob), 120000);
    }
}
