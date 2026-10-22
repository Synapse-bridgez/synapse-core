#!/usr/bin/env python3
"""Tests for the swap-engine SLO rules (docs/slo.md)."""

import re
import sys
import unittest
from pathlib import Path

import yaml

sys.path.insert(0, str(Path(__file__).parent))
import slo_model as m  # noqa: E402

ROOT = Path(__file__).resolve().parent.parent
# Metrics emitted by the service (src/metrics.rs) that SLO rules may use.
EXISTING_METRICS = {"http_request_duration_ms_count", "http_request_duration_ms_bucket"}


def load(path):
    return yaml.safe_load((ROOT / path).read_text())


def rules(path):
    return [r for g in load(path)["groups"] for r in g["rules"]]


class SliComputation(unittest.TestCase):
    def test_tenant_errors_excluded_from_denominator(self):
        self.assertEqual(m.availability_error_ratio([200, 200, 400, 404, 429, 500]), 1 / 3)

    def test_idle_stream_is_not_an_error(self):
        self.assertEqual(m.availability_error_ratio([]), 0.0)
        self.assertEqual(m.latency_error_ratio([(400, 9999)]), 0.0)

    def test_latency_ratio(self):
        self.assertEqual(m.latency_error_ratio([(200, 100), (200, 1500), (200, 900), (404, 5000)]), 1 / 3)


class DryRun(unittest.TestCase):
    def test_fast_burn_fires_on_degradation_not_on_idle(self):
        degraded = [500] * 10 + [200] * 90  # 10% errors vs 0.072 threshold
        r = m.availability_error_ratio(degraded)
        self.assertTrue(m.burn_alert(r, r, rps=50, objective_budget=0.005, burn_rate=14.4))
        healthy = m.availability_error_ratio([200] * 999 + [500])
        self.assertFalse(m.burn_alert(healthy, healthy, 50, 0.005, 14.4))
        # idle system: a single failed request must not page
        one = m.availability_error_ratio([500])
        self.assertFalse(m.burn_alert(one, one, rps=0.001, objective_budget=0.005, burn_rate=14.4))


class RuleFiles(unittest.TestCase):
    def test_alerts_reference_only_defined_series(self):
        recorded = {r["record"] for r in rules("observability/recording-rules.yml") if r["record"].startswith("synapse:slo_")}
        slo_alerts = [r for r in rules("alerting/prometheus-rules.yml") if r.get("alert", "").startswith("SloBurn")]
        self.assertEqual(len(slo_alerts), 4)
        for a in slo_alerts:
            used = set(re.findall(r"synapse:[a-z0-9_]+:rate\w+", a["expr"]))
            self.assertTrue(used and used <= recorded, (a["alert"], used - recorded))
            self.assertIn("route", a["labels"])

    def test_recording_rules_use_only_existing_metrics(self):
        for r in rules("observability/recording-rules.yml"):
            if not r["record"].startswith("synapse:slo_"):
                continue
            used = set(re.findall(r"\b([a-z_]+_(?:count|bucket|total))\b", r["expr"]))
            self.assertTrue(used <= EXISTING_METRICS, (r["record"], used - EXISTING_METRICS))


if __name__ == "__main__":
    unittest.main()
