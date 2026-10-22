# Swap dry-run

`swap::simulation::SimulationContext` runs the real `PricingEngine` (routing, fee, slippage) with no persistence or settlement and returns the normal quote schema with `simulated: true`. Enable with `SWAP_DRY_RUN_ENABLED=true`; per-tenant quota `SWAP_DRY_RUN_QUOTA_PER_MIN` (default 60), counted separately from real orders. Stale market data adds a `warnings` entry.
