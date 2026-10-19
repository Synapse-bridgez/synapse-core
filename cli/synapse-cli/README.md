# synapse-cli

Command-line interface for Synapse.

## Installation

```sh
cargo install --path cli/synapse-cli
```

## Usage

```sh
synapse-cli <command> [options]
```

### Commands

| Command | Description |
| --- | --- |
| `init` | Initialize a new local configuration. |
| `doctor` | Diagnose local environment and configuration issues. |
| `webhooks listen` | Tunnel real webhook deliveries to a local port for development. |

## `webhooks listen`

`synapse webhooks listen --port 4000 --path /hooks/synapse` lets you develop a
webhook handler locally, similar to `stripe listen`. It:

1. Starts a public tunnel to `localhost:<port>` using an existing tool
   (`cloudflared` quick tunnels by default; override with `--tunnel-cmd`, where
   `{port}` is substituted, or reuse a running tunnel with `--tunnel-url`).
2. Registers the tunnel URL as a temporary webhook endpoint through the normal
   admin API (`POST /admin/webhooks/endpoints/batch`) and prints the endpoint ID
   and signing secret (`--secret` / `SYNAPSE_WEBHOOK_SECRET` to supply your own).
3. Disables the temporary endpoint and stops the tunnel when you press Ctrl-C,
   on SIGTERM, or if the tunnel exits.

Deliveries go straight through the tunnel, so the original
`X-Webhook-Timestamp` and `X-Webhook-Signature` headers and raw body reach your
handler unchanged; verify them with the printed secret as described in
`docs/webhook-authentication.md`.

`SIGKILL` cannot be handled. If the CLI is killed that way, disable the endpoint
manually using the ID printed at startup.

## `doctor`

`synapse-cli doctor` runs a set of independent diagnostic checks against your
local environment and reports which checks pass and which fail, along with a
specific suggested fix for every failure. It performs diagnosis and guidance
only — it never modifies your configuration or credentials automatically.

The command runs the following checks:

| Check | What it verifies |
| --- | --- |
| Config | The config file exists, is readable, and parses as valid configuration. |
| Credentials | The configured credentials are present and accepted by a live auth check. |
| Connectivity | The target server is reachable, and reports its latency. |
| Version | The CLI version is compatible with the target server version. |

### Output

Passing checks are listed under `PASS`, failing checks under `FAIL`. Each
failure includes a `fix:` line describing the concrete next step. For example:

```
$ synapse-cli doctor
PASS  config        configuration loaded from ~/.config/synapse/config.toml
PASS  credentials   credentials accepted by server
FAIL  connectivity  could not reach https://api.synapse.example (connection refused)
      fix: verify the server URL in your config and that the server is running
PASS  version       CLI 0.4.1 is compatible with server 0.4.0

1 check failed. Address the suggested fixes above and re-run `synapse-cli doctor`.
```

If no configuration has ever been initialized, `doctor` reports the config
check as failing and points you at `synapse-cli init` rather than emitting a
generic error:

```
FAIL  config        no configuration found at ~/.config/synapse/config.toml
      fix: run `synapse-cli init` to create a configuration
```

### Exit codes

| Code | Meaning |
| --- | --- |
| `0` | All checks passed. |
| `1` | One or more checks failed; see the suggested fixes. |

## Versioning

See [VERSIONING.md](./VERSIONING.md) for the CLI-to-server version
compatibility matrix.
