#!/usr/bin/env python3
"""Contributor recognition system tied to merged Wave PRs.

Parses merged PR metadata (linked issues + complexity/point labels) and
maintains a per-contributor cumulative record: points earned, categories
contributed to, and streaks. The record is published as a generated,
auto-updating leaderboard file.

Out of scope: any monetary payout mechanism (recognition/tracking only).
"""

from __future__ import annotations

import argparse
import json
import re
from dataclasses import dataclass, field
from datetime import datetime, timezone
from pathlib import Path
from typing import Any, Dict, Iterable, List, Optional

# Complexity label -> point value. Mirrors the Wave issue point scheme.
COMPLEXITY_POINTS: Dict[str, int] = {
    "low": 50,
    "medium": 100,
    "high": 200,
}

# Matches "Complexity: High (200 points)" style lines in issue bodies.
COMPLEXITY_BODY_RE = re.compile(
    r"complexity\s*[:\-]\s*(low|medium|high)\b", re.IGNORECASE
)
POINTS_BODY_RE = re.compile(r"\((\d+)\s*points?\)", re.IGNORECASE)

# Matches closing keywords: "Closes #12", "fixes #34", "resolved #56".
CLOSES_RE = re.compile(
    r"\b(?:close[sd]?|fix(?:e[sd])?|resolve[sd]?)\s*:?\s*#(\d+)",
    re.IGNORECASE,
)


@dataclass
class Attribution:
    """Result of attributing points for a single merged PR."""

    contributor: str
    points: int = 0
    categories: List[str] = field(default_factory=list)
    issues: List[int] = field(default_factory=list)
    needs_review: bool = False
    reasons: List[str] = field(default_factory=list)


@dataclass
class ContributorRecord:
    """Cumulative per-contributor record."""

    login: str
    points: int = 0
    categories: Dict[str, int] = field(default_factory=dict)
    merged_prs: int = 0
    streak: int = 0
    last_merged_at: Optional[str] = None

    def to_dict(self) -> Dict[str, Any]:
        return {
            "login": self.login,
            "points": self.points,
            "categories": dict(sorted(self.categories.items())),
            "merged_prs": self.merged_prs,
            "streak": self.streak,
            "last_merged_at": self.last_merged_at,
        }


def _labels_of(issue: Dict[str, Any]) -> List[str]:
    labels = issue.get("labels") or []
    out: List[str] = []
    for label in labels:
        if isinstance(label, dict):
            name = label.get("name")
        else:
            name = label
        if name:
            out.append(str(name))
    return out


def _category_of(issue: Dict[str, Any]) -> Optional[str]:
    """Extract the Wave category from labels like 'category: Governance'."""
    for label in _labels_of(issue):
        if label.lower().startswith("category:"):
            return label.split(":", 1)[1].strip()
    return None


def _complexity_of(issue: Dict[str, Any]) -> Optional[str]:
    """Resolve complexity from labels first, then the issue body."""
    for label in _labels_of(issue):
        low = label.lower()
        if low.startswith("complexity:"):
            value = low.split(":", 1)[1].strip()
            if value in COMPLEXITY_POINTS:
                return value
        if low in COMPLEXITY_POINTS:
            return low
    body = issue.get("body") or ""
    match = COMPLEXITY_BODY_RE.search(body)
    if match:
        return match.group(1).lower()
    return None


def _points_of(issue: Dict[str, Any], complexity: Optional[str]) -> Optional[int]:
    """Resolve point value from an explicit '(N points)' hint or complexity."""
    body = issue.get("body") or ""
    match = POINTS_BODY_RE.search(body)
    if match:
        return int(match.group(1))
    if complexity in COMPLEXITY_POINTS:
        return COMPLEXITY_POINTS[complexity]
    return None


def linked_issues(pr: Dict[str, Any]) -> List[int]:
    """Return issue numbers closed by a PR (handles multiple issues)."""
    text = "\n".join(
        str(pr.get(key) or "") for key in ("title", "body")
    )
    seen: List[int] = []
    for number in CLOSES_RE.findall(text):
        value = int(number)
        if value not in seen:
            seen.append(value)
    return seen


def attribute_pr(
    pr: Dict[str, Any], issues_by_number: Dict[int, Dict[str, Any]]
) -> Attribution:
    """Attribute points for a merged PR across all linked issues.

    Ambiguous or missing complexity labels are flagged for manual review
    rather than silently mis-attributing points.
    """
    contributor = (pr.get("user") or {}).get("login") or pr.get("author") or "unknown"
    result = Attribution(contributor=str(contributor))

    numbers = linked_issues(pr)
    if not numbers:
        result.needs_review = True
        result.reasons.append("no linked issue found")
        return result

    for number in numbers:
        issue = issues_by_number.get(number)
        if issue is None:
            result.needs_review = True
            result.reasons.append(f"issue #{number} metadata unavailable")
            continue

        complexity = _complexity_of(issue)
        points = _points_of(issue, complexity)
        if points is None:
            result.needs_review = True
            result.reasons.append(f"issue #{number} has ambiguous/missing complexity")
            continue

        result.points += points
        result.issues.append(number)
        category = _category_of(issue)
        if category:
            result.categories.append(category)

    return result


