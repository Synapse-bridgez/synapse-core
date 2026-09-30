#!/usr/bin/env python3
"""Validate dependency-root alert grouping without a YAML package dependency."""

import json
import re
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent


def declared_health_edges():
    source = (ROOT / "src/health.rs").read_text()
    return set(
        re.findall(r'\("([a-z_]+)",\s*"([a-z_]+)",\s*(?:true|false)\)', source)
    )


def prometheus_alert_labels():
    text = (ROOT / "alerting/prometheus-rules.yml").read_text()
    alerts = {}
    for block in re.split(r"(?=^\s+- alert:)", text, flags=re.MULTILINE):
        match = re.search(r"^\s+- alert:\s*([A-Za-z0-9_]+)", block, re.MULTILINE)
        if not match:
            continue
        labels = re.search(
            r"^\s+labels:\s*\n(.*?)(?=^\s+annotations:)",
            block,
            re.MULTILINE | re.DOTALL,
        )
        root = re.search(r"^\s+root_cause:\s*([A-Za-z0-9_]+)\s*$", labels.group(1), re.MULTILINE) if labels else None
        alerts[match.group(1)] = {"root_cause": root.group(1) if root else None}
    return alerts


def alert_group_key(alert, root_route_keys, fallback_keys):
    labels = alert["labels"]
    root_cause = labels.get("root_cause")
    if root_cause:
        return ("root_cause",) + tuple(labels.get(key, "") for key in root_route_keys)
    return ("alert",) + tuple(labels.get(key, "") for key in fallback_keys)


class AlertGroupingTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.mapping = json.loads((ROOT / "alerting/dependency-alert-groups.json").read_text())
        cls.labels = prometheus_alert_labels()
        cls.routes = (ROOT / "alerting/alertmanager-grouping.yml").read_text()
        cls.template = (ROOT / "alerting/templates/grouped-alerts.tmpl").read_text()
        cls.edges = declared_health_edges()

    def test_grouped_alerts_map_to_declared_dependency_edge(self):
        for root, config in self.mapping["root_cause_groups"].items():
            with self.subTest(root_cause=root):
                self.assertIn((config["depends_from"], root), self.edges)
                for name in config["alerts"]:
                    self.assertEqual(self.labels[name]["root_cause"], root)

    def test_cascading_postgres_alerts_share_a_group(self):
        alerts = [
            {"labels": {"alertname": name, "root_cause": "postgres", "environment": "prod", "cluster": "a"}, "fired_at": 100}
            for name in self.mapping["root_cause_groups"]["postgres"]["alerts"]
        ]
        keys = {
            alert_group_key(alert, ("root_cause", "environment", "cluster"), ("alertname", "instance", "dependency", "environment"))
            for alert in alerts
        }
        self.assertEqual(len(keys), 1)
        self.assertEqual(
            len(alerts),
            len(self.mapping["root_cause_groups"]["postgres"]["alerts"]),
            "grouping must retain every symptom alert",
        )

    def test_simultaneous_unrelated_alerts_remain_separate(self):
        alerts = [
            {"labels": {"alertname": "DatabasePoolExhausted", "root_cause": "postgres", "environment": "prod", "cluster": "a"}, "fired_at": 100},
            {"labels": {"alertname": "RedisUnavailable", "root_cause": "redis", "environment": "prod", "cluster": "a"}, "fired_at": 100},
            {"labels": {"alertname": "HighErrorRate", "environment": "prod", "instance": "app-1"}, "fired_at": 100},
        ]
        keys = [
            alert_group_key(alert, ("root_cause", "environment", "cluster"), ("alertname", "instance", "dependency", "environment"))
            for alert in alerts
        ]
        self.assertEqual(len(set(keys)), 3)

    def test_routes_do_not_group_unclassified_alerts_by_time(self):
        self.assertIn('root_cause=~".+"', self.routes)
        self.assertIn("group_by: [root_cause, environment, cluster]", self.routes)
        self.assertIn("group_by: [alertname, instance, environment, dependency]", self.routes)
        self.assertNotIn("group_by: [environment]", self.routes)

    def test_notification_template_includes_all_grouped_alert_details(self):
        for detail in (".Alerts.Firing", ".Alerts.Resolved", ".Labels", ".Annotations", ".StartsAt", ".EndsAt"):
            with self.subTest(detail=detail):
                self.assertIn(detail, self.template)


if __name__ == "__main__":
    unittest.main()
