#!/usr/bin/env python3
"""Wave funding-allocation transparency dashboard.

Reconciles two sources and publishes the result as a markdown page:

  * points earned  - merged PRs attributed with scripts/contributor_recognition.py
  * disbursements  - payout records exported from the program's payout mechanism

Every mismatch between the two is listed as a discrepancy; nothing is dropped
silently. Reporting is aggregate by default. Per-contributor detail (including
login-level discrepancies) is shown only for logins in the opt-in file.

Disbursement export format (JSON Lines, one object per payout):

  {"pr": 1416, "contributor": "login", "amount": 200.0, "paid_at": "2026-10-03T12:00:00Z"}

Usage:
  python3 scripts/wave_funding_dashboard.py \
    --prs prs.json --issues issues.json --disbursements payouts.jsonl
"""

from __future__ import annotations

import argparse
import json
import statistics
import sys
from collections import defaultdict
from dataclasses import dataclass, field
from datetime import datetime, timezone
from pathlib import Path
from typing import Any, Dict, Iterable, List, Optional, Tuple

sys.path.insert(0, str(Path(__file__).resolve().parent))

from contributor_recognition import (  # noqa: E402
    ContributorRecord,
    _category_of,
    _complexity_of,
    _points_of,
    attribute_pr,
    load_records,
)

DEFAULT_OUTPUT = "docs/community/funding/dashboard.md"
DEFAULT_RECORDS = "docs/contributors/records.json"
UNCATEGORIZED = "uncategorized"


@dataclass
class IssueCredit:
    """Points credited for one closed issue within a merged PR."""

    number: int
    category: str
    complexity: str
    points: int


@dataclass
class Allocation:
    """Points earned by one merged PR."""

    pr: int
    contributor: str
    merged_at: Optional[datetime]
    points: int
    credits: List[IssueCredit] = field(default_factory=list)


@dataclass
class Payout:
    """One disbursement record from the payout mechanism."""

    pr: int
    contributor: str
    amount: float
    paid_at: Optional[datetime]


@dataclass
class Discrepancy:
    """A mismatch between points earned and funding disbursed."""

    pr: int
    contributor: str
    kind: str
    detail: str


def _parse_ts(value: Any) -> Optional[datetime]:
    if not value:
        return None
    try:
        parsed = datetime.fromisoformat(str(value).replace("Z", "+00:00"))
    except ValueError:
        return None
    if parsed.tzinfo is None:
        parsed = parsed.replace(tzinfo=timezone.utc)
    return parsed


def _period(ts: Optional[datetime]) -> str:
    return ts.strftime("%Y-%m") if ts else "unknown"


def build_allocations(
    prs: Iterable[Dict[str, Any]], issues_by_number: Dict[int, Dict[str, Any]]
) -> Tuple[List[Allocation], List[Discrepancy]]:
    """Attribute points per PR. PRs needing review are returned as flags."""
    allocations: List[Allocation] = []
    flagged: List[Discrepancy] = []
    for pr in prs:
        number = int(pr.get("number") or 0)
        attribution = attribute_pr(pr, issues_by_number)
        if attribution.needs_review or attribution.points <= 0:
            flagged.append(
                Discrepancy(
                    pr=number,
                    contributor=attribution.contributor,
                    kind="points unresolved",
                    detail="; ".join(attribution.reasons) or "no points attributed",
                )
            )
            continue

        credits = []
        for issue_number in attribution.issues:
            issue = issues_by_number[issue_number]
            complexity = _complexity_of(issue) or "unknown"
            credits.append(
                IssueCredit(
                    number=issue_number,
                    category=_category_of(issue) or UNCATEGORIZED,
                    complexity=complexity,
                    points=_points_of(issue, complexity) or 0,
                )
            )
        allocations.append(
            Allocation(
                pr=number,
                contributor=attribution.contributor,
                merged_at=_parse_ts(pr.get("merged_at") or pr.get("mergedAt")),
                points=attribution.points,
                credits=credits,
            )
        )
    return allocations, flagged


def load_payouts(path: Path) -> List[Payout]:
    payouts: List[Payout] = []
    for line in path.read_text(encoding="utf-8").splitlines():
        if not line.strip():
            continue
        row = json.loads(line)
        payouts.append(
            Payout(
                pr=int(row["pr"]),
                contributor=str(row["contributor"]),
                amount=float(row["amount"]),
                paid_at=_parse_ts(row.get("paid_at")),
            )
        )
    return payouts


