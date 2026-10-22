# Tenant Sandbox (#1527)

An isolated environment for testing swap quoting, orders and webhooks without
real funds or production credentials.

## Run

```bash
export SANDBOX_DB_PASSWORD=<random>
docker compose -f docker-compose.sandbox.yml up -d      # http://localhost:3100
APP_ENV=sandbox scripts/sandbox-reset.sh                # wipe + reseed
```

It runs the same app image with `APP_ENV=sandbox`, its own Postgres and Redis
(no shared data or credentials with production), synthetic liquidity at a
configurable price (`SANDBOX_PRICE_USDC_XLM`), and tighter rate limits.

## Tenant credentials and webhooks

Create sandbox tenants and API keys via the standard admin API against the
sandbox URL; these credentials are only valid there. Webhooks are signed per
`docs/webhook-signing.md`; inspect/replay deliveries with the existing tooling in
`docs/webhook-replay-quickstart.md`.

## Limitations

- Responses carry `SANDBOX_RESPONSE_LABEL=non-production`; treat all values as simulated.
- No real liquidity or settlement; prices are fixed, not market data.
- Data can be reset at any time. Do not store anything you need to keep.
- Out of scope: production onboarding and mainnet settlement.
