#!/usr/bin/env python3
"""Run authenticated, non-mutating synthetic probes from an external runner."""

import hashlib
import hmac
import json
import os
import sys
import time
import urllib.parse
import urllib.request


BASE_URL = os.environ.get("SYNTHETIC_BASE_URL", "").rstrip("/")
PROBE_SECRET = os.environ.get("SYNTHETIC_PROBE_SECRET", "").encode()
PUSHGATEWAY_URL = os.environ.get("SYNTHETIC_PUSHGATEWAY_URL", "").rstrip("/")
PUSHGATEWAY_TOKEN = os.environ.get("SYNTHETIC_PUSHGATEWAY_TOKEN", "")
GRAPHQL_QUERY = "query SyntheticProbe { __typename }"


def signed_headers(body: bytes) -> dict[str, str]:
    timestamp = str(int(time.time()))
    digest = hmac.new(
        PROBE_SECRET,
        timestamp.encode() + b"." + body,
        hashlib.sha256,
    ).hexdigest()
    return {
        "Content-Type": "application/json",
        "X-Synthetic-Probe-Timestamp": timestamp,
        "X-Synthetic-Probe-Signature": f"sha256={digest}",
    }


def post_json(flow: str, path: str, payload: dict) -> dict:
    body = json.dumps(payload, separators=(",", ":")).encode()
    request = urllib.request.Request(
        f"{BASE_URL}{path}",
        data=body,
        headers=signed_headers(body),
        method="POST",
    )
    with urllib.request.urlopen(request, timeout=15) as response:
        result = json.loads(response.read())
        if flow == "webhook_callback":
            if response.status != 201 or result.get("synthetic") is not True:
                raise RuntimeError(f"unexpected callback response: {response.status} {result}")
        elif response.status != 200 or result.get("data", {}).get("__typename") != "Query":
            raise RuntimeError(f"unexpected GraphQL response: {response.status} {result}")
    print(f"{flow}: passed")
    return result


def run_probe(flow: str, path: str, payload: dict) -> bool:
    try:
        post_json(flow, path, payload)
        return True
    except Exception as error:
        print(f"{flow}: failed: {error}", file=sys.stderr)
        return False


def push_metrics(results: dict[str, bool]) -> None:
    timestamp = int(time.time())
    for flow, succeeded in results.items():
        lines = [
            "# TYPE synapse_synthetic_probe_success gauge",
            f'synapse_synthetic_probe_success{{probe_type="synthetic"}} {int(succeeded)}',
        ]
        if succeeded:
            lines.extend(
                [
                    "# TYPE synapse_synthetic_probe_last_success_timestamp_seconds gauge",
                    f'synapse_synthetic_probe_last_success_timestamp_seconds{{probe_type="synthetic"}} {timestamp}',
                ]
            )
        body = ("\n".join(lines) + "\n").encode()
        url = (
            f"{PUSHGATEWAY_URL}/metrics/job/synapse-synthetic/flow/"
            f"{urllib.parse.quote(flow, safe='')}"
        )
        headers = {"Content-Type": "text/plain; version=0.0.4"}
        headers["Authorization"] = f"Bearer {PUSHGATEWAY_TOKEN}"
        request = urllib.request.Request(url, data=body, headers=headers, method="POST")
        with urllib.request.urlopen(request, timeout=15) as response:
            if response.status not in (200, 202):
                raise RuntimeError(f"Pushgateway returned HTTP {response.status} for {flow}")


def main() -> int:
    if not BASE_URL or not PROBE_SECRET or not PUSHGATEWAY_URL or not PUSHGATEWAY_TOKEN:
        raise ValueError("all SYNTHETIC_* workflow secrets must be configured")
    if not BASE_URL.startswith("https://"):
        raise ValueError("SYNTHETIC_BASE_URL must use HTTPS")
    if not PUSHGATEWAY_URL.startswith("https://"):
        raise ValueError("SYNTHETIC_PUSHGATEWAY_URL must use HTTPS")
    if not PUSHGATEWAY_TOKEN:
        raise ValueError("SYNTHETIC_PUSHGATEWAY_TOKEN must be configured")
    callback_payload = {
        "stellar_account": "G" + "A" * 55,
        "amount": "1.00",
        "asset_code": "USD",
        "callback_type": "deposit",
        "callback_status": "pending",
        "anchor_transaction_id": "synthetic-probe",
    }
    results = {
        "webhook_callback": run_probe(
            "webhook_callback", "/__synthetic/callback", callback_payload
        ),
        "graphql_query": run_probe(
            "graphql_query", "/__synthetic/graphql", {"query": GRAPHQL_QUERY}
        ),
    }
    try:
        push_metrics(results)
    except Exception as error:
        print(f"synthetic metrics push failed: {error}", file=sys.stderr)
        return 2
    return 0 if all(results.values()) else 1


if __name__ == "__main__":
    raise SystemExit(main())
