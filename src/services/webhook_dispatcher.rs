use sqlx::PgPool;
use uuid::Uuid;
use serde_json::Value;
use chrono::{DateTime, Utc, Duration};

#[derive(sqlx::FromRow, Debug, Clone)]
pub struct WebhookEndpoint {
    pub id: Uuid,
    pub url: String,
    pub secret: Option<String>,
    pub circuit_state: String,
    pub circuit_failure_count: i32,
    pub circuit_opened_at: Option<DateTime<Utc>>,
}

#[derive(Debug)]
pub enum CircuitState {
    Closed,
    Open,
    HalfOpen,
}

pub struct WebhookDispatcher {
    pool: PgPool,
}

impl WebhookDispatcher {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    pub async fn send_webhook(&self, endpoint_id: Uuid, payload: &Value) -> Result<(), anyhow::Error> {
        let endpoint: WebhookEndpoint = sqlx::query_as(
            "SELECT id, url, secret, circuit_state, circuit_failure_count, circuit_opened_at FROM webhook_endpoints WHERE id = $1"
        )
        .bind(endpoint_id)
        .fetch_one(&self.pool)
        .await?;

        let now = Utc::now();
        let should_attempt = match endpoint.circuit_state.as_str() {
            "closed" => true,
            "open" => {
                if let Some(opened_at) = endpoint.circuit_opened_at {
                    if now.signed_duration_since(opened_at) > Duration::minutes(10) {
                        // Transition to half-open
                        sqlx::query("UPDATE webhook_endpoints SET circuit_state = 'half_open' WHERE id = $1")
                            .bind(endpoint_id)
                            .execute(&self.pool)
                            .await?;
                        true
                    } else {
                        false
                    }
                } else {
                    true // invalid state, assume closed
                }
            }
            "half_open" => true,
            _ => true,
        };

        if !should_attempt {
            return Ok(()); // skip delivery
        }

        // Attempt delivery
        let client = reqwest::Client::new();
        let mut request = client.post(&endpoint.url).json(payload);

        if let Some(secret) = &endpoint.secret {
            request = request.header("X-Webhook-Secret", secret);
        }

        let response = request.send().await?;
        let success = response.status().is_success();

        if success {
            // Reset circuit
            sqlx::query("UPDATE webhook_endpoints SET circuit_state = 'closed', circuit_failure_count = 0, circuit_opened_at = NULL WHERE id = $1")
                .bind(endpoint_id)
                .execute(&self.pool)
                .await?;
        } else {
            // Record failure
            let new_count = endpoint.circuit_failure_count + 1;
            if new_count >= 5 {
                sqlx::query("UPDATE webhook_endpoints SET circuit_state = 'open', circuit_failure_count = $1, circuit_opened_at = $2 WHERE id = $3")
                    .bind(new_count)
                    .bind(now)
                    .bind(endpoint_id)
                    .execute(&self.pool)
                    .await?;
            } else {
                sqlx::query("UPDATE webhook_endpoints SET circuit_failure_count = $1 WHERE id = $2")
                    .bind(new_count)
                    .bind(endpoint_id)
                    .execute(&self.pool)
                    .await?;
            }
        }

        Ok(())
    }

    pub async fn reset_circuit(&self, endpoint_id: Uuid) -> Result<(), anyhow::Error> {
        sqlx::query("UPDATE webhook_endpoints SET circuit_state = 'closed', circuit_failure_count = 0, circuit_opened_at = NULL WHERE id = $1")
            .bind(endpoint_id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    // Placeholder for process_pending - would be called periodically to send pending webhooks
    pub async fn process_pending(&self) -> Result<(), anyhow::Error> {
        // TODO: Implement logic to fetch pending webhooks and send them
        // For each pending webhook, call send_webhook(endpoint_id, payload)
        Ok(())
    }
}