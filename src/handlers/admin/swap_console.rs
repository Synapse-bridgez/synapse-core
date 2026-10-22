//! Swap engine operations console: role-gated write actions with an immutable,
//! hash-chained audit log. Actions go through [`ConsoleOps`] (the same service
//! layer automated paths use), never direct SQL. See `docs/swap-console.md`.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::sync::Mutex;
use uuid::Uuid;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConsoleRole {
    /// Read views only.
    ReadOnly,
    /// Read views plus the enumerated write actions.
    Approver,
}

/// The complete, explicit set of sanctioned write actions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConsoleAction {
    RequeueDeadLetter,
    AckAlert,
    ResumePair,
}

impl ConsoleAction {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::RequeueDeadLetter => "requeue_dead_letter",
            Self::AckAlert => "ack_alert",
            Self::ResumePair => "resume_pair",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ConsoleError {
    #[error("role not permitted to perform this action")]
    Forbidden,
    #[error("a reason of at least 10 characters is required")]
    ReasonRequired,
    #[error("action must be explicitly confirmed")]
    NotConfirmed,
    #[error("target not visible to this operator")]
    TenantScope,
    #[error("operation failed: {0}")]
    Operation(String),
}

pub fn authorize(role: ConsoleRole, _action: ConsoleAction) -> Result<(), ConsoleError> {
    match role {
        ConsoleRole::Approver => Ok(()),
        ConsoleRole::ReadOnly => Err(ConsoleError::Forbidden),
    }
}

/// Tenant-scoped visibility: platform operators (`None`) see everything,
/// tenant-scoped operators only their own tenant's rows.
pub fn visible_to(operator_tenant: Option<Uuid>, row_tenant: Uuid) -> bool {
    operator_tenant.map_or(true, |t| t == row_tenant)
}

#[derive(Debug, Clone, Deserialize)]
pub struct ConsoleRequest {
    pub action: ConsoleAction,
    /// Order id, alert id or pair symbol the action applies to.
    pub target: String,
    pub target_tenant: Option<Uuid>,
    pub reason: String,
    pub confirm: bool,
    /// Repeating a request with the same key returns the first result.
    pub idempotency_key: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct AuditEntry {
    pub seq: u64,
    pub operator: String,
    pub action: &'static str,
    pub target: String,
    pub reason: String,
    pub outcome: String,
    pub prev_hash: String,
    pub hash: String,
}

fn entry_hash(
    prev: &str,
    seq: u64,
    op: &str,
    action: &str,
    target: &str,
    reason: &str,
    outcome: &str,
) -> String {
    let mut h = Sha256::new();
    for p in [prev, &seq.to_string(), op, action, target, reason, outcome] {
        h.update((p.len() as u64).to_be_bytes());
        h.update(p.as_bytes());
    }
    hex::encode(h.finalize())
}

/// Append-only audit log; each entry commits to the previous one so edits or
/// deletions are detectable. Production persists to `swap_console_audit_log`,
/// which is protected by an update/delete-rejecting trigger.
#[derive(Default)]
pub struct AuditLog {
    entries: Mutex<Vec<AuditEntry>>,
}

impl AuditLog {
    pub fn append(
        &self,
        operator: &str,
        action: ConsoleAction,
        target: &str,
        reason: &str,
        outcome: &str,
    ) -> AuditEntry {
        let mut es = self.entries.lock().unwrap();
        let prev = es.last().map(|e| e.hash.clone()).unwrap_or_default();
        let seq = es.len() as u64 + 1;
        let hash = entry_hash(
            &prev,
            seq,
            operator,
            action.as_str(),
            target,
            reason,
            outcome,
        );
        let e = AuditEntry {
            seq,
            operator: operator.into(),
            action: action.as_str(),
            target: target.into(),
            reason: reason.into(),
            outcome: outcome.into(),
            prev_hash: prev,
            hash,
        };
        es.push(e.clone());
        e
    }

    pub fn entries(&self) -> Vec<AuditEntry> {
        self.entries.lock().unwrap().clone()
    }

    pub fn verify_chain(&self) -> bool {
        let mut prev = String::new();
        self.entries().iter().all(|e| {
            let ok = e.prev_hash == prev
                && e.hash
                    == entry_hash(
                        &e.prev_hash,
                        e.seq,
                        &e.operator,
                        e.action,
                        &e.target,
                        &e.reason,
                        &e.outcome,
                    );
            prev = e.hash.clone();
            ok
        })
    }
}

/// Service-layer operations the console delegates to.
pub trait ConsoleOps: Send + Sync {
    fn execute(&self, action: ConsoleAction, target: &str) -> Result<String, String>;
}

pub struct ConsoleService<O: ConsoleOps> {
    ops: O,
    pub audit: AuditLog,
    done: Mutex<HashMap<String, String>>,
}

impl<O: ConsoleOps> ConsoleService<O> {
    pub fn new(ops: O) -> Self {
        Self {
            ops,
            audit: AuditLog::default(),
            done: Mutex::new(HashMap::new()),
        }
    }

