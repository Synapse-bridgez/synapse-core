# Deployment Notes

## Canary migration note

Apply new schema changes to one canary environment first, verify migrations
and webhook processing stay healthy, then continue rollout to the rest of the
fleet.

## The production image now runs as a non-root user

As of this change, `dockerfile`'s runtime stage creates a fixed-UID user
(`synapse`, UID/GID `10001`) and switches to it with `USER synapse:synapse`
before `CMD`. Previously the container ran as root for the life of every
instance, with no privilege drop — see `.github/workflows/rust.yml`'s
`docker-image-security` job, which now fails the build if a future change
regresses this back to root.

### What this means for custom orchestration

If your deployment does anything that assumes the container runs as root,
it will need to change:

- **Bind mounts / volumes the app writes to**: audit-log archives default to
  `/tmp/audit_archives` and profiling flamegraphs are written under `/tmp` —
  both world-writable in the `debian:bookworm-slim` base image, so these
  need no extra configuration. Backups are different: `Config::backup_dir`
  (`BACKUP_DIR` env var) defaults to the *relative* path `./backups`, i.e.
  `/app/backups` given this image's `WORKDIR /app` — not `/tmp`. `dockerfile`
  now runs `chown synapse:synapse /app` specifically so the app can create
  that directory on demand under the non-root user; if you override
  `BACKUP_DIR` to an absolute, bind-mounted path instead, that directory
  needs to be writable by UID `10001` the same way. If you've redirected any
  of these paths (`AUDIT_LOG_ARCHIVE_DIR`, etc.) to a custom bind-mounted
  directory, that directory needs to be writable by UID `10001`, e.g.
  `chown -R 10001:10001 <dir>` on the host path, or a Kubernetes
  `securityContext.fsGroup` that includes `10001`.
- **`docker-compose.yml`'s `./migrations:/app/migrations` bind mount**: this
  is read-only from the app's perspective (migrations are read by
  `sqlx::migrate::Migrator`, never written to at runtime), so ordinary
  world-readable file permissions on the host (the repo's default) are
  sufficient — no chown needed for this one.
- **`PORT` / listen address**: unaffected. The app listens on port `3000`,
  which is above `1024` and has never required root to bind.
- **Kubernetes `securityContext`**: if you're already setting
  `runAsNonRoot: true` and it was previously failing against this image,
  it should now pass. You can additionally pin `runAsUser: 10001` to match
  the image's default explicitly.
- **`docker-compose.dev.yml` / `docker-compose.load.yml`**: unaffected —
  both run the app service directly from the `rust:latest` base image via
  `cargo run`/`cargo watch`, not from `dockerfile`, so they still run as
  whatever user that base image defaults to. This change only touches the
  production image built from `dockerfile`.

### Verifying locally

```bash
docker build -f dockerfile -t synapse-core:local .
docker inspect --format '{{.Config.User}}' synapse-core:local
# => synapse:synapse
docker run --rm synapse-core:local id
# => uid=10001(synapse) gid=10001(synapse) groups=10001(synapse)
```
