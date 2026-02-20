use failsafe::{backoff, failure_policy, Config, Error as FailsafeError, StateMachine};
use failsafe::futures::CircuitBreaker;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use std::time::Duration;
use thiserror::Error;

#[derive(Error, Debug)]
pub enum HorizonError {
    #[error("HTTP request failed: {0}")]
    RequestError(#[from] reqwest::Error),
    #[error("Account not found: {0}")]
    AccountNotFound(String),
    #[error("Invalid response from Horizon: {0}")]
    InvalidResponse(String),
    #[error("Circuit breaker open - Horizon API unavailable")]
    CircuitBreakerOpen,
}

/// Response from Horizon /accounts endpoint
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AccountResponse {
    pub id: String,
    pub account_id: String,
    pub balances: Vec<Balance>,
    pub sequence: String,
    pub subentry_count: i32,
    pub home_domain: Option<String>,
    pub last_modified_ledger: i64,
    pub last_modified_time: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Balance {
    pub balance: String,
    pub limit: Option<String>,
    pub asset_type: String,
    pub asset_code: Option<String>,
    pub asset_issuer: Option<String>,
}

/// HTTP client for interacting with the Stellar Horizon API
pub struct HorizonClient {
    client: Client,
    base_url: String,
    circuit_breaker: StateMachine<
        failure_policy::ConsecutiveFailures<backoff::Exponential>,
        (),
    >,
}

impl HorizonClient {
    /// Creates a new HorizonClient with the specified base URL
    pub fn new(base_url: String) -> Self {
        Self::with_circuit_breaker_config(base_url, 5, Duration::from_secs(60))
    }

    /// Creates a new HorizonClient with custom circuit breaker configuration
    pub fn with_circuit_breaker_config(
        base_url: String,
        failure_threshold: u32,
        reset_timeout: Duration,
    ) -> Self {
        let client = Client::builder()
            .timeout(Duration::from_secs(30))
            .build()
            .unwrap_or_default();

        let backoff = backoff::exponential(Duration::from_secs(10), reset_timeout);
        let policy = failure_policy::consecutive_failures(failure_threshold, backoff);
        let circuit_breaker = Config::new().failure_policy(policy).build();

        HorizonClient {
            client,
            base_url,
            circuit_breaker,
        }
    }

    /// Fetches account details from the Horizon API
    pub async fn get_account(&self, address: &str) -> Result<AccountResponse, HorizonError> {
        let url = format!("{}/accounts/{}", self.base_url.trim_end_matches('/'), address);
        let client = self.client.clone();
        let addr = address.to_string();

        let result = self
            .circuit_breaker
            .call(async move {
                let response = client.get(&url).send().await?;

                if response.status() == 404 {
                    return Err(HorizonError::AccountNotFound(addr));
                }

                let account = response.json::<AccountResponse>().await?;
                Ok(account)
            })
            .await;

        match result {
            Ok(account) => Ok(account),
            Err(FailsafeError::Rejected) => Err(HorizonError::CircuitBreakerOpen),
            Err(FailsafeError::Inner(e)) => Err(e),
        }
    }
}

impl Clone for HorizonClient {
    fn clone(&self) -> Self {
        Self {
            client: self.client.clone(),
            base_url: self.base_url.clone(),
            circuit_breaker: self.circuit_breaker.clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_horizon_client_creation() {
        let client = HorizonClient::new("https://horizon-testnet.stellar.org".to_string());
        assert_eq!(client.base_url, "https://horizon-testnet.stellar.org");
    }

    #[tokio::test]
    async fn test_get_account_with_mock() {
        use mockito::Mock;

        let mut server = mockito::Server::new();

        let mock_response = r#"{
            "id": "GBBD47UZQ5CSKQPV456PYYH4FSYJHBWGQJUVNMCNWZ2NBEHKQPW3KXKJ",
            "account_id": "GBBD47UZQ5CSKQPV456PYYH4FSYJHBWGQJUVNMCNWZ2NBEHKQPW3KXKJ",
            "balances": [
                {
                    "balance": "100.0000000",
                    "asset_type": "native",
                    "balance": "100.0000000",
                    "limit": null,
                    "asset_code": null,
                    "asset_issuer": null
                }
            ],
            "sequence": "1",
            "subentry_count": 0,
            "home_domain": null,
            "last_modified_ledger": 1,
            "last_modified_time": "2021-01-01T00:00:00Z"
        }"#;

        let _mock = server
            .mock("GET", mockito::Matcher::Regex(r".*/accounts/.*".into()))
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(mock_response)
            .create();

        let client = HorizonClient::new(server.url());
        let account = client
            .get_account("GBBD47UZQ5CSKQPV456PYYH4FSYJHBWGQJUVNMCNWZ2NBEHKQPW3KXKJ")
            .await;

        assert!(account.is_ok());
        let acc = account.unwrap();
        assert_eq!(
            acc.account_id,
            "GBBD47UZQ5CSKQPV456PYYH4FSYJHBWGQJUVNMCNWZ2NBEHKQPW3KXKJ"
        );
    }

    #[tokio::test]
    async fn test_get_account_not_found() {
        let mut server = mockito::Server::new();

        let _mock = server
            .mock("GET", mockito::Matcher::Regex(r".*/accounts/.*".into()))
            .with_status(404)
            .create();

        let client = HorizonClient::new(server.url());
        let result = client
            .get_account("GBBD47UZQ5CSKQPV456PYYH4FSYJHBWGQJUVNMCNWZ2NBEHKQPW3KXKJ")
            .await;

        assert!(matches!(result, Err(HorizonError::AccountNotFound(_))));
    }
}
