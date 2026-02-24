use std::collections::HashMap;
use std::env;

use anyhow::{Context, Result};
use vaultrs::auth::approle;
use vaultrs::client::{Client, VaultClient, VaultClientSettingsBuilder};
use vaultrs::kv2;

pub struct SecretsManager {
    client: VaultClient,
    kv_mount: String,
}

impl SecretsManager {
    pub async fn new() -> Result<Self> {
        let vault_addr =
            env::var("VAULT_ADDR").unwrap_or_else(|_| "http://127.0.0.1:8200".to_string());
        let role_id = env::var("VAULT_ROLE_ID").context("VAULT_ROLE_ID is required")?;
        let secret_id = env::var("VAULT_SECRET_ID").context("VAULT_SECRET_ID is required")?;
        let auth_mount = env::var("VAULT_AUTH_MOUNT").unwrap_or_else(|_| "auth/approle".to_string());
        let kv_mount = env::var("VAULT_KV_MOUNT").unwrap_or_else(|_| "secret".to_string());

        let mut client = VaultClient::new(
            VaultClientSettingsBuilder::default()
                .address(&vault_addr)
                .build()
                .context("failed to build Vault client settings")?,
        )
        .context("failed to create Vault client")?;

        let auth = approle::login(&mut client, &auth_mount, &role_id, &secret_id)
            .await
            .context("failed to authenticate to Vault with AppRole")?;
        client.set_token(&auth.client_token);

        Ok(Self { client, kv_mount })
    }

    pub async fn get_db_password(&self) -> Result<String> {
        let secret: HashMap<String, String> = kv2::read(&self.client, &self.kv_mount, "database")
            .await
            .context("failed to read secret/database from Vault")?;

        secret
            .get("password")
            .cloned()
            .context("password key not found in Vault secret/database")
    }

    pub async fn get_anchor_secret(&self) -> Result<String> {
        let secret: HashMap<String, String> = kv2::read(&self.client, &self.kv_mount, "anchor")
            .await
            .context("failed to read secret/anchor from Vault")?;

        secret
            .get("secret")
            .cloned()
            .context("secret key not found in Vault secret/anchor")
    }
}
