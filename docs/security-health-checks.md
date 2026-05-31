# Security Health Checks

This document describes the health check mechanisms implemented in the security module (`src/security/`) and how they integrate with the broader application health endpoint.

## Overview

The security module does not implement traditional "health checks" that probe external services. Instead, it provides **validation functions that act as liveness checks** for the security layer:

- **Session validation** (`validate_session_params`, `validate_session`) determines whether sessions are properly configured and not expired/revoked
- These checks are non-fatal; failing validation does not cause the application to become unhealthy
- The security module operates in **degraded mode** if sessions are invalid—users are denied access rather than the entire service failing

## Health Check Functions

### 1. `validate_session_params(user_id: &str, ttl_seconds: i64) -> Result<(), SessionValidationError>`

**Purpose**: Validates session creation parameters before creating a new session.

**What it checks**:
- User ID is non-empty
- User ID does not exceed 128 characters
- TTL is between 1 and 86400 seconds (24 hours)

**Failure modes**:
- `SessionValidationError::EmptyUserId` — User ID is missing; indicates misconfiguration
- `SessionValidationError::UserIdTooLong` — User ID exceeds limits; indicates input validation failure
- `SessionValidationError::InvalidTtl` — TTL is out of bounds; indicates invalid request or misconfiguration

**Caller responsibility**: Reject session creation if validation fails. Log the specific error for debugging.

### 2. `validate_session(session: &SessionRecord) -> Result<(), SessionValidationError>`

**Purpose**: Validates whether an existing session is still usable for authorization.

**What it checks**:
- Session is marked as active (not revoked)
- Session has not expired (current time is before `expires_at`)

**Failure modes**:
- `SessionValidationError::Inactive` — Session was explicitly revoked (security event)
- `SessionValidationError::Expired` — Session exceeded TTL; stale credential

**Caller responsibility**:
- On `Inactive`: Deny access immediately; this is a security event
- On `Expired`: Prompt user for re-authentication

## Integration with Health Endpoint

The security module's validation functions are **local checks** and do not participate in the main `/health` endpoint. The `/health` endpoint (in `handlers/`) aggregates checks from:

- **PostgreSQL** (critical dependency)
- **Redis** (non-critical; used for rate limiting and session storage)
- **Horizon** (non-critical; external Stellar blockchain service)

Session validation is performed **at request time** by handler middleware or per-request logic, not as part of the health check flow.

## Behavior Under Degraded Conditions

### When session validation fails

1. **Invalid TTL on session creation** → Reject the session creation; inform the client
2. **Expired session** → User sees authentication error; prompt for re-login
3. **Revoked session** → Deny access immediately; log as security event

### Cascading failures

- If the **application itself is unhealthy** (PostgreSQL down, etc.), the `/health` endpoint returns non-200
- Session validation is still performed for in-flight requests, but new requests may be rejected if the session store is unavailable
- The system is designed to **reject unvalidated sessions rather than allow them through**

## Adding a New Security Health Check

To add a new validation function to the security module:

1. **Define the validation logic** as a function returning `Result<T, SessionValidationError>`
   - Or extend `SessionValidationError` with new variants if needed

2. **Add comprehensive doc comments** documenting:
   - What the check verifies
   - What failure means for security
   - What the caller should do on failure

3. **Ensure non-fatal behavior**: The check should never panic; return errors gracefully

4. **Update the module-level doc** in `src/security/mod.rs` to link to the new function

5. **Add tests** in the same file or a separate test module

Example:

```rust
/// Validates that a session's user has required permissions.
/// Returns `SessionValidationError::Unauthorized` if permissions are insufficient.
pub fn validate_session_permissions(
    session: &SessionRecord,
    required_perms: &[Permission],
) -> Result<(), SessionValidationError> {
    // Load permissions from session metadata or store
    // Check that all required_perms are present
    Ok(())
}
```

## Testing Health Checks

To verify session validation works correctly:

```bash
# Run security module tests
cargo test --lib security::session

# Run a specific test
cargo test --lib security::session::tests::test_expired_session
```

All test cases are in `src/security/session.rs` under `#[cfg(test)]`.

## References

- `src/security/mod.rs` — Module documentation and re-exports
- `src/security/session.rs` — Session validation implementation and tests
- `src/health.rs` — Main health check endpoint (external dependencies)
- `docs/error-catalog.md` — Error handling patterns
