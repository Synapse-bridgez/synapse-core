#!/usr/bin/env python3
"""Compare raw and Thanos hourly downsampled scorecard data over their overlap."""

import argparse
import json
import os
import statistics
import sys
import time
import urllib.error
import urllib.parse
import urllib.request

DEFAULT_QUERY = "synapse:http_request_duration_ms:p95_rate5m"


def query_range(base_url, query, start, end, step, resolution, token):
    params = urllib.parse.urlencode(
        {
            "query": query,
            "start": start,
            "end": end,
            "step": step,
            "max_source_resolution": resolution,
        }
    )
    request = urllib.request.Request(f"{base_url.rstrip('/')}/api/v1/query_range?{params}")
    if token:
        request.add_header("Authorization", f"Bearer {token}")
    with urllib.request.urlopen(request, timeout=60) as response:
        body = json.load(response)
    if body.get("status") != "success":
        raise RuntimeError(f"Prometheus-compatible query failed: {body.get('error', 'unknown error')}")
    results = body.get("data", {}).get("result", [])
    if len(results) != 1:
        raise RuntimeError(f"expected one aggregated series for {query!r}, got {len(results)}")
    return [(int(float(ts)), float(value)) for ts, value in results[0].get("values", [])]


def compare_hourly_means(raw_points, downsampled_points):
    raw_by_hour = {}
    for timestamp, value in raw_points:
        raw_by_hour.setdefault(timestamp // 3600 * 3600, []).append(value)
    downsampled_by_hour = {timestamp // 3600 * 3600: value for timestamp, value in downsampled_points}
    common_hours = sorted(raw_by_hour.keys() & downsampled_by_hour.keys())
    comparisons = [
        (statistics.fmean(raw_by_hour[hour]), downsampled_by_hour[hour])
        for hour in common_hours
        if raw_by_hour[hour]
    ]
    return comparisons


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--url", default=os.environ.get("PROMETHEUS_URL"), required=os.environ.get("PROMETHEUS_URL") is None)
    parser.add_argument("--token", default=os.environ.get("PROMETHEUS_BEARER_TOKEN"))
    parser.add_argument("--query", default=DEFAULT_QUERY)
    parser.add_argument("--end-age-days", type=float, default=12.0, help="End the comparison this many days ago; choose 10-14 for raw/hourly overlap.")
    parser.add_argument("--hours", type=int, default=24)
    parser.add_argument("--minimum-hours", type=int, default=12)
    parser.add_argument("--max-relative-error", type=float, default=0.10)
    args = parser.parse_args(argv)
    if not 10 <= args.end_age_days < 14:
        parser.error("--end-age-days must be at least 10 and less than 14 to overlap hourly and raw tiers")
    if args.hours < args.minimum_hours or args.minimum_hours < 1:
        parser.error("--hours must be >= --minimum-hours >= 1")

    end = int(time.time() - args.end_age_days * 86400)
    start = end - args.hours * 3600
    try:
        raw = query_range(args.url, args.query, start, end, 300, "0", args.token)
        hourly = query_range(args.url, args.query, start, end, 3600, "1h", args.token)
        comparisons = compare_hourly_means(raw, hourly)
    except (OSError, urllib.error.URLError, json.JSONDecodeError, RuntimeError, ValueError) as error:
        print(f"Downsampling validation failed: {error}", file=sys.stderr)
        return 1

    if len(comparisons) < args.minimum_hours:
        print(
            f"Downsampling validation failed: only {len(comparisons)} overlapping hourly points; "
            f"need {args.minimum_hours}. Verify recording rules, block upload, and compaction.",
            file=sys.stderr,
        )
        return 1

    relative_errors = [
        abs(raw_value - downsampled_value) / max(abs(raw_value), 1e-9)
        for raw_value, downsampled_value in comparisons
    ]
    mean_error = statistics.fmean(relative_errors)
    max_error = max(relative_errors)
    if mean_error > args.max_relative_error:
        print(
            f"Downsampling validation failed: mean relative error {mean_error:.1%} "
            f"exceeds {args.max_relative_error:.1%} (max {max_error:.1%}, "
            f"{len(comparisons)} overlapping hours).",
            file=sys.stderr,
        )
        return 1

    print(
        f"Downsampling validation passed: {len(comparisons)} overlapping hourly points; "
        f"mean relative error {mean_error:.1%}, max {max_error:.1%}."
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())
