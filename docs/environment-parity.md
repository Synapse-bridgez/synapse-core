# Environment Configuration Parity

The admin-only `GET /admin/config/export` endpoint returns an allowlisted,
read-only snapshot of effective environment settings, resource limits, and
feature-flag enabled values. It never returns credentials, connection strings,
or secret values. Use the script below to compare development, staging, and
production snapshots:

```sh
export DEV_ADMIN_API_KEY='...'
export STAGING_ADMIN_API_KEY='...'
export PROD_ADMIN_API_KEY='...'
python scripts/config-parity.py \
  --dev https://dev.example.invalid \
  --staging https://staging.example.invalid \
  --prod https://api.example.invalid
```

The command exits `0` when all differences match the documented exceptions,
`1` when an unexpected divergence needs review, and `2` when a snapshot cannot
be fetched or parsed. Add an approved difference to
`scripts/config-parity-exceptions.json` with the exact value for every
environment; partial or changed values remain unexpected. Feature-flag
divergences are unexpected by default.

Protect the endpoint with the existing admin API key and restrict its network
exposure like other `/admin/*` routes. The diff tool sends only authenticated
GET requests and does not apply or synchronize configuration.