/**
 * search_load.js — Concurrent transaction search queries.
 *
 * Success criteria:
 *   - p95 latency < 200ms
 *   - error rate < 0.1%
 *
 * Run:
 *   docker compose -f docker-compose.load.yml --profile load-test run --rm k6 run /scripts/search_load.js
 */

import http from "k6/http";
import { check, sleep } from "k6";
import { Rate, Trend } from "k6/metrics";
import { randomString } from "https://jslib.k6.io/k6-utils/1.4.0/index.js";

const errorRate = new Rate("errors");
const searchDuration = new Trend("search_duration", true);

const BASE_URL = __ENV.BASE_URL || "http://localhost:3000";

export const options = {
  scenarios: {
    concurrent_search: {
      executor: "ramping-vus",
      startVUs: 0,
      stages: [
        { duration: "30s", target: 50 },
        { duration: "4m", target: 50 },
        { duration: "30s", target: 0 },
      ],
    },
  },
  thresholds: {
    http_req_duration: ["p(95)<200"],
    errors: ["rate<0.001"],
  },
};

const ASSETS = ["USDC", "USDT", "XLM"];
const STATUSES = ["pending", "completed", "failed"];

function randomSearchParams() {
  const params = new URLSearchParams();
  // Randomly include different search dimensions.
  if (Math.random() > 0.5) {
    params.set("asset_code", ASSETS[Math.floor(Math.random() * ASSETS.length)]);
  }
  if (Math.random() > 0.5) {
    params.set("status", STATUSES[Math.floor(Math.random() * STATUSES.length)]);
  }
  if (Math.random() > 0.7) {
    params.set("memo", randomString(6));
  }
  params.set("limit", String(Math.floor(Math.random() * 50) + 10));
  return params.toString();
}

export default function () {
  const qs = randomSearchParams();
  const url = `${BASE_URL}/transactions/search?${qs}`;

  const params = {
    headers: {
      "X-API-Key": __ENV.API_KEY || "load-test-key",
    },
    timeout: "10s",
  };

  const res = http.get(url, params);

  const ok = check(res, {
    "status is 200": (r) => r.status === 200,
    "response is array": (r) => {
      try {
        const body = JSON.parse(r.body);
        return Array.isArray(body) || Array.isArray(body.data);
      } catch {
        return false;
      }
    },
  });

  errorRate.add(!ok);
  searchDuration.add(res.timings.duration);

  sleep(0.05); // slight think-time to model realistic search cadence
}

export function handleSummary(data) {
  return {
    "/results/search_load_summary.html": htmlReport(data),
    stdout: textSummary(data),
  };
}

function htmlReport(data) {
  const p95 = data.metrics.http_req_duration?.values?.["p(95)"] ?? "N/A";
  const p99 = data.metrics.http_req_duration?.values?.["p(99)"] ?? "N/A";
  const errRate = ((data.metrics.errors?.values?.rate ?? 0) * 100).toFixed(3);
  const reqs = data.metrics.http_reqs?.values?.count ?? 0;
  return `<!DOCTYPE html><html><head><title>Search Load Test</title></head><body>
<h1>Search Load Test Results</h1>
<table border="1" cellpadding="6">
  <tr><th>Metric</th><th>Value</th><th>Threshold</th><th>Pass</th></tr>
  <tr><td>p95 latency</td><td>${p95}ms</td><td>&lt;200ms</td><td>${p95 < 200 ? "✅" : "❌"}</td></tr>
  <tr><td>p99 latency</td><td>${p99}ms</td><td>—</td><td>—</td></tr>
  <tr><td>Error rate</td><td>${errRate}%</td><td>&lt;0.1%</td><td>${errRate < 0.1 ? "✅" : "❌"}</td></tr>
  <tr><td>Total requests</td><td>${reqs}</td><td>—</td><td>—</td></tr>
</table></body></html>`;
}

function textSummary(data) {
  const p95 = data.metrics.http_req_duration?.values?.["p(95)"] ?? "N/A";
  const errRate = ((data.metrics.errors?.values?.rate ?? 0) * 100).toFixed(3);
  return `\n=== Search Load Test ===\np95 latency : ${p95}ms (threshold: <200ms)\nError rate  : ${errRate}% (threshold: <0.1%)\n`;
}
