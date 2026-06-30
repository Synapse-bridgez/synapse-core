# Synapse CLI

Command-line interface for managing transactions and settlements in Synapse.

## Installation

Build the CLI:

```bash
cargo build --release
```

The binary will be available at `target/release/synapse`.

## Configuration

Set the Synapse API URL and optional API key via environment variables:

```bash
export SYNAPSE_URL=http://localhost:3000
export SYNAPSE_API_KEY=your-api-key
```

Or pass them as command-line flags:

```bash
synapse --url http://localhost:3000 --api-key your-api-key [COMMAND]
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

### Admin Locks

#### List Active Locks

List active distributed locks currently held by the Synapse instance.

```bash
synapse admin locks list [--json]
```

**Required flags:** none

**Optional flags:**
- `--json`: Print the raw API response as pretty JSON instead of the default table.

**Mock server example:**

Start the mock server in one terminal:

```bash
cd cli/synapse-cli
MOCK_SERVER_ADDR=127.0.0.1:4010 MOCK_SERVER_SCENARIO=happy cargo run --bin mock-server
```

Then run the CLI against it:

```bash
cargo run --bin synapse -- --base-url http://127.0.0.1:4010 admin locks list
```

Sample output:

```text
Active locks: 2 total (1 overdue)
Resource | Token | Acquired At | TTL | Expected Duration | Overdue
-------- | ----- | ----------- | --- | ----------------- | -------
settlement:550e8400-e29b-41d4-a716-446655440000 | 4e4e9e47-7e0f-4f2f-8d63-323c61279209 | 1782540612 | 30 | 30 | no
payout-batch:daily | 89ca5ddc-51bd-44bd-817e-f4175dcab0bc | 1782540400 | 30 | 30 | yes
```

### Transactions

#### Export Transactions

Export transactions to CSV or JSON format with optional filters. The export streams raw data without parsing or modification.

```bash
synapse transactions export [OPTIONS]
```

**Options (all optional):**
- `--format <FORMAT>`: Export format - `csv` (default) or `json`
  - CSV: Raw comma-separated values with headers, suitable for spreadsheet import
  - JSON: Wrapped in a JSON object, each row as a JSON object with metadata
- `--from <FROM>`: Start date filter (inclusive, YYYY-MM-DD format)
- `--to <TO>`: End date filter (inclusive, YYYY-MM-DD format)
- `--status <STATUS>`: Filter by transaction status (e.g., `pending`, `completed`, `failed`, `cancelled`)
- `--asset-code <ASSET_CODE>`: Filter by asset code (e.g., `USD`, `EUR`, `USDC`, `BRL`)
- `--output <OUTPUT>`: Save to file instead of stdout

**Output Format:**

CSV format (default):
```
id,stellar_account,amount,asset_code,status,created_at,updated_at,anchor_transaction_id,callback_type,callback_status
550e8400-e29b-41d4-a716-446655440000,GAAA...,100.00,USD,completed,2024-01-15T10:30:00Z,2024-01-15T11:00:00Z,,send,completed
550e8401-e29b-41d4-a716-446655440001,GBBB...,250.50,EUR,pending,2024-01-15T11:30:00Z,2024-01-15T11:30:00Z,,receive,pending
```

JSON format:
```json
{
  "data": [
    {
      "id": "550e8400-e29b-41d4-a716-446655440000",
      "stellar_account": "GAAA...",
      "amount": "100.00",
      "asset_code": "USD",
      "status": "completed",
      "created_at": "2024-01-15T10:30:00Z",
      "updated_at": "2024-01-15T11:00:00Z"
    }
  ]
}
```

**Examples:**

Export all transactions as CSV to stdout:
```bash
synapse transactions export
```

Export pending USD transactions as JSON:
```bash
synapse transactions export --format json --status pending --asset-code USD
```

Export transactions from January 2024 as CSV:
```bash
synapse transactions export --from 2024-01-01 --to 2024-01-31
```

Export completed EUR transactions to a file:
```bash
synapse transactions export --status completed --asset-code EUR --output completed_eur.csv
```

Export all EUR and USD transactions in the last 30 days (requires two commands):
```bash
synapse transactions export --asset-code USD --from 2024-01-01 > usd_export.csv
synapse transactions export --asset-code EUR --from 2024-01-01 > eur_export.csv
```

**Notes:**
- The export endpoint streams raw data without intermediate parsing
- Large exports are streamed efficiently without loading entire dataset into memory
- Date filters are inclusive on both ends (from date and to date both included)
- Empty filter results still return valid CSV/JSON with headers (CSV) or empty data array (JSON)
- File output is useful for large exports that may not fit in terminal output

### Settlements

#### List Settlements

List settlements with cursor-based pagination. Settlements are ordered by creation date, most recent first (forward) or oldest first (backward).

```bash
synapse settlements list [OPTIONS]
```

**Options (all optional):**
- `--cursor <CURSOR>`: Pagination cursor from a previous response. Cursors are opaque - always use the value from `next_cursor` in the API response.
- `--limit <LIMIT>`: Results per page (1-100, default: 10). Larger limits retrieve more data in fewer requests.
- `--direction <DIRECTION>`: Order direction - `forward` (default, newest first) or `backward` (oldest first)
- `--format <FORMAT>`: Output format - `table` (default, human-readable) or `json` (complete JSON)

**Sample Table Output:**
```
id: 550e8400-e29b-41d4-a716-446655440000 | status: completed | amount: 1500.00 | asset_code: USD | created_at: 2024-01-15T10:30:00Z
550e8401-e29b-41d4-a716-446655440001 | status: pending | amount: 2500.50 | asset_code: EUR | created_at: 2024-01-15T09:15:00Z
550e8402-e29b-41d4-a716-446655440002 | status: failed | amount: 500.00 | asset_code: GBP | created_at: 2024-01-14T23:45:00Z
```

**Sample JSON Output:**
```json
{
  "settlements": [
    {
      "id": "550e8400-e29b-41d4-a716-446655440000",
      "status": "completed",
      "amount": "1500.00",
      "asset_code": "USD",
      "created_at": "2024-01-15T10:30:00Z",
      "updated_at": "2024-01-15T11:00:00Z"
    },
    {
      "id": "550e8401-e29b-41d4-a716-446655440001",
      "status": "pending",
      "amount": "2500.50",
      "asset_code": "EUR",
      "created_at": "2024-01-15T09:15:00Z",
      "updated_at": "2024-01-15T09:15:00Z"
    }
  ],
  "next_cursor": "eyJpZCI6IjU1MGU4NDAyLWUyOWItNDFkNC1hNzE2LTQ0NjY1NTQ0MDAwMiIsImNyZWF0ZWRfYXQiOiIyMDI0LTAxLTE0VDIzOjQ1OjAwWiJ9",
  "has_more": true
}
```

**Examples:**

List first 10 settlements (default):
```bash
synapse settlements list
```

List 50 most recent settlements in JSON:
```bash
synapse settlements list --limit 50 --format json
```

List settlements in reverse chronological order (oldest first):
```bash
synapse settlements list --direction backward --limit 25
```

Navigate to next page using cursor from previous response:
```bash
synapse settlements list --cursor <cursor-from-previous-response> --limit 10
```

#### Get Settlement

Get detailed information about a specific settlement by UUID.

```bash
synapse settlements get <SETTLEMENT_ID> [OPTIONS]
```

**Arguments (required):**
- `SETTLEMENT_ID`: The settlement UUID (format: `xxxxxxxx-xxxx-xxxx-xxxx-xxxxxxxxxxxx`)

**Options (optional):**
- `--format <FORMAT>`: Output format - `table` (default, key-value pairs) or `json` (complete JSON)

**Sample Table Output:**
```
id: 550e8400-e29b-41d4-a716-446655440000
status: completed
amount: 1500.00
asset_code: USD
counterparty_account: GABC...
created_at: 2024-01-15T10:30:00Z
updated_at: 2024-01-15T11:00:00Z
memo: Settlement for invoice #12345
```

**Sample JSON Output:**
```json
{
  "id": "550e8400-e29b-41d4-a716-446655440000",
  "status": "completed",
  "amount": "1500.00",
  "asset_code": "USD",
  "counterparty_account": "GABC...",
  "created_at": "2024-01-15T10:30:00Z",
  "updated_at": "2024-01-15T11:00:00Z",
  "memo": "Settlement for invoice #12345"
}
```

**Examples:**

Get settlement details in human-readable format:
```bash
synapse settlements get 550e8400-e29b-41d4-a716-446655440000
```

Get settlement details in JSON (useful for scripting):
```bash
synapse settlements get 550e8400-e29b-41d4-a716-446655440000 --format json
```

Combine with jq for selective JSON fields:
```bash
synapse settlements get 550e8400-e29b-41d4-a716-446655440000 --format json | jq '.status, .amount, .asset_code'
```

## Output Formats

### Table Format (default)
Human-readable output with columns for lists and key-value pairs for objects.

### JSON Format
Full JSON output with all fields, useful for scripting and integration.

## Testing

Run tests:

```bash
cargo test
```

Tests requiring external services are marked with `#[ignore]` and can be run with:

