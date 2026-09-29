#!/usr/bin/env python3
"""Tests for scripts/check-alert-runbook-links.py.

Run: python3 scripts/test_check_alert_runbook_links.py
"""

import importlib.util
import unittest
from pathlib import Path

SCRIPT = Path(__file__).resolve().parent / "check-alert-runbook-links.py"
spec = importlib.util.spec_from_file_location("check_links", SCRIPT)
check = importlib.util.module_from_spec(spec)
spec.loader.exec_module(check)

BASE = "https://example.test/runbook.md"
RUNBOOK = """# Runbook
## Monitoring & Alerting
```bash
# Not a heading
```
### 3. High Error Rate
### Tasks
### Tasks
"""


def mapping(alerts):
    return {"runbook": "runbook.md", "base_url": BASE, "alerts": alerts}


class SlugTests(unittest.TestCase):
    def test_slugify(self):
        self.assertEqual(check.slugify("Monitoring & Alerting"), "monitoring--alerting")
        self.assertEqual(
            check.slugify("Symptom: Pool usage consistently high (>80%)"),
            "symptom-pool-usage-consistently-high-80",
        )

    def test_anchors(self):
        anchors = check.heading_anchors(RUNBOOK)
        self.assertIn("3-high-error-rate", anchors)
        self.assertIn("tasks", anchors)
        self.assertIn("tasks-1", anchors)
        self.assertNotIn("not-a-heading", anchors)


class ValidateTests(unittest.TestCase):
    def test_valid(self):
        m = mapping({"HighErrorRate": {"anchor": "3-high-error-rate"}})
        rules = {"HighErrorRate": f"{BASE}#3-high-error-rate"}
        self.assertEqual(check.validate(m, RUNBOOK, rules, ["HighErrorRate"]), [])

    def test_renamed_heading_fails(self):
        m = mapping({"HighErrorRate": {"anchor": "3-high-error-rates"}})
        problems = check.validate(m, RUNBOOK, {}, [])
        self.assertEqual(len(problems), 1)
        self.assertIn("does not exist", problems[0])

    def test_unmapped_rule_and_in_process_alert_fail(self):
        problems = check.validate(mapping({}), RUNBOOK, {"NewRule": None}, ["NewAlert"])
        self.assertEqual(len(problems), 2)

    def test_stale_annotation_fails(self):
        m = mapping({"HighErrorRate": {"anchor": "3-high-error-rate"}})
        problems = check.validate(m, RUNBOOK, {"HighErrorRate": f"{BASE}#old"}, [])
        self.assertIn("expected", problems[0])

    def test_exemptions(self):
        good = {"exempt": {"reason": "why", "reviewed_by": "me", "reviewed_on": "2026-01-01"}}
        lazy = {"exempt": {"reason": "", "reviewed_by": "me"}}
        m = mapping({"W": good, "L": lazy, "Both": {**good, "anchor": "tasks"}})
        problems = check.validate(m, RUNBOOK, {"W": f"{BASE}#tasks"}, [])
        self.assertTrue(any("exactly one" in p for p in problems))
        self.assertTrue(any("non-empty reason" in p for p in problems))
        self.assertTrue(any("must not carry" in p for p in problems))

    def test_rule_parser(self):
        text = """
      - alert: A
        annotations:
          runbook_url: "https://x#a"
      - alert: B
        expr: vector(1)
"""
        self.assertEqual(check.prometheus_rules(text), {"A": "https://x#a", "B": None})

    def test_in_process_parser(self):
        src = 'pub const FOO: &str = "Foo";\npub const ALL: &[&str] = &[FOO];\n'
        self.assertEqual(check.in_process_alerts(src), ["Foo"])


class RepositoryTest(unittest.TestCase):
    """The real CI assertion: the checked-in mapping, rules and runbook agree."""

    def test_repository_is_consistent(self):
        self.assertEqual(check.main([]), 0)


if __name__ == "__main__":
    unittest.main()
