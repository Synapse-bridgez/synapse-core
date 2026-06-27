use crate::client::SynapseClient;
use crate::error::SynapseError;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HealthStatus {
    pub status: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HealthErrors {
    pub errors: Vec<String>,
}

pub struct Health {
    client: SynapseClient,
}

impl Health {
    pub fn new(client: SynapseClient) -> Self {
        Self { client }
    }

    pub async fn live(&self) -> Result<HealthStatus, SynapseError> {
        self.client.get("/health/live").await
    }

    pub async fn ready(&self) -> Result<HealthStatus, SynapseError> {
        self.client.get("/health/ready").await
    }

    pub async fn health(&self) -> Result<HealthStatus, SynapseError> {
        self.client.get("/health").await
    }

    pub async fn errors(&self) -> Result<HealthErrors, SynapseError> {
        self.client.get("/health/errors").await
    }
}
