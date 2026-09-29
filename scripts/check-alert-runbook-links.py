#!/usr/bin/env python3
"""Validate alert -> runbook links.

Reads alerting/runbook-links.json (the single source of truth, also embedded
into the service binary by src/alerting/runbook.rs) and fails if:

  * a mapped anchor does not exist as a heading in the runbook (e.g. the
    heading was renamed or removed),
  * an entry has both or neither of `anchor` / `exempt`,
  * an exemption is missing its reason or reviewer,
  * an alert defined in alerting/prometheus-rules.yml or in the in-process
    catalog src/alerting/names.rs has no mapping entry,
  * a Prometheus rule's `runbook_url` annotation disagrees with the mapping,
    or an exempt rule carries one.

Standard library only, so the CI job needs no dependencies.

Usage: scripts/check-alert-runbook-links.py [--root DIR]
"""

import argparse
import json
import re
import sys
from pathlib import Path


def slugify(heading):
    """GitHub-style anchor slug. Mirrors `slugify` in src/alerting/runbook.rs."""
    out = []
    for c in heading.strip().lower():
        if c == " ":
            out.append("-")
        elif c in "-_" or c.isalnum():
            out.append(c)
    return "".join(out)


def heading_anchors(markdown):
    anchors = set()
    seen = {}
    in_fence = False
    for line in markdown.splitlines():
        stripped = line.lstrip()
        if stripped.startswith("```") or stripped.startswith("~~~"):
            in_fence = not in_fence
            continue
        if in_fence:
            continue
        m = re.match(r"^(#{1,6}) (.*)$", stripped)
        if not m:
            continue
        text = m.group(2).strip().rstrip("#").strip()
        base = slugify(text)
        count = seen.get(base, 0)
        anchors.add(base if count == 0 else f"{base}-{count}")
        seen[base] = count + 1
    return anchors


def prometheus_rules(text):
    """Return {alert_name: runbook_url or None} from a rules file.

    Line-oriented on purpose (see the header of prometheus-rules.yml)."""
    rules = {}
    current = None
    for line in text.splitlines():
        m = re.match(r"^\s*-\s*alert:\s*([A-Za-z0-9_]+)\s*$", line)
        if m:
            current = m.group(1)
            rules[current] = None
            continue
        m = re.match(r"^\s*runbook_url:\s*[\"']?([^\"'\s]+)[\"']?\s*$", line)
        if m and current is not None:
            rules[current] = m.group(1)
    return rules


def in_process_alerts(rust_source):
    return re.findall(r'^pub const [A-Z0-9_]+: &str = "([A-Za-z0-9_]+)";', rust_source, re.M)


def validate(mapping, runbook_md, rules, in_process):
    problems = []
    anchors = heading_anchors(runbook_md)
    base_url = mapping["base_url"].rstrip("#")
    alerts = mapping.get("alerts", {})

    for name, entry in sorted(alerts.items()):
        has_anchor = "anchor" in entry
        has_exempt = "exempt" in entry
        if has_anchor == has_exempt:
            problems.append(f"{name}: entry must have exactly one of `anchor` or `exempt`")
            continue
        if has_anchor and entry["anchor"] not in anchors:
            problems.append(
                f"{name}: anchor #{entry['anchor']} does not exist in {mapping['runbook']} "
                f"(heading renamed or removed?)"
            )
        if has_exempt:
            ex = entry["exempt"]
            if not str(ex.get("reason", "")).strip() or not str(ex.get("reviewed_by", "")).strip():
                problems.append(f"{name}: exemption needs a non-empty reason and reviewed_by")

    for name, url in sorted(rules.items()):
        entry = alerts.get(name)
        if entry is None:
            problems.append(f"{name}: Prometheus rule has no runbook link or exemption in the mapping")
            continue
        if "anchor" in entry and "exempt" not in entry:
            expected = f"{base_url}#{entry['anchor']}"
            if url != expected:
                problems.append(f"{name}: runbook_url annotation is {url!r}, expected {expected!r}")
        elif "exempt" in entry and url is not None:
            problems.append(f"{name}: exempt rule must not carry a runbook_url annotation")

    for name in in_process:
        if name not in alerts:
            problems.append(f"{name}: in-process alert has no runbook link or exemption in the mapping")

    return problems


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--root", default=Path(__file__).resolve().parent.parent, type=Path)
    args = parser.parse_args(argv)
    root = args.root

    mapping = json.loads((root / "alerting/runbook-links.json").read_text())
    runbook_md = (root / mapping["runbook"]).read_text()
    rules = prometheus_rules((root / "alerting/prometheus-rules.yml").read_text())
    in_process = in_process_alerts((root / "src/alerting/names.rs").read_text())

    problems = validate(mapping, runbook_md, rules, in_process)
    if problems:
        print("Alert runbook link check FAILED:", file=sys.stderr)
        for p in problems:
            print(f"  - {p}", file=sys.stderr)
        return 1

    print(
        f"Alert runbook links OK: {len(mapping['alerts'])} mapping entries, "
        f"{len(rules)} Prometheus rules, {len(in_process)} in-process alerts."
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())
