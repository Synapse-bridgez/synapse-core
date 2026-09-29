#!/usr/bin/env python3
"""Fetch and compare effective Synapse configuration from dev/staging/prod."""

import argparse
import json
import os
import sys
import urllib.error
import urllib.request
from pathlib import Path


ENVIRONMENTS = ("dev", "staging", "prod")


def fetch_snapshot(url, token):
    request = urllib.request.Request(
        url.rstrip("/") + "/admin/config/export",
        headers={"Authorization": f"Bearer {token}", "Accept": "application/json"},
    )
    with urllib.request.urlopen(request, timeout=20) as response:
        if response.status != 200:
            raise RuntimeError(f"{url}: endpoint returned HTTP {response.status}")
        return json.loads(response.read())


def flatten(value, prefix=""):
    fields = {}
    if isinstance(value, dict):
        for key, child in value.items():
            path = f"{prefix}.{key}" if prefix else key
            fields.update(flatten(child, path))
    else:
        fields[prefix] = value
    return fields


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for environment in ENVIRONMENTS:
        parser.add_argument(f"--{environment}", required=True, help=f"{environment} base URL")
    parser.add_argument(
        "--exceptions",
        default="scripts/config-parity-exceptions.json",
        help="JSON file documenting approved environment-specific values",
    )
    args = parser.parse_args()

    try:
        exceptions = json.loads(Path(args.exceptions).read_text(encoding="utf-8"))
        snapshots = {}
        for environment in ENVIRONMENTS:
            token_name = f"{environment.upper()}_ADMIN_API_KEY"
            token = os.environ.get(token_name)
            if not token:
                raise RuntimeError(f"Set {token_name} to an admin API key")
            snapshots[environment] = fetch_snapshot(getattr(args, environment), token)
    except (OSError, ValueError, urllib.error.URLError, RuntimeError) as error:
        print(f"Configuration parity failed: {error}", file=sys.stderr)
        return 2

    flattened = {env: flatten(snapshot) for env, snapshot in snapshots.items()}
    all_paths = sorted(set().union(*(fields.keys() for fields in flattened.values())))
    expected = exceptions.get("expected_divergences", {})
    findings = []
    for path in all_paths:
        observed = {env: flattened[env].get(path) for env in ENVIRONMENTS}
        if observed["dev"] == observed["staging"] == observed["prod"]:
            continue
        approved_values = expected.get(path)
        is_expected = approved_values is not None and observed == approved_values
        findings.append({
            "path": path,
            "classification": "expected" if is_expected else "unexpected",
            "values": observed,
        })

    unexpected = [item for item in findings if item["classification"] == "unexpected"]
    if findings:
        print(json.dumps({"unexpected_count": len(unexpected), "divergences": findings}, indent=2, sort_keys=True))
    else:
        print("No configuration divergence found.")
    return 1 if unexpected else 0


if __name__ == "__main__":
    sys.exit(main())