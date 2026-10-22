# Wave Funding Allocation Dashboard

The dashboard reports how Wave funding is allocated and disbursed across
completed issues: points earned per period, funding disbursed per period,
breakdown by category and complexity, and merge-to-payout latency.

Generated page: [`dashboard.md`](dashboard.md) (produced by
[`scripts/wave_funding_dashboard.py`](../../../scripts/wave_funding_dashboard.py)).

## Data sources

The report reconciles two sources and flags every mismatch between them.

| Source | What it is | Produced by |
| --- | --- | --- |
| Points earned | Merged PRs, their linked issues, and each issue's complexity and category labels | `scripts/contributor_recognition.py` (issue 108/120 tracking) |
| Disbursements | One JSON Lines record per payout, exported from the payout mechanism | The program's payout tooling (not re-entered by hand) |

Disbursement record format:

```json
{"pr": 1416, "contributor": "login", "amount": 200.0, "paid_at": "2026-10-03T12:00:00Z"}
```

## Regenerating the dashboard

```bash
python3 scripts/wave_funding_dashboard.py \
  --prs prs.json \
  --issues issues.json \
  --disbursements payouts.jsonl \
  --opt-in docs/community/funding/opt-in.json \
  --rate 1.0
```

- `--prs`: JSON list of merged PR objects (same shape the recognition script reads).
- `--issues`: JSON map of issue number to issue metadata.
- `--rate`: expected amount paid per point. Omit it to skip amount checks.
- `--opt-in`: JSON list of logins allowed to appear with individual detail.

## Discrepancies

Each of these is listed under *Discrepancies* and never dropped:

- **points unresolved**: a merged PR has a missing or ambiguous complexity label.
- **paid without resolved points**: a payout exists for a PR that has no resolved points.
- **points earned, no disbursement**: points are recorded but nothing has been paid yet, or the export is missing the payout.
- **multiple disbursements**: more than one payout record exists for the same PR.
- **payee mismatch**: the payout went to someone other than the PR author.
- **amount mismatch**: the paid amount differs from `points × --rate`.
- **disbursement without earned points**: a payout matches no merged PR.
- **recognition records mismatch**: the per-contributor total in `docs/contributors/records.json` differs from the total computed from merged PRs.

## Privacy

- Reporting is aggregate by default: totals by period, category, and complexity.
- A contributor's login and per-contributor totals appear only if they are in the opt-in list. Otherwise they show as `_withheld_` or fall into `_other contributors_`.
- Payout and point figures tied to a specific contributor (amount mismatches, payee mismatches, records mismatches) are withheld in the discrepancy list unless that contributor has opted in.