def reconcile(
    allocations: List[Allocation],
    flagged: List[Discrepancy],
    payouts: List[Payout],
    records: Dict[str, ContributorRecord],
    rate: Optional[float] = None,
) -> List[Discrepancy]:
    """Cross-check points earned against disbursements and recognition records."""
    found: List[Discrepancy] = []
    alloc_by_pr = {a.pr: a for a in allocations}
    flagged_prs = {f.pr for f in flagged}
    payouts_by_pr: Dict[int, List[Payout]] = defaultdict(list)
    for payout in payouts:
        payouts_by_pr[payout.pr].append(payout)

    for item in flagged:
        if item.pr in payouts_by_pr:
            found.append(
                Discrepancy(item.pr, item.contributor, "paid without resolved points", item.detail)
            )

    for allocation in allocations:
        pr_payouts = payouts_by_pr.get(allocation.pr, [])
        if not pr_payouts:
            found.append(
                Discrepancy(
                    allocation.pr,
                    allocation.contributor,
                    "points earned, no disbursement",
                    f"{allocation.points} points not yet paid or missing from export",
                )
            )
            continue
        if len(pr_payouts) > 1:
            found.append(
                Discrepancy(
                    allocation.pr,
                    allocation.contributor,
                    "multiple disbursements",
                    f"{len(pr_payouts)} payout records for one PR",
                )
            )
        for payout in pr_payouts:
            if payout.contributor != allocation.contributor:
                found.append(
                    Discrepancy(
                        allocation.pr,
                        allocation.contributor,
                        "payee mismatch",
                        f"paid to {payout.contributor}, PR author is {allocation.contributor}",
                    )
                )
        if rate is not None:
            paid = sum(p.amount for p in pr_payouts)
            expected = allocation.points * rate
            if abs(paid - expected) > 1e-9:
                found.append(
                    Discrepancy(
                        allocation.pr,
                        allocation.contributor,
                        "amount mismatch",
                        f"paid {paid:g}, expected {expected:g} for {allocation.points} points",
                    )
                )

    for payout in payouts:
        if payout.pr not in alloc_by_pr and payout.pr not in flagged_prs:
            found.append(
                Discrepancy(
                    payout.pr,
                    payout.contributor,
                    "disbursement without earned points",
                    "no merged PR with a resolvable issue matches this payout",
                )
            )

    points_by_login: Dict[str, int] = defaultdict(int)
    for allocation in allocations:
        points_by_login[allocation.contributor] += allocation.points
    for login in sorted(set(points_by_login) | set(records)):
        recorded = records[login].points if login in records else 0
        computed = points_by_login.get(login, 0)
        if recorded != computed:
            found.append(
                Discrepancy(
                    0,
                    login,
                    "recognition records mismatch",
                    f"records.json has {recorded} points, merged PRs give {computed}",
                )
            )
    return found


def _median(values: List[float]) -> Optional[float]:
    return statistics.median(values) if values else None


def disbursement_latency_days(
    allocations: List[Allocation], payouts: List[Payout]
) -> List[float]:
    """Days from PR merge to payout, for payouts that match an allocation."""
    merged = {a.pr: a.merged_at for a in allocations}
    days = []
    for payout in payouts:
        merged_at = merged.get(payout.pr)
        if merged_at and payout.paid_at:
            days.append((payout.paid_at - merged_at).total_seconds() / 86400)
    return days


SENSITIVE_KINDS = {"amount mismatch", "payee mismatch", "recognition records mismatch"}


def _masked(login: str, opted_in: set) -> str:
    return login if login in opted_in else "_withheld_"


def _detail(item: Discrepancy, opted_in: set) -> str:
    """Hide payout and point figures for contributors who have not opted in."""
    if item.kind in SENSITIVE_KINDS and item.contributor not in opted_in:
        return "_withheld: contributor has not opted in_"
    return item.detail