def _week_key(timestamp: Optional[str]) -> Optional[str]:
    if not timestamp:
        return None
    try:
        parsed = datetime.fromisoformat(timestamp.replace("Z", "+00:00"))
    except ValueError:
        return None
    iso = parsed.isocalendar()
    return f"{iso[0]}-W{iso[1]:02d}"


def update_records(
    records: Dict[str, ContributorRecord],
    pr: Dict[str, Any],
    issues_by_number: Dict[int, Dict[str, Any]],
) -> Attribution:
    """Apply a merged PR to the cumulative record set in place."""
    attribution = attribute_pr(pr, issues_by_number)
    if attribution.needs_review or attribution.points <= 0:
        return attribution

    login = attribution.contributor
    record = records.get(login) or ContributorRecord(login=login)

    merged_at = pr.get("merged_at") or pr.get("mergedAt")
    week = _week_key(merged_at)
    previous_week = _week_key(record.last_merged_at)
    if week and previous_week and week != previous_week:
        record.streak += 1
    elif week and not previous_week:
        record.streak = 1

    record.points += attribution.points
    record.merged_prs += 1
    for category in attribution.categories:
        record.categories[category] = record.categories.get(category, 0) + 1
    if merged_at:
        record.last_merged_at = merged_at

    records[login] = record
    return attribution


def build_leaderboard(records: Iterable[ContributorRecord]) -> Dict[str, Any]:
    """Build the generated leaderboard payload."""
    ordered = sorted(
        records, key=lambda r: (-r.points, -r.merged_prs, r.login.lower())
    )
    return {
        "generated_at": datetime.now(timezone.utc).isoformat(),
        "contributors": [record.to_dict() for record in ordered],
    }


def render_markdown(leaderboard: Dict[str, Any]) -> str:
    """Render the leaderboard as a human-readable markdown page."""
    lines = [
        "# Wave Contributor Recognition",
        "",
        "<!-- Generated by scripts/contributor_recognition.py. Do not edit manually. -->",
        "",
        f"_Last updated: {leaderboard['generated_at']}_",
        "",
        "| Rank | Contributor | Points | Merged PRs | Streak | Categories |",
        "| ---- | ----------- | ------ | ---------- | ------ | ---------- |",
    ]
    contributors = leaderboard.get("contributors", [])
    if not contributors:
        lines.append("| - | _No merged Wave PRs yet_ | - | - | - | - |")
    for index, entry in enumerate(contributors, start=1):
        categories = ", ".join(entry.get("categories", {}).keys()) or "-"
        lines.append(
            "| {rank} | {login} | {points} | {prs} | {streak} | {categories} |".format(
                rank=index,
                login=entry.get("login", "unknown"),
                points=entry.get("points", 0),
                prs=entry.get("merged_prs", 0),
                streak=entry.get("streak", 0),
                categories=categories,
            )
        )
    lines.append("")
    return "\n".join(lines)


def load_records(path: Path) -> Dict[str, ContributorRecord]:
    if not path.exists():
        return {}
    data = json.loads(path.read_text(encoding="utf-8"))
    records: Dict[str, ContributorRecord] = {}
    for entry in data.get("contributors", []):
        login = entry.get("login")
        if not login:
            continue
        records[login] = ContributorRecord(
            login=login,
            points=int(entry.get("points", 0)),
            categories=dict(entry.get("categories", {})),
            merged_prs=int(entry.get("merged_prs", 0)),
            streak=int(entry.get("streak", 0)),
            last_merged_at=entry.get("last_merged_at"),
        )
    return records


def main(argv: Optional[List[str]] = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--pr", required=True, help="Path to merged PR JSON")
    parser.add_argument(
        "--issues",
        required=True,
        help="Path to JSON mapping issue number -> issue metadata",
    )
    parser.add_argument(
        "--records",
        default="docs/contributors/records.json",
        help="Path to the cumulative records file",
    )
    parser.add_argument(
        "--leaderboard",
        default="docs/contributors/LEADERBOARD.md",
        help="Path to the generated leaderboard page",
    )
    args = parser.parse_args(argv)

    pr = json.loads(Path(args.pr).read_text(encoding="utf-8"))
    raw_issues = json.loads(Path(args.issues).read_text(encoding="utf-8"))
    issues_by_number = {int(k): v for k, v in raw_issues.items()}

    records_path = Path(args.records)
    records = load_records(records_path)
    attribution = update_records(records, pr, issues_by_number)

    leaderboard = build_leaderboard(records.values())
    records_path.parent.mkdir(parents=True, exist_ok=True)
    records_path.write_text(
        json.dumps(leaderboard, indent=2, sort_keys=True) + "\n", encoding="utf-8"
    )
    Path(args.leaderboard).write_text(render_markdown(leaderboard), encoding="utf-8")

    if attribution.needs_review:
        print("NEEDS_REVIEW: " + "; ".join(attribution.reasons))
        return 0
    print(f"Attributed {attribution.points} points to {attribution.contributor}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
