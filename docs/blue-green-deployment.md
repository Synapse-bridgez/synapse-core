# Blue-Green Deployment (#1288)

Blue-green deployment lets a new "green" release run alongside the current
"blue" release and receive traffic only after it passes live health and
readiness checks.  If green fails post-cutover checks, traffic reverts to blue
instantly within a configured rollback window.

This is an **application-level** feature.  Actual traffic steering (load
balancer rule changes, Kubernetes Service selector updates) is handled by the
orchestration layer; this module produces the signal (which slot is currently
active) that the orchestration layer reads.

## Scope

Only schema-compatible releases (no breaking schema changes).  Releases that
require breaking schema migrations must use the existing canary migration
process described in `docs/deployment-notes.md`.

---

## Architecture

```
            ┌──────────────┐          ┌──────────────┐
            │  Blue (old)  │          │ Green (new)  │
            │  :3000       │          │  :3001       │
            └──────────────┘          └──────────────┘
                    ▲                         ▲
                    │                         │
            ┌───────┴─────────────────────────┘
            │     BlueGreenController
            │     (checks /health + /ready on both slots)
            │
            ▼
     BlueGreenState.active_slot   ← orchestration layer reads this
```

`BlueGreenState` is held in-process.  In a multi-instance deployment,
orchestration should read `GET /admin/blue-green/status` from a single
authoritative instance (the leader, determined by `LeaderElection`) rather
than any replica.

---

## Configuration

| Environment variable              | Default                     | Description                          |
|-----------------------------------|-----------------------------|--------------------------------------|
| `BLUE_URL`                        | `http://localhost:3000`     | Base URL of the blue (old) instance  |
| `GREEN_URL`                       | `http://localhost:3001`     | Base URL of the green (new) instance |
| `BLUE_GREEN_ROLLBACK_WINDOW_SECS` | `60`                        | Rollback window in seconds           |

---

## Admin Endpoints

All endpoints require `Authorization: Bearer <ADMIN_API_KEY>`.

### `GET /admin/blue-green/status`

Returns the current active slot and the last cutover attempt record.

```json
{
  "active_slot": "blue",
  "blue_url": "http://blue.internal:3000",
  "green_url": "http://green.internal:3001",
  "rollback_window_secs": 60,
  "last_attempt": null
}
```

### `POST /admin/blue-green/cutover`

Triggers a blue → green cutover.

1. Runs `/health` and `/ready` checks against the green URL `N` times
   (default: 3 consecutive passing rounds) with 2-second pauses between rounds.
2. If pre-checks pass, flips `active_slot` to `green`.
3. Waits 5 seconds for traffic to stabilise.
4. Runs post-cutover checks against the green URL.
5. If post-cutover checks fail, immediately reverts `active_slot` to `blue`
   and records `outcome: "rolled_back"`.

**Request body**: `{}` (currently no parameters)

**Response — success (200):**
```json
{
  "id": "...",
  "outcome": "success",
  "from_slot": "blue",
  "to_slot": "green",
  "pre_checks": [...],
  "post_checks": [...],
  "started_at": "...",
  "completed_at": "..."
}
```

**Response — pre-check failure (422):**
```json
{
  "outcome": "pre_check_failed",
  ...
}
```

**Response — post-cutover rollback (422):**
```json
{
  "outcome": "rolled_back",
  ...
}
```

### `POST /admin/blue-green/rollback`

Instantly reverts the active slot to `blue` regardless of current state.
Returns `200` with a `manual_rollback` attempt record.  This is the fast path
for operator-initiated rollbacks after a successful cutover.

---

## Operational Runbook

### Deploying a new release

1. Deploy the new image to the green slot (separate container / replica set).
2. Verify green is reachable on `$GREEN_URL`.
3. Call `POST /admin/blue-green/cutover` from the admin API (or CI pipeline).
4. Monitor the response:
   - `"outcome": "success"` — green is live, decommission blue at leisure.
   - `"outcome": "pre_check_failed"` — green is not healthy; investigate before retrying.
   - `"outcome": "rolled_back"` — green passed pre-checks but failed post-cutover;
     blue is already restored.  Investigate green before retrying.
5. Update the orchestration layer (e.g. Kubernetes Service selector, load
   balancer rule) to point to the green instance based on
   `GET /admin/blue-green/status` → `active_slot`.

### Emergency rollback

```bash
curl -s -X POST http://localhost:3000/admin/blue-green/rollback \
  -H "Authorization: Bearer $ADMIN_API_KEY" | jq
```

`active_slot` is set to `blue` immediately, before the response is returned.

---

## Kubernetes Integration

The readiness probe at `/ready` (implemented in `src/readiness.rs`) is the
signal that Kubernetes uses to decide whether to include a pod in Service
endpoints.  Blue-green at the application level works alongside this:

- The orchestration layer (Helm chart / Argo Rollouts / Flux) watches
  `GET /admin/blue-green/status` and updates the Service selector to point
  to the active slot's pod label (e.g. `slot: green`).
- Because the green pod's readiness probe must return `200` before the
  `BlueGreenController` proceeds with pre-checks, there is no window where
  green is "live" in Kubernetes before this module has verified it.

This module does **not** replace Kubernetes' own rolling-update mechanism for
day-to-day deployments that do not require zero-downtime traffic control.  Use
blue-green for releases where you need instant rollback capability beyond what
`RollingUpdate + maxUnavailable: 0` provides.
