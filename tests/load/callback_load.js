/**
 * callback_load.js — Sustained callback ingestion at 1000 req/s for 5 minutes.
 *
 * Success criteria:
 *   - p95 latency < 200ms
 *   - error rate < 0.1%
 *
 * Run:
 *   docker compose -f docker-compose.load.yml --profile load-test run --rm k6 run /scripts/callback_load.js
 */

import http from "k6/http";
import { check, sleep } from "k6";
import { Rate, Trend } from "k6/metrics";
import { randomString, uuidv4 } from "https://jslib.k6.io/k6-utils/1.4.0/index.js";

const errorRate = new Rate("errors");
const callbackDuration = new Trend("callback_duration", true);

const BASE_URL = __ENV.BASE_URL || "http://localhost:3000";

export const options = {
  scenarios: {
    sustained_load: {
      executor: "constant-arrival-rate",
      rate: 1000,
      timeUnit: "1s",
      duration: "5m",
      preAllocatedVUs: 200,
      maxVUs: 500,
    },
  },
  thresholds: {
    http_req_duration: ["p(95)<200"],
    errors: ["rate<0.001"],
  },
};

function randomAsset() {
  const assets = ["USDC", "USDT", "XLM", "BTC", "ETH"];
  return assets[Math.floor(Math.random() * assets.length)];
}

function randomAmount() {
  return (Math.random() * 9999 + 1).toFixed(2);
}

export default function () {
  const payload = JSON.stringify({
    stellar_account: `G${randomString(55, "ABCDEFGHIJKLMNOPQRSTUVWXYZ234567")}`,
    amount: randomAmount(),
    asset_code: randomAsset(),
    anchor_transaction_id: uuidv4(),
    callback_type: "deposit",
    callback_status: "completed",
    memo: `load-test-${randomString(8)}`,
    memo_type: "text",
  });

  const params = {
    headers: {
      "Content-Type": "application/json",
      "X-API-Key": __ENV.API_KEY || "load-test-key",
      "Idempotency-Key": uuidv4(),
    },
    timeout: "10s",
  };

  const res = http.post(`${BASE_URL}/callback`, payload, params);

  const ok = check(res, {
    "status is 201": (r) => r.status === 201,
    "response has id": (r) => {
      try {
        return JSON.parse(r.body).id !== undefined;
      } catch {
        return false;
      }
    },
  });

  errorRate.add(!ok);
  callbackDuration.add(res.timings.duration);
}

export function handleSummary(data) {
  return {
    "/results/callback_load_summary.html": htmlReport(data),
    stdout: textSummary(data, { indent: " ", enableColors: true }),
  };
}

// Inline minimal HTML report generator (no external dependency required).
function htmlReport(data) {
  const p95 = data.metrics.http_req_duration?.values?.["p(95)"] ?? "N/A";
  const p99 = data.metrics.http_req_duration?.values?.["p(99)"] ?? "N/A";
  const errRate = ((data.metrics.errors?.values?.rate ?? 0) * 100).toFixed(3);
  const reqs = data.metrics.http_reqs?.values?.count ?? 0;
  return `<!DOCTYPE html><html><head><title>Callback Load Test</title></head><body>
<h1>Callback Load Test Results</h1>
<table border="1" cellpadding="6">
  <tr><th>Metric</th><th>Value</th><th>Threshold</th><th>Pass</th></tr>
  <tr><td>p95 latency</td><td>${p95}ms</td><td>&lt;200ms</td><td>${p95 < 200 ? "✅" : "❌"}</td></tr>
  <tr><td>p99 latency</td><td>${p99}ms</td><td>—</td><td>—</td></tr>
  <tr><td>Error rate</td><td>${errRate}%</td><td>&lt;0.1%</td><td>${errRate < 0.1 ? "✅" : "❌"}</td></tr>
  <tr><td>Total requests</td><td>${reqs}</td><td>—</td><td>—</td></tr>
</table></body></html>`;
}

function textSummary(data, _opts) {
  const p95 = data.metrics.http_req_duration?.values?.["p(95)"] ?? "N/A";
  const errRate = ((data.metrics.errors?.values?.rate ?? 0) * 100).toFixed(3);
  return `\n=== Callback Load Test ===\np95 latency : ${p95}ms (threshold: <200ms)\nError rate  : ${errRate}% (threshold: <0.1%)\n`;
}
