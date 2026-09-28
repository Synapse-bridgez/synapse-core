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
