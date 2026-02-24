# Admin API Authentication Tests

This project implements comprehensive tests for admin API authentication and authorization.

## Project Structure

```
.
├── src/
│   ├── lib.rs                    # Library root
│   ├── models.rs                 # Data models (Claims, Role)
│   ├── middleware/
│   │   ├── mod.rs
│   │   └── auth.rs              # Authentication middleware
│   └── handlers/
│       ├── mod.rs
│       └── admin.rs             # Admin API handlers
├── tests/
│   └── admin_auth_test.rs       # Admin authentication tests
└── Cargo.toml                   # Project dependencies

```

## Features

### Authentication Middleware (`src/middleware/auth.rs`)

- JWT-based authentication
- Role-based authorization
- Token validation and expiration checking
- Bearer token extraction from Authorization header

### Admin Handlers (`src/handlers/admin.rs`)

- `/admin/dashboard` - Admin dashboard endpoint
- `/admin/users` - User management endpoint

### Test Coverage (`tests/admin_auth_test.rs`)

1. **test_admin_api_valid_credentials** - Verifies admin API access with valid admin credentials
2. **test_admin_api_invalid_credentials** - Tests rejection of invalid/malformed tokens
3. **test_admin_api_missing_credentials** - Tests rejection when Authorization header is missing
4. **test_admin_api_expired_token** - Tests rejection of expired JWT tokens
5. **test_admin_api_authorization** - Tests role-based authorization (Admin, User, Guest)

## Running Tests

```bash
cargo test
```

To run only admin auth tests:

```bash
cargo test --test admin_auth_test
```

To run with output:

```bash
cargo test -- --nocapture
```

## Git Workflow

Create feature branch:

```bash
git checkout -b feature/issue-80-admin-auth-tests
```

Add and commit changes:

```bash
git add .
git commit -m "Add admin API authentication tests

- Implement JWT-based authentication middleware
- Add admin handlers with role-based access control
- Create comprehensive test suite covering:
  - Valid credentials
  - Invalid credentials
  - Missing credentials
  - Expired tokens
  - Role-based authorization"
```

Push to remote:

```bash
git push origin feature/issue-80-admin-auth-tests
```

Then create a Pull Request targeting the `develop` branch.

## Dependencies

- `actix-web` - Web framework
- `jsonwebtoken` - JWT token handling
- `chrono` - Date/time operations for token expiration
- `serde` - Serialization/deserialization

## Security Notes

- Test secret key is hardcoded for testing purposes only
- In production, use environment variables for secrets
- Token expiration is set to 1 hour for valid tokens
- All admin endpoints require Admin role
