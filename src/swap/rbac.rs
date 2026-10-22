//! Role-based access control for swap engine APIs.
//!
//! A single role-to-permission matrix ([`Role::permissions`]) and a single
//! operation-to-scope map ([`Operation::required_scope`]) are evaluated by
//! [`authorize`], which every surface (REST, GraphQL, CLI, webhooks) calls.

use std::collections::HashSet;
use std::fmt;

/// Permission scopes for swap operations.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Scope {
    QuotesRead,
    OrdersRead,
    OrdersCreate,
    OrdersCancel,
    LimitsRead,
    LimitsWrite,
    Admin,
}

impl Scope {
    pub fn as_str(&self) -> &'static str {
        match self {
            Scope::QuotesRead => "quotes:read",
            Scope::OrdersRead => "orders:read",
            Scope::OrdersCreate => "orders:create",
            Scope::OrdersCancel => "orders:cancel",
            Scope::LimitsRead => "limits:read",
            Scope::LimitsWrite => "limits:write",
            Scope::Admin => "admin",
        }
    }
}

/// Roles that can be carried in token claims.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Role {
    Viewer,
    Trader,
    Admin,
    Operator,
    Service,
}

impl Role {
    pub fn parse(s: &str) -> Option<Role> {
        match s {
            "viewer" => Some(Role::Viewer),
            "trader" => Some(Role::Trader),
            "admin" => Some(Role::Admin),
            "operator" => Some(Role::Operator),
            "service" => Some(Role::Service),
            _ => None,
        }
    }

    /// The role-to-permission matrix.
    pub fn permissions(&self) -> &'static [Scope] {
        use Scope::*;
        match self {
            Role::Viewer => &[QuotesRead, OrdersRead, LimitsRead],
            Role::Trader => &[
                QuotesRead,
                OrdersRead,
                OrdersCreate,
                OrdersCancel,
                LimitsRead,
            ],
            Role::Admin => &[
                QuotesRead,
                OrdersRead,
                OrdersCreate,
                OrdersCancel,
                LimitsRead,
                LimitsWrite,
            ],
            Role::Operator => &[
                QuotesRead,
                OrdersRead,
                OrdersCancel,
                LimitsRead,
                LimitsWrite,
                Admin,
            ],
            Role::Service => &[QuotesRead, OrdersRead, OrdersCreate, OrdersCancel],
        }
    }
}

/// Every swap API operation, regardless of transport.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Operation {
    GetQuote,
    GetOrder,
    SubmitOrder,
    CancelOrder,
    GetLimits,
    SetLimits,
    AdminAction,
}

impl Operation {
    pub fn required_scope(&self) -> Scope {
        match self {
            Operation::GetQuote => Scope::QuotesRead,
            Operation::GetOrder => Scope::OrdersRead,
            Operation::SubmitOrder => Scope::OrdersCreate,
            Operation::CancelOrder => Scope::OrdersCancel,
            Operation::GetLimits => Scope::LimitsRead,
            Operation::SetLimits => Scope::LimitsWrite,
            Operation::AdminAction => Scope::Admin,
        }
    }
}

/// Authorization-relevant claims parsed from a token.
#[derive(Debug, Clone, Default)]
pub struct Claims {
    pub tenant_id: Option<uuid::Uuid>,
    pub roles: Vec<String>,
    /// Unix expiry; `None` means the claim set carries no expiry (rejected).
    pub exp: Option<i64>,
    /// Service-to-service credential: acts on behalf of any tenant.
    pub service: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AuthzError {
    /// Missing, expired or role-less claims.
    InvalidClaims,
    /// Authenticated but lacking the required scope.
    Forbidden { required: &'static str },
    /// Target tenant differs from the caller's tenant.
    CrossTenant,
}

impl AuthzError {
    pub fn status_code(&self) -> u16 {
        match self {
            AuthzError::InvalidClaims => 401,
            _ => 403,
        }
    }
}

impl fmt::Display for AuthzError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            AuthzError::InvalidClaims => write!(f, "invalid or expired credentials"),
            AuthzError::Forbidden { required } => write!(f, "missing required scope: {required}"),
            AuthzError::CrossTenant => write!(f, "access to another tenant is not permitted"),
        }
    }
}

impl std::error::Error for AuthzError {}

/// Resolve the effective scope set; unknown roles are ignored.
pub fn effective_scopes(claims: &Claims) -> HashSet<Scope> {
    let mut roles: Vec<Role> = claims.roles.iter().filter_map(|r| Role::parse(r)).collect();
    if claims.service {
        roles.push(Role::Service);
    }
    roles
        .iter()
        .flat_map(|r| r.permissions().iter().copied())
        .collect()
}

