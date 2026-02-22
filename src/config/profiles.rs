use serde::Deserialize;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Profile {
    Development,
    Staging,
    Production,
}

impl Profile {
    pub fn from_env() -> Self {
        std::env::var("APP_PROFILE")
            .ok()
            .and_then(|s| match s.to_lowercase().as_str() {
                "development" | "dev" => Some(Self::Development),
                "staging" | "stage" => Some(Self::Staging),
                "production" | "prod" => Some(Self::Production),
                _ => None,
            })
            .unwrap_or(Self::Development)
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Development => "development",
            Self::Staging => "staging",
            Self::Production => "production",
        }
    }
}

#[derive(Debug, Deserialize, Clone)]
pub struct ProfileDefaults {
    pub server_port: u16,
    pub database_url: Option<String>,
    pub stellar_horizon_url: String,
    pub redis_url: String,
    pub cors_allowed_origins: Option<String>,
}

impl ProfileDefaults {
    pub fn for_profile(profile: Profile) -> Self {
        match profile {
            Profile::Development => Self {
                server_port: 3000,
                database_url: None,
                stellar_horizon_url: "https://horizon-testnet.stellar.org".to_string(),
                redis_url: "redis://localhost:6379".to_string(),
                cors_allowed_origins: None,
            },
            Profile::Staging => Self {
                server_port: 8080,
                database_url: None,
                stellar_horizon_url: "https://horizon-testnet.stellar.org".to_string(),
                redis_url: "redis://redis:6379".to_string(),
                cors_allowed_origins: Some("https://staging.example.com".to_string()),
            },
            Profile::Production => Self {
                server_port: 8080,
                database_url: None,
                stellar_horizon_url: "https://horizon.stellar.org".to_string(),
                redis_url: "redis://redis:6379".to_string(),
                cors_allowed_origins: Some("https://app.example.com".to_string()),
            },
        }
    }
}
