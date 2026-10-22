#!/usr/bin/env python3
"""Reference model of the swap-engine SLIs in docs/slo.md.

Mirrors the PromQL in observability/recording-rules.yml (group
synapse-core-slo) so SLI semantics can be unit-tested and burn-rate alerts
dry-run against synthetic request streams. Standard library only.
"""

MIN_VOLUME_RPS = 0.1  # matches synapse:slo_http_request_volume:rate1h > 0.1


def availability_error_ratio(requests):
    """requests: iterable of HTTP status codes. 4xx (tenant-caused) is excluded
    from the denominator; 5xx counts as bad."""
    counted = [s for s in requests if not 400 <= s < 500]
    if not counted:
        return 0.0
    return sum(1 for s in counted if s >= 500) / len(counted)


def latency_error_ratio(requests, threshold_ms=1000):
    """requests: iterable of (status, duration_ms). Slow = over threshold."""
    counted = [d for s, d in requests if not 400 <= s < 500]
    if not counted:
        return 0.0
    return sum(1 for d in counted if d > threshold_ms) / len(counted)


def burn_alert(ratio_long, ratio_short, rps, objective_budget, burn_rate):
    """True when both windows exceed burn_rate * budget and traffic is non-idle."""
    thr = burn_rate * objective_budget
    return ratio_long > thr and ratio_short > thr and rps > MIN_VOLUME_RPS