    /// Validate, authorize, execute via the service layer, and audit. Denied
    /// attempts are audited too. Idempotent on `idempotency_key`.
    pub fn perform(
        &self,
        operator: &str,
        role: ConsoleRole,
        operator_tenant: Option<Uuid>,
        req: &ConsoleRequest,
    ) -> Result<String, ConsoleError> {
        if let Err(e) = authorize(role, req.action) {
            self.audit
                .append(operator, req.action, &req.target, &req.reason, "denied");
            return Err(e);
        }
        if req.reason.trim().len() < 10 {
            return Err(ConsoleError::ReasonRequired);
        }
        if !req.confirm {
            return Err(ConsoleError::NotConfirmed);
        }
        if let Some(t) = req.target_tenant {
            if !visible_to(operator_tenant, t) {
                self.audit.append(
                    operator,
                    req.action,
                    &req.target,
                    &req.reason,
                    "denied_tenant_scope",
                );
                return Err(ConsoleError::TenantScope);
            }
        }
        if let Some(prev) = self.done.lock().unwrap().get(&req.idempotency_key) {
            return Ok(prev.clone());
        }
        match self.ops.execute(req.action, &req.target) {
            Ok(res) => {
                self.audit
                    .append(operator, req.action, &req.target, &req.reason, "ok");
                self.done
                    .lock()
                    .unwrap()
                    .insert(req.idempotency_key.clone(), res.clone());
                Ok(res)
            }
            Err(e) => {
                self.audit
                    .append(operator, req.action, &req.target, &req.reason, "error");
                Err(ConsoleError::Operation(e))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Ops(Mutex<u32>);
    impl ConsoleOps for Ops {
        fn execute(&self, _a: ConsoleAction, t: &str) -> Result<String, String> {
            *self.0.lock().unwrap() += 1;
            Ok(format!("done:{t}"))
        }
    }

    fn req(key: &str) -> ConsoleRequest {
        ConsoleRequest {
            action: ConsoleAction::AckAlert,
            target: "alert-1".into(),
            target_tenant: None,
            reason: "investigated, benign".into(),
            confirm: true,
            idempotency_key: key.into(),
        }
    }

    #[test]
    fn read_only_is_denied_every_write_and_denial_is_audited() {
        let svc = ConsoleService::new(Ops(Mutex::new(0)));
        for a in [
            ConsoleAction::RequeueDeadLetter,
            ConsoleAction::AckAlert,
            ConsoleAction::ResumePair,
        ] {
            let mut r = req("k");
            r.action = a;
            assert_eq!(
                svc.perform("op", ConsoleRole::ReadOnly, None, &r),
                Err(ConsoleError::Forbidden)
            );
        }
        assert_eq!(svc.audit.entries().len(), 3);
        assert_eq!(*svc.ops.0.lock().unwrap(), 0);
    }

    #[test]
    fn approver_actions_are_idempotent_audited_and_chained() {
        let svc = ConsoleService::new(Ops(Mutex::new(0)));
        assert!(svc
            .perform("op", ConsoleRole::Approver, None, &req("k"))
            .is_ok());
        assert!(svc
            .perform("op", ConsoleRole::Approver, None, &req("k"))
            .is_ok());
        assert_eq!(*svc.ops.0.lock().unwrap(), 1);
        assert!(svc.audit.verify_chain());
        let mut bad = req("k2");
        bad.reason = "x".into();
        assert_eq!(
            svc.perform("op", ConsoleRole::Approver, None, &bad),
            Err(ConsoleError::ReasonRequired)
        );
    }

    #[test]
    fn tenant_scope_is_enforced() {
        let (a, b) = (Uuid::new_v4(), Uuid::new_v4());
        assert!(visible_to(None, a) && visible_to(Some(a), a) && !visible_to(Some(a), b));
        let svc = ConsoleService::new(Ops(Mutex::new(0)));
        let mut r = req("k");
        r.target_tenant = Some(b);
        assert_eq!(
            svc.perform("op", ConsoleRole::Approver, Some(a), &r),
            Err(ConsoleError::TenantScope)
        );
    }
}
