#!/usr/bin/env python3
"""Normalize a cloud billing export into Prometheus metrics.

The script intentionally supports a small superset of CSV fields used by AWS,
Azure, and GCP exports. It reads rows like:

date,service,component,category,usage,unit,cost
2026-09-01,postgres,database,compute,42,GB-Hours,14.25

and emits metrics in Prometheus exposition format to stdout.
"""

from __future__ import annotations

import argparse
import csv
import sys
from collections import defaultdict
from typing import Dict, Iterable, List, Tuple


DEFAULT_FIELD_ALIASES = {
    "date": ("date", "timestamp", "usage_date"),
    "service": ("service", "service_name", "resource_name", "resource"),
    "component": ("component", "component_name", "family"),
    "category": ("category", "kind", "tier"),
    "usage": ("usage", "usage_amount", "quantity"),
    "unit": ("unit", "usage_unit", "measurement_unit"),
    "cost": ("cost", "amount", "total_cost", "total_amount", "bill"),
}


def _resolve_value(row: Dict[str, str], candidates: Iterable[str]) -> str:
    for candidate in candidates:
        if candidate in row and row[candidate] not in (None, ""):
            return str(row[candidate]).strip()
    return ""


def _normalize_component(value: str) -> str:
    value = (value or "unknown").lower().strip()
    mapping = {
        "postgres": "database",
        "redis": "cache",
        "compute": "compute",
        "storage": "storage",
        "database": "database",
        "cache": "cache",
    }
    return mapping.get(value, value)


def _normalize_category(value: str) -> str:
    value = (value or "unknown").lower().strip()
    mapping = {
        "db": "database",
        "cache": "cache",
        "storage": "storage",
        "compute": "compute",
    }
    return mapping.get(value, value)


def _normalize_usage(value: str) -> float:
    try:
        return float(value)
    except (TypeError, ValueError):
        return 0.0


def _read_rows(path: str) -> List[Dict[str, str]]:
    with open(path, newline="", encoding="utf-8") as handle:
        reader = csv.DictReader(handle)
        if not reader.fieldnames:
            raise ValueError(f"{path} does not contain a CSV header")
        rows: List[Dict[str, str]] = []
        for row in reader:
            rows.append({key.strip(): value.strip() if isinstance(value, str) else value for key, value in row.items()})
        return rows


def _normalize_rows(rows: List[Dict[str, str]]) -> List[Tuple[str, str, str, float, float]]:
    normalized: List[Tuple[str, str, str, float, float]] = []
    for row in rows:
        component = _normalize_component(_resolve_value(row, DEFAULT_FIELD_ALIASES["component"]))
        category = _normalize_category(_resolve_value(row, DEFAULT_FIELD_ALIASES["category"]))
        service = _resolve_value(row, DEFAULT_FIELD_ALIASES["service"]) or component
        usage = _normalize_usage(_resolve_value(row, DEFAULT_FIELD_ALIASES["usage"]))
        unit = _resolve_value(row, DEFAULT_FIELD_ALIASES["unit"]) or "usd"
        cost = _normalize_usage(_resolve_value(row, DEFAULT_FIELD_ALIASES["cost"]))
        if component == "unknown" and service:
            component = _normalize_component(service)
        normalized.append((component, category, unit, usage, cost))
    return normalized


def _group_costs(rows: Iterable[Tuple[str, str, str, float, float]]) -> Dict[Tuple[str, str, str], float]:
    grouped: Dict[Tuple[str, str, str], float] = defaultdict(float)
    for component, category, unit, _, cost in rows:
        grouped[(component, category, unit)] += cost
    return grouped


def _group_usage(rows: Iterable[Tuple[str, str, str, float, float]]) -> Dict[Tuple[str, str, str], float]:
    grouped: Dict[Tuple[str, str, str], float] = defaultdict(float)
    for component, category, unit, usage, _ in rows:
        grouped[(component, category, unit)] += usage
    return grouped


def main() -> int:
    parser = argparse.ArgumentParser(description="Normalize provider billing export to Prometheus metrics")
    parser.add_argument("--input", required=True, help="CSV file to ingest")
    parser.add_argument("--output", help="Optional destination file; defaults to stdout")
    args = parser.parse_args()

    try:
        normalized = _normalize_rows(_read_rows(args.input))
    except FileNotFoundError:
        print(f"billing export input not found: {args.input}", file=sys.stderr)
        return 2
    except ValueError as exc:
        print(str(exc), file=sys.stderr)
        return 2

    cost_metrics = _group_costs(normalized)
    usage_metrics = _group_usage(normalized)

    lines = [
        "# HELP cloud_cost_total Total infrastructure spend by component, category, and unit.",
        "# TYPE cloud_cost_total gauge",
        "# HELP cloud_usage_total Aggregate usage by component, category, and unit.",
        "# TYPE cloud_usage_total gauge",
    ]

    for (component, category, unit), total in sorted(cost_metrics.items()):
        lines.append(f'cloud_cost_total{{component="{component}",category="{category}",unit="{unit}"}} {total:.6f}')

    for (component, category, unit), total in sorted(usage_metrics.items()):
        lines.append(f'cloud_usage_total{{component="{component}",category="{category}",unit="{unit}"}} {total:.6f}')

    output = "\n".join(lines) + "\n"
    if args.output:
        with open(args.output, "w", encoding="utf-8") as handle:
            handle.write(output)
    else:
        sys.stdout.write(output)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