/// Central authorization check. Denials are audited via `tracing` without
/// logging tenant identifiers or claim contents beyond the operation.
pub fn authorize(
    claims: &Claims,
    op: Operation,
    target_tenant: Option<uuid::Uuid>,
    now: i64,
) -> Result<(), AuthzError> {
    let result = check(claims, op, target_tenant, now);
    if let Err(e) = &result {
        tracing::warn!(operation = ?op, reason = %e, "swap authorization denied");
    }
    result
}

fn check(
    claims: &Claims,
    op: Operation,
    target_tenant: Option<uuid::Uuid>,
    now: i64,
) -> Result<(), AuthzError> {
    match claims.exp {
        Some(exp) if exp > now => {}
        _ => return Err(AuthzError::InvalidClaims),
    }
    let scopes = effective_scopes(claims);
    if scopes.is_empty() {
        return Err(AuthzError::InvalidClaims);
    }
    let required = op.required_scope();
    if !scopes.contains(&required) {
        return Err(AuthzError::Forbidden {
            required: required.as_str(),
        });
    }
    if !claims.service && !scopes.contains(&Scope::Admin) {
        if let Some(target) = target_tenant {
            if claims.tenant_id != Some(target) {
                return Err(AuthzError::CrossTenant);
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: i64 = 1_000;
    const OPS: [Operation; 7] = [
        Operation::GetQuote,
        Operation::GetOrder,
        Operation::SubmitOrder,
        Operation::CancelOrder,
        Operation::GetLimits,
        Operation::SetLimits,
        Operation::AdminAction,
    ];

    fn claims(role: &str, tenant: uuid::Uuid) -> Claims {
        Claims {
            tenant_id: Some(tenant),
            roles: vec![role.into()],
            exp: Some(NOW + 60),
            service: false,
        }
    }

    #[test]
    fn permission_matrix() {
        let t = uuid::Uuid::new_v4();
        // (role, allowed ops by index in OPS)
        let table: [(&str, [bool; 7]); 5] = [
            ("viewer", [true, true, false, false, true, false, false]),
            ("trader", [true, true, true, true, true, false, false]),
            ("admin", [true, true, true, true, true, true, false]),
            ("operator", [true, true, false, true, true, true, true]),
            ("service", [true, true, true, true, false, false, false]),
        ];
        for (role, allowed) in table {
            for (op, want) in OPS.iter().zip(allowed) {
                let got = authorize(&claims(role, t), *op, Some(t), NOW).is_ok();
                assert_eq!(got, want, "{role} {op:?}");
            }
        }
    }

    #[test]
    fn expired_missing_or_unknown_claims_rejected() {
        let t = uuid::Uuid::new_v4();
        let mut c = claims("trader", t);
        c.exp = Some(NOW);
        assert_eq!(
            authorize(&c, Operation::GetQuote, None, NOW),
            Err(AuthzError::InvalidClaims)
        );
        c.exp = None;
        assert_eq!(
            authorize(&c, Operation::GetQuote, None, NOW),
            Err(AuthzError::InvalidClaims)
        );
        let c = claims("root", t);
        let err = authorize(&c, Operation::GetQuote, None, NOW).unwrap_err();
        assert_eq!(err.status_code(), 401);
    }

    #[test]
    fn cross_tenant_denied_unless_service_or_operator() {
        let (a, b) = (uuid::Uuid::new_v4(), uuid::Uuid::new_v4());
        assert_eq!(
            authorize(&claims("trader", a), Operation::SubmitOrder, Some(b), NOW),
            Err(AuthzError::CrossTenant)
        );
        assert!(authorize(&claims("operator", a), Operation::GetOrder, Some(b), NOW).is_ok());
        let svc = Claims {
            service: true,
            exp: Some(NOW + 1),
            ..Default::default()
        };
        assert!(authorize(&svc, Operation::SubmitOrder, Some(b), NOW).is_ok());
    }

    #[test]
    fn privilege_escalation_denied() {
        let t = uuid::Uuid::new_v4();
        let err = authorize(&claims("trader", t), Operation::SetLimits, Some(t), NOW).unwrap_err();
        assert_eq!(
            err,
            AuthzError::Forbidden {
                required: "limits:write"
            }
        );
        assert_eq!(err.status_code(), 403);
    }
}
