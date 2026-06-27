# Synapse CLI

Rust command-line interface for interacting with the Synapse API.

## Installation

```bash
cd cli/synapse-cli
cargo build --release
```

## Configuration

Set API credentials via environment variables or CLI flags:

```bash
export SYNAPSE_BASE_URL="https://api.synapse.example.com"
export SYNAPSE_API_KEY="your-api-key-here"
```

Or pass them as CLI arguments:

```bash
synapse --base-url https://api.synapse.example.com --api-key your-key transactions get <id>
```

## Commands

### transactions get

Fetch a single transaction by its UUID.

**Usage:**
```bash
synapse transactions get <ID> [--format <FORMAT>]
```

**Arguments:**
- `ID` - Transaction UUID (required)

**Options:**
- `--format <FORMAT>` - Output format: `table` (default) or `json`

**Exit codes:**
- `0` - Success
- `1` - Transaction not found (HTTP 404) or other error

#### Example: Table Output (Default)

```bash
$ synapse transactions get 550e8400-e29b-41d4-a716-446655440000
ID	550e8400-e29b-41d4-a716-446655440000
Status	pending
Amount	100.00
Asset	USD

```

#### Example: JSON Output

```bash
$ synapse transactions get 550e8400-e29b-41d4-a716-446655440000 --format json
{
  "id": "550e8400-e29b-41d4-a716-446655440000",
  "stellar_account": "GABC1234567890123456789012345678901234567890123456789012",
  "amount": "100.00",
  "asset_code": "USD",
  "status": "pending",
  "created_at": "2024-01-15T10:00:00Z",
  "updated_at": "2024-01-15T10:00:00Z",
  "anchor_transaction_id": null,
  "callback_type": null,
  "callback_status": null,
  "settlement_id": null,
  "memo": null,
  "memo_type": null,
  "metadata": null
}
```

#### Example: Not-Found Error

```bash
$ synapse transactions get 00000000-0000-0000-0000-000000000000
transaction not found: Transaction 00000000 not found

$ echo $?
1
```

#### Example: With Env Vars

```bash
export SYNAPSE_BASE_URL="https://api.example.com"
export SYNAPSE_API_KEY="sk-test-123456"

synapse transactions get 550e8400-e29b-41d4-a716-446655440000
```

## Output Format Details

### Table Format

Displays transaction data in a human-readable table with key-value pairs:
```
ID      <id>
Status  <status>
Amount  <amount>
Asset   <asset_code>
```

### JSON Format

Outputs the full transaction object as pretty-printed JSON. Useful for piping to other tools:

```bash
synapse transactions get <id> --format json | jq '.status'
```

## Not-Found Handling

HTTP 404 responses are surfaced distinctly:
- Exit code: `1`
- Stderr message: `transaction not found: <error message>`
- This distinguishes "record doesn't exist" from network errors or server failures

## Testing

Run integration tests (requires mock server):

```bash
cargo test --test transactions_get_integration
```

Run all tests:

```bash
cargo test
```
