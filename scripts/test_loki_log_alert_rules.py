#!/usr/bin/env python3
"""Tests for sustained structured-log alert rules. Run with unittest."""

import re
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
RULES_FILE = ROOT / "alerting/loki-rules.yml"

FIXTURE_SEQUENCES = {
    "TelemetryErrorsSustained": {
        "message": "Telemetry error occurred",
        "fields": {"error_kind": "export", "error_count": 10, "threshold": 10},
        "steady_events_per_minute": 4,
    },
    "TelemetryWebhookOversizedPayloadBurst": {
        "message": "Telemetry webhook rejected: payload too large",
        "fields": {
            "reason": "payload_too_large",
            "endpoint": "/telemetry/webhook",
            "size": 90000,
            "max": 65536,
            "size_band": "up_to_2x_limit",
        },
        "steady_events_per_minute": 2,
    },
    "TelemetryWebhookTimestampSkewBurst": {
        "message": "Telemetry webhook rejected: timestamp outside allowed window",
        "fields": {
            "reason": "timestamp_outside_window",
            "endpoint": "/telemetry/webhook",
            "skew_direction": "past",
            "skew_seconds": 420,
            "skew_band": "up_to_2x_window",
            "max_skew_seconds": 300,
        },
        "steady_events_per_minute": 2,
    },
}


def parse_rules(text):
    rules = {}
    chunks = re.split(r"(?=^\s+- alert:)", text, flags=re.MULTILINE)
    for chunk in chunks:
        name = re.search(r"^\s+- alert:\s*([A-Za-z0-9_]+)", chunk, re.MULTILINE)
        if not name:
            continue
        expression = re.search(
            r"^\s+expr:\s*\|\s*\n(.*?)(?=^\s+for:)",
            chunk,
            re.MULTILINE | re.DOTALL,
        )
        if not expression:
            raise AssertionError(f"{name.group(1)} has no multiline expression")
        expr = expression.group(1)
        message = re.search(r'\|\s*message="([^"]+)"', expr)
        window = re.search(r"\[(\d+)m\]", expr)
        threshold = re.search(r"\)\s*>=\s*(\d+)", expr)
        pending = re.search(r"^\s+for:\s*(\d+)m\s*$", chunk, re.MULTILINE)
        grouping = re.search(r"sum by \(([^)]+)\)", expr)
        if not all((message, window, threshold, pending, grouping)):
            raise AssertionError(f"{name.group(1)} is missing a testable rule field")
        rules[name.group(1)] = {
            "message": message.group(1),
            "window_seconds": int(window.group(1)) * 60,
            "threshold": int(threshold.group(1)),
            "pending_seconds": int(pending.group(1)) * 60,
            "grouping": {field.strip() for field in grouping.group(1).split(",")},
            "extractions": set(re.findall(r'\b([A-Za-z_][A-Za-z0-9_]*)="fields\.[^"]+"', expr)),
            "expression": expr,
        }
    return rules


def fixture_logs(fixture, event_count, interval_seconds):
    logs = []
    for index in range(event_count):
        logs.append(
            {
                "timestamp": index * interval_seconds,
                "message": fixture["message"],
                **fixture["fields"],
            }
        )
    return logs


def rule_fires(rule, logs):
    end = max((log["timestamp"] for log in logs), default=0)
    end += rule["window_seconds"] + rule["pending_seconds"] + 60
    active_since = None
    for now in range(0, end + 1, 60):
        count = sum(
            log["message"] == rule["message"]
            and now - rule["window_seconds"] < log["timestamp"] <= now
            for log in logs
        )
        if count >= rule["threshold"]:
            if active_since is None:
                active_since = now
            if now - active_since >= rule["pending_seconds"]:
                return True
        else:
            active_since = None
    return False


class LokiLogAlertRuleTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.rules = parse_rules(RULES_FILE.read_text())

    def test_rules_cover_known_silent_failure_classes(self):
        self.assertEqual(set(self.rules), set(FIXTURE_SEQUENCES))
        for name, fixture in FIXTURE_SEQUENCES.items():
            with self.subTest(rule=name):
                rule = self.rules[name]
                self.assertEqual(rule["message"], fixture["message"])
                self.assertEqual(rule["window_seconds"], 5 * 60)
                self.assertEqual(rule["pending_seconds"], 6 * 60)
                self.assertIn("service", rule["grouping"])
                self.assertIn("environment", rule["grouping"])
                self.assertTrue(
                    (rule["grouping"] - {"service", "environment"})
                    <= rule["extractions"]
                )
                for field in fixture["fields"]:
                    if field in {"error_kind", "size_band", "skew_direction", "skew_band"}:
                        self.assertIn(field, rule["grouping"])

    def test_sustained_fixture_sequences_fire(self):
        for name, fixture in FIXTURE_SEQUENCES.items():
            with self.subTest(rule=name):
                rule = self.rules[name]
                count = fixture["steady_events_per_minute"] * 16
                interval = 60 // fixture["steady_events_per_minute"]
                logs = fixture_logs(fixture, count, interval)
                self.assertGreaterEqual(
                    fixture["steady_events_per_minute"] * 5,
                    rule["threshold"],
                )
                self.assertTrue(rule_fires(rule, logs))

    def test_short_over_threshold_bursts_do_not_fire(self):
        for name, fixture in FIXTURE_SEQUENCES.items():
            with self.subTest(rule=name):
                rule = self.rules[name]
                logs = fixture_logs(fixture, rule["threshold"], 0)
                logs.extend(
                    {
                        "timestamp": index,
                        "message": "Telemetry webhook event received",
                        "source": "upstream",
                    }
                    for index in range(30)
                )
                self.assertFalse(rule_fires(rule, logs))


if __name__ == "__main__":
    unittest.main()