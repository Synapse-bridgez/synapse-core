# Deployment Guide — Rolling Restarts & Connection Draining

This document covers how synapse-core handles rolling restarts in Kubernetes with zero dropped in-flight requests.

---

## How It Works

1. Kubernetes sends a `preStop` hook call to `POST /admin/drain` before sending SIGTERM.
2. The drain endpoint sets the readiness flag to `false` and starts a countdown timer (default 30 s).
3. The `/ready` probe immediately returns `503`, so the load balancer stops routing new traffic to this pod.
4. In-flight requests continue to be served until the drain timeout elapses.
5. After the timeout the process exits cleanly (exit code 0).

If SIGTERM arrives without a prior drain call (e.g. direct `kubectl delete pod`), the graceful shutdown handler in `main.rs` starts the drain automatically before stopping the server.

---

## Endpoints

### `POST /admin/drain`

Starts the connection drain. Safe to call multiple times — subsequent calls return `already_draining`.

**Authentication:** Requires `Authorization: Bearer <ADMIN_API_KEY>` header.

**Response (200):**
```json
{ "status": "draining", "drain_timeout_secs": 30 }
```

**Response when already draining (200):**
```json
{ "status": "already_draining", "drain_timeout_secs": 30 }
```

### `GET /ready`

Kubernetes readiness probe. Returns `200` when ready, `503` during drain or before initialization completes.

**Response (200):**
```json
{ "status": "ready", "draining": false }
```

**Response (503):**
```json
{ "status": "not_ready", "draining": true }
```

---

## Kubernetes Deployment Spec

```yaml
apiVersion: apps/v1
kind: Deployment
metadata:
  name: synapse-core
spec:
  replicas: 3
  strategy:
    type: RollingUpdate
    rollingUpdate:
      maxUnavailable: 0      # never take a pod fully offline before a new one is ready
      maxSurge: 1            # spin up one extra pod during rollout
  template:
    spec:
      terminationGracePeriodSeconds: 60   # must be > drain timeout (30 s) + buffer
      containers:
        - name: synapse-core
          image: synapse-core:latest
          ports:
            - containerPort: 3000
          env:
            - name: APP_ENV
              value: production
            - name: DRAIN_TIMEOUT_SECS   # optional override; default is 30
              value: "30"
          readinessProbe:
            httpGet:
              path: /ready
              port: 3000
            initialDelaySeconds: 5
            periodSeconds: 5
            failureThreshold: 2
          livenessProbe:
            httpGet:
              path: /health
              port: 3000
            initialDelaySeconds: 10
            periodSeconds: 15
          lifecycle:
            preStop:
              httpGet:
                path: /admin/drain
                port: 3000
                httpHeaders:
                  - name: Authorization
                    value: Bearer $(ADMIN_API_KEY)
```

> **Note:** Set `terminationGracePeriodSeconds` to at least `DRAIN_TIMEOUT_SECS + 15` to give the process time to finish draining before Kubernetes force-kills it.

---

## Configuring the Drain Timeout

The drain timeout defaults to 30 seconds. To change it, set the `DRAIN_TIMEOUT_SECS` environment variable. The `ReadinessState` is constructed in `src/main.rs` — update the constructor call there if you need a code-level default change.

---

## Testing Locally

Start the server, then in a second terminal:

```bash
# Trigger drain
curl -s -X POST http://localhost:3000/admin/drain \
  -H "Authorization: Bearer dev-admin-key" | jq

# Readiness probe should now return 503
curl -s -o /dev/null -w "%{http_code}" http://localhost:3000/ready
# => 503
```
