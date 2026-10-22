//! Schema-registry-backed validation for swap lifecycle events.
//!
//! The registry is the source of truth: before an event is inserted into the
//! outbox it is validated against the registered schema for its subject and
//! stamped with the schema version. Dev/test environments without a registry
//! use [`InMemoryRegistry`].
//!
//! Compatibility mode is **BACKWARD** (new schema can read data written with
//! the previous one): removing/retyping a property or adding a new required
//! property is rejected by [`check_backward_compatible`], which CI exercises
//! via the `schemas_are_backward_compatible_with_baseline` test. When a schema
//! change is intentional and compatible, copy it into `schemas/baseline/`.

use async_trait::async_trait;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::Mutex;

pub const ORDER_CREATED: &str = include_str!("schemas/order_created.json");
pub const ORDER_SETTLED: &str = include_str!("schemas/order_settled.json");

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SchemaError {
    Incompatible(Vec<String>),
    InvalidEvent(String),
    UnknownSubject(String),
    RegistryUnavailable(String),
}

impl std::fmt::Display for SchemaError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SchemaError::Incompatible(v) => {
                write!(f, "incompatible schema change: {}", v.join("; "))
            }
            SchemaError::InvalidEvent(m) => write!(f, "event does not match schema: {m}"),
            SchemaError::UnknownSubject(s) => write!(f, "no schema registered for subject {s}"),
            SchemaError::RegistryUnavailable(m) => write!(f, "schema registry unavailable: {m}"),
        }
    }
}

impl std::error::Error for SchemaError {}

