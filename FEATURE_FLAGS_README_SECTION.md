# Feature Flags System - README Update

Add this section to the main README.md under a new "## 🚩 Feature Flags" heading:

---

## 🚩 Feature Flags

The application includes a feature flag system for runtime feature toggling without redeployment.

### Quick Start

**Check if a feature is enabled:**
```rust
if state.feature_flags.is_enabled("experimental_processor").await {
    // Feature-specific logic
}
```

**List all flags:**
```bash
curl http://localhost:3000/admin/flags
```

**Toggle a flag:**
```bash
curl -X PUT http://localhost:3000/admin/flags/experimental_processor \
  -H "Content-Type: application/json" \
  -d '{"enabled": true}'
```

### Default Flags

- `experimental_processor` - Enable experimental transaction processor logic
- `new_asset_support` - Enable support for new asset types

### How It Works

- **Database-backed**: Flags stored in `feature_flags` table
- **In-memory cache**: Fast O(1) lookups with periodic refresh (1 hour)
- **Admin API**: Manage flags via REST endpoints
- **Immediate updates**: API changes update cache instantly

See [docs/feature_flags.md](docs/feature_flags.md) for detailed documentation.

---