```bash
cargo test -- --ignored
```

## Troubleshooting

### Connection Refused
Ensure the Synapse API server is running and the `--url` or `SYNAPSE_URL` environment variable is correctly set.

### Invalid UUID
Settlement IDs must be valid UUIDs (format: `xxxxxxxx-xxxx-xxxx-xxxx-xxxxxxxxxxxx`).

### Empty Results
When exporting transactions or listing settlements returns no results:
- Verify filter parameters are correct
- Check date ranges (use YYYY-MM-DD format)
- Confirm the asset code or status value exists
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

## Settlement Example

In one terminal, start the mock API:

```powershell
cargo run --manifest-path cli/synapse-cli/Cargo.toml --bin mock-server
```

Then update a settlement status against it and print the resulting settlement:

```powershell
cargo run --manifest-path cli/synapse-cli/Cargo.toml -- `
  --base-url http://127.0.0.1:4010 `
  admin settlements update-status `
  8f9b0f0c-9a89-4d1f-9d7d-0c7d7d0d9a11 `
  --status adjusted `
  --reason "Audit correction" `
  --new-total 125.0000000
```

Sample output:

```text
Settlement updated successfully

Settlement ID: 8f9b0f0c-9a89-4d1f-9d7d-0c7d7d0d9a11
Asset code: USDC
Status: adjusted
Total amount: 125.0000000
Tx count: 8
Period: 2026-06-26T00:00:00Z to 2026-06-27T00:00:00Z
Dispute reason: Audit correction
Original total amount: 130.0000000
Reviewed by: admin
Reviewed at: 2026-06-27T09:15:00Z
Created at: 2026-06-27T09:00:00Z
Updated at: 2026-06-27T09:15:00Z
```