/// Classify a BACKWARD-compatibility change; returns the list of violations.
pub fn check_backward_compatible(old: &Value, new: &Value) -> Result<(), SchemaError> {
    let props = |v: &Value| {
        v.get("properties")
            .and_then(Value::as_object)
            .cloned()
            .unwrap_or_default()
    };
    let required = |v: &Value| -> Vec<String> {
        v.get("required")
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .filter_map(|s| s.as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_default()
    };
    let (op, np) = (props(old), props(new));
    let (oreq, nreq) = (required(old), required(new));
    let mut problems = Vec::new();
    for (name, oschema) in &op {
        match np.get(name) {
            None => problems.push(format!("property `{name}` removed")),
            Some(ns) if ns.get("type") != oschema.get("type") => {
                problems.push(format!("property `{name}` changed type"))
            }
            _ => {}
        }
    }
    for name in &nreq {
        if !oreq.contains(name) {
            problems.push(format!("property `{name}` newly required"));
        }
    }
    if problems.is_empty() {
        Ok(())
    } else {
        Err(SchemaError::Incompatible(problems))
    }
}

/// A registered schema and its registry version.
#[derive(Debug, Clone, PartialEq)]
pub struct Registered {
    pub version: u32,
    pub schema: Value,
}

#[async_trait]
pub trait SchemaRegistry: Send + Sync {
    /// Register `schema` for `subject`, enforcing compatibility; idempotent
    /// for an identical schema. Returns the version.
    async fn register(&self, subject: &str, schema: Value) -> Result<u32, SchemaError>;
    async fn latest(&self, subject: &str) -> Result<Registered, SchemaError>;
}

/// In-process registry for dev/test environments without a registry.
#[derive(Default)]
pub struct InMemoryRegistry(Mutex<HashMap<String, Vec<Value>>>);

#[async_trait]
impl SchemaRegistry for InMemoryRegistry {
    async fn register(&self, subject: &str, schema: Value) -> Result<u32, SchemaError> {
        let mut m = self.0.lock().unwrap();
        let versions = m.entry(subject.to_string()).or_default();
        if let Some(last) = versions.last() {
            if *last == schema {
                return Ok(versions.len() as u32);
            }
            check_backward_compatible(last, &schema)?;
        }
        versions.push(schema);
        Ok(versions.len() as u32)
    }

    async fn latest(&self, subject: &str) -> Result<Registered, SchemaError> {
        let m = self.0.lock().unwrap();
        m.get(subject)
            .and_then(|v| {
                v.last().map(|s| Registered {
                    version: v.len() as u32,
                    schema: s.clone(),
                })
            })
            .ok_or_else(|| SchemaError::UnknownSubject(subject.into()))
    }
}

/// Confluent-compatible HTTP registry client (`SCHEMA_REGISTRY_URL`).
pub struct HttpRegistry {
    base: String,
    client: reqwest::Client,
}

impl HttpRegistry {
    pub fn new(base: impl Into<String>) -> Self {
        Self {
            base: base.into().trim_end_matches('/').to_string(),
            client: reqwest::Client::new(),
        }
    }

    fn unavailable(e: impl std::fmt::Display) -> SchemaError {
        SchemaError::RegistryUnavailable(e.to_string())
    }
}

#[async_trait]
impl SchemaRegistry for HttpRegistry {
    async fn register(&self, subject: &str, schema: Value) -> Result<u32, SchemaError> {
        let ct = "application/vnd.schemaregistry.v1+json";
        let body = json!({ "schemaType": "JSON", "schema": schema.to_string() });
        let compat = self
            .client
            .post(format!(
                "{}/compatibility/subjects/{subject}/versions/latest",
                self.base
            ))
            .header("Content-Type", ct)
            .json(&body)
            .send()
            .await
            .map_err(Self::unavailable)?;
        // 404: first registration for the subject, nothing to be compatible with.
        if compat.status().is_success() {
            let v: Value = compat.json().await.map_err(Self::unavailable)?;
            if v.get("is_compatible") == Some(&Value::Bool(false)) {
                return Err(SchemaError::Incompatible(vec![format!(
                    "registry rejected `{subject}`"
                )]));
            }
        } else if compat.status().as_u16() != 404 {
            return Err(Self::unavailable(compat.status()));
        }
        let resp = self
            .client
            .post(format!("{}/subjects/{subject}/versions", self.base))
            .header("Content-Type", ct)
            .json(&body)
            .send()
            .await
            .map_err(Self::unavailable)?;
        if !resp.status().is_success() {
            return Err(Self::unavailable(resp.status()));
        }
        self.latest(subject).await.map(|r| r.version)
    }

    async fn latest(&self, subject: &str) -> Result<Registered, SchemaError> {
        let resp = self
            .client
            .get(format!("{}/subjects/{subject}/versions/latest", self.base))
            .send()
            .await
            .map_err(Self::unavailable)?;
        if resp.status().as_u16() == 404 {
            return Err(SchemaError::UnknownSubject(subject.into()));
        }
        if !resp.status().is_success() {
            return Err(Self::unavailable(resp.status()));
        }
        let v: Value = resp.json().await.map_err(Self::unavailable)?;
        let schema = v["schema"]
            .as_str()
            .and_then(|s| serde_json::from_str(s).ok())
            .ok_or_else(|| {
                SchemaError::RegistryUnavailable("malformed registry response".into())
            })?;
        Ok(Registered {
            version: v["version"].as_u64().unwrap_or(0) as u32,
            schema,
        })
    }
}

/// Register the built-in swap event schemas (call at deploy/startup; fails
/// startup on an incompatible change).
pub async fn register_all(registry: &dyn SchemaRegistry) -> Result<(), SchemaError> {
    for (subject, raw) in [
        ("swap.order_created", ORDER_CREATED),
        ("swap.order_settled", ORDER_SETTLED),
    ] {
        let schema =
            serde_json::from_str(raw).map_err(|e| SchemaError::InvalidEvent(e.to_string()))?;
        registry.register(subject, schema).await?;
    }
    Ok(())
}

/// Validate `payload` against the subject's registered schema and return the
/// outbox envelope carrying schema version metadata.
pub async fn validate_for_outbox(
    registry: &dyn SchemaRegistry,
    subject: &str,
    payload: Value,
) -> Result<Value, SchemaError> {
    let reg = registry.latest(subject).await?;
    let compiled = jsonschema::JSONSchema::compile(&reg.schema)
        .map_err(|e| SchemaError::InvalidEvent(e.to_string()))?;
    if let Err(errors) = compiled.validate(&payload) {
        let msg = errors.map(|e| e.to_string()).collect::<Vec<_>>().join("; ");
        return Err(SchemaError::InvalidEvent(msg));
    }
    Ok(json!({ "subject": subject, "schema_version": reg.version, "payload": payload }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(s: &str) -> Value {
        serde_json::from_str(s).unwrap()
    }

    #[test]
    fn schemas_are_backward_compatible_with_baseline() {
        for (cur, base) in [
            (
                ORDER_CREATED,
                include_str!("schemas/baseline/order_created.json"),
            ),
            (
                ORDER_SETTLED,
                include_str!("schemas/baseline/order_settled.json"),
            ),
        ] {
            check_backward_compatible(&parse(base), &parse(cur)).unwrap();
        }
    }

    #[test]
    fn classifies_incompatible_changes() {
        let old = parse(ORDER_CREATED);
        let mut removed = old.clone();
        removed["properties"]
            .as_object_mut()
            .unwrap()
            .remove("quote_id");
        let mut retyped = old.clone();
        retyped["properties"]["order_id"]["type"] = json!("integer");
        let mut required = old.clone();
        required["required"] = json!(["order_id", "tenant_id", "quote_id"]);
        for bad in [removed, retyped, required] {
            assert!(matches!(
                check_backward_compatible(&old, &bad),
                Err(SchemaError::Incompatible(_))
            ));
        }
        let mut added = old.clone();
        added["properties"]["note"] = json!({"type": "string"});
        assert!(check_backward_compatible(&old, &added).is_ok());
    }

    #[tokio::test]
    async fn registry_rejects_incompatible_and_stamps_version() {
        let reg = InMemoryRegistry::default();
        register_all(&reg).await.unwrap();
        register_all(&reg).await.unwrap(); // idempotent
        let mut bad = parse(ORDER_CREATED);
        bad["properties"]["order_id"]["type"] = json!("integer");
        assert!(reg.register("swap.order_created", bad).await.is_err());

        let env = validate_for_outbox(
            &reg,
            "swap.order_created",
            json!({"order_id": "o", "tenant_id": "t"}),
        )
        .await
        .unwrap();
        assert_eq!(env["schema_version"], 1);
        assert!(
            validate_for_outbox(&reg, "swap.order_created", json!({"order_id": "o"}))
                .await
                .is_err()
        );
        assert_eq!(
            validate_for_outbox(&reg, "nope", json!({})).await,
            Err(SchemaError::UnknownSubject("nope".into()))
        );
    }
}
