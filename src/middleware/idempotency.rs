use redis::{Client, Commands, Connection};
use serde::{Deserialize, Serialize};
use std::time::Duration;

#[derive(Clone)]
pub struct IdempotencyService {
    client: Client,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct IdempotencyKey {
    pub key: String,
    pub ttl_seconds: u64,
}

impl IdempotencyService {
    pub fn new(redis_url: &str) -> Result<Self, redis::RedisError> {
        let client = Client::open(redis_url)?;
        Ok(Self { client })
    }

    pub async fn check_and_set(&self, key: &str, value: &str, ttl: Duration) -> Result<bool, redis::RedisError> {
        let mut conn = self.client.get_connection()?;
        let result: Option<String> = conn.set_nx_ex(key, value, ttl.as_secs())?;
        Ok(result.is_some())
    }

    pub async fn get(&self, key: &str) -> Result<Option<String>, redis::RedisError> {
        let mut conn = self.client.get_connection()?;
        conn.get(key)
    }
}