def render_markdown(
    allocations: List[Allocation],
    payouts: List[Payout],
    discrepancies: List[Discrepancy],
    flagged: List[Discrepancy],
    opted_in: set,
) -> str:
    """Render the transparency dashboard. Aggregate by default."""
    points_by_period: Dict[str, int] = defaultdict(int)
    paid_by_period: Dict[str, float] = defaultdict(float)
    points_by_category: Dict[str, int] = defaultdict(int)
    points_by_complexity: Dict[str, int] = defaultdict(int)
    points_by_login: Dict[str, int] = defaultdict(int)
    prs_by_login: Dict[str, int] = defaultdict(int)

    for allocation in allocations:
        points_by_period[_period(allocation.merged_at)] += allocation.points
        points_by_login[allocation.contributor] += allocation.points
        prs_by_login[allocation.contributor] += 1
        for credit in allocation.credits:
            points_by_category[credit.category] += credit.points
            points_by_complexity[credit.complexity] += credit.points
    for payout in payouts:
        paid_by_period[_period(payout.paid_at)] += payout.amount

    total_points = sum(a.points for a in allocations)
    total_paid = sum(p.amount for p in payouts)
    latency = disbursement_latency_days(allocations, payouts)
    median_latency = _median(latency)
    max_latency = max(latency) if latency else None
    generated_at = datetime.now(timezone.utc).isoformat()

    lines = [
        "# Wave Funding Allocation Dashboard",
        "",
        "<!-- Generated by scripts/wave_funding_dashboard.py. Do not edit manually. -->",
        "",
        f"_Last updated: {generated_at}_",
        "",
        "Points earned come from merged Wave PRs (see the "
        "[contributor leaderboard](../../contributors/leaderboard.md)). Disbursements "
        "come from the payout export. Every mismatch between the two is listed below.",
        "",
        "## Summary",
        "",
        "| Metric | Value |",
        "| --- | --- |",
        f"| Merged PRs with points | {len(allocations)} |",
        f"| Total points earned | {total_points} |",
        f"| Total disbursed | {total_paid:g} |",
        f"| Payouts recorded | {len(payouts)} |",
        f"| Median merge-to-payout latency (days) | {_fmt(median_latency)} |",
        f"| Max merge-to-payout latency (days) | {_fmt(max_latency)} |",
        f"| Open discrepancies | {len(discrepancies)} |",
        f"| PRs flagged for review | {len(flagged)} |",
        "",
        "## Points and disbursement by period",
        "",
        "Points are grouped by merge month; disbursements by payout month.",
        "",
        "| Period | Points earned | Disbursed |",
        "| --- | --- | --- |",
    ]
    for period in sorted(set(points_by_period) | set(paid_by_period)):
        lines.append(
            f"| {period} | {points_by_period.get(period, 0)} | {paid_by_period.get(period, 0.0):g} |"
        )
    lines += ["", "## Breakdown by category", "", "| Category | Points |", "| --- | --- |"]
    lines += [f"| {k} | {v} |" for k, v in sorted(points_by_category.items())]
    lines += ["", "## Breakdown by complexity", "", "| Complexity | Points |", "| --- | --- |"]
    lines += [f"| {k} | {v} |" for k, v in sorted(points_by_complexity.items())]

    lines += [
        "",
        "## Contributors",
        "",
        "Only contributors who have opted in are listed by login. Everyone else is aggregated.",
        "",
        "| Contributor | Merged PRs | Points |",
        "| --- | --- | --- |",
    ]
    for login in sorted(points_by_login, key=lambda k: (-points_by_login[k], k.lower())):
        if login in opted_in:
            lines.append(f"| {login} | {prs_by_login[login]} | {points_by_login[login]} |")
    anonymous_prs = sum(n for login, n in prs_by_login.items() if login not in opted_in)
    anonymous_points = sum(p for login, p in points_by_login.items() if login not in opted_in)
    lines.append(f"| _other contributors_ | {anonymous_prs} | {anonymous_points} |")

    lines += [
        "",
        "## Discrepancies",
        "",
        "| PR | Contributor | Issue | Detail |",
        "| --- | --- | --- | --- |",
    ]
    if not discrepancies and not flagged:
        lines.append("| - | - | _None_ | - |")
    for item in discrepancies + flagged:
        pr = f"#{item.pr}" if item.pr else "-"
        lines.append(
            f"| {pr} | {_masked(item.contributor, opted_in)} | {item.kind} | {_detail(item, opted_in)} |"
        )
    lines.append("")
    return "\n".join(lines)


def _fmt(value: Optional[float]) -> str:
    return "-" if value is None else f"{value:.1f}"


def load_opt_in(path: Optional[Path]) -> set:
    if path is None or not path.exists():
        return set()
    return set(json.loads(path.read_text(encoding="utf-8")))


def main(argv: Optional[List[str]] = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--prs", required=True, help="JSON list of merged PR objects")
    parser.add_argument(
        "--issues",
        required=True,
        help="JSON mapping issue number -> issue metadata",
    )
    parser.add_argument("--disbursements", required=True, help="JSONL payout export")
    parser.add_argument("--records", default=DEFAULT_RECORDS, help="Recognition records file")
    parser.add_argument(
        "--opt-in",
        default=None,
        help="JSON list of logins that may appear with individual detail",
    )
    parser.add_argument(
        "--rate",
        type=float,
        default=None,
        help="Expected disbursement per point; enables amount checks",
    )
    parser.add_argument("--output", default=DEFAULT_OUTPUT, help="Dashboard markdown path")
    args = parser.parse_args(argv)

    prs = json.loads(Path(args.prs).read_text(encoding="utf-8"))
    raw_issues = json.loads(Path(args.issues).read_text(encoding="utf-8"))
    issues_by_number = {int(k): v for k, v in raw_issues.items()}

    allocations, flagged = build_allocations(prs, issues_by_number)
    payouts = load_payouts(Path(args.disbursements))
    records = load_records(Path(args.records))
    discrepancies = reconcile(allocations, flagged, payouts, records, args.rate)

    markdown = render_markdown(
        allocations,
        payouts,
        discrepancies,
        flagged,
        load_opt_in(Path(args.opt_in) if args.opt_in else None),
    )
    output = Path(args.output)
    output.parent.mkdir(parents=True, exist_ok=True)
    output.write_text(markdown, encoding="utf-8")

    print(f"Wrote {output}: {len(allocations)} PRs, {len(discrepancies)} discrepancies, {len(flagged)} flagged")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
