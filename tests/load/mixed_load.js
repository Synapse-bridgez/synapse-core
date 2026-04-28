/**
 * mixed_load.js — Realistic mix of callbacks (60%), reads (25%), and searches (15%).
 *
 * Success criteria:
 *   - p95 latency < 200ms
 *   - error rate < 0.1%
 *
 * Run:
 *   docker compose -f docker-compose.load.yml --profile load-test run --rm k6 run /scripts/mixed_load.js
 */

import http from "k6/http";
import { check, sleep } from "k6";
import { Rate, Trend, Counter } from "k6/metrics";
import { randomString, uuidv4 } from "https://jslib.k6.io/k6-utils/1.4.0/index.js";

const errorRate = new Rate("errors");
const callbackDuration = new Trend("callback_duration", true);
const readDuration = new Trend("read_duration", true);
const searchDuration = new Trend("search_duration", true);
const callbackCount = new Counter("callback_requests");
const readCount = new Counter("read_requests");
const searchCount = new Counter("search_requests");

const BASE_URL = __ENV.BASE_URL || "http://localhost:3000";
const ASSETS = ["USDC", "USDT", "XLM", "BTC", "ETH"];
const STATUSES = ["pending", "completed", "failed"];

// Shared pool of transaction IDs collected during the run for read requests.
const txIds = [];

export const options = {
  scenarios: {
    mixed_traffic: {
      executor: "ramping-arrival-rate",
      startRate: 100,
      timeUnit: "1s",
      preAllocatedVUs: 100,
      maxVUs: 400,
      stages: [
        { duration: "1m", target: 200 },
        { duration: "3m", target: 500 },
        { duration: "1m", target: 500 },
        { duration: "30s", target: 0 },
      ],
    },
  },
  thresholds: {
    http_req_duration: ["p(95)<200"],
    errors: ["rate<0.001"],
  },
};

const defaultHeaders = {
  "Content-Type": "application/json",
  "X-API-Key": __ENV.API_KEY || "load-test-key",
};

function doCallback() {
  const payload = JSON.stringify({
    stellar_account: `G${randomString(55, "ABCDEFGHIJKLMNOPQRSTUVWXYZ234567")}`,
    amount: (Math.random() * 9999 + 1).toFixed(2),
    asset_code: ASSETS[Math.floor(Math.random() * ASSETS.length)],
    anchor_transaction_id: uuidv4(),
    callback_type: "deposit",
    callback_status: "completed",
    memo: `mixed-${randomString(6)}`,
    memo_type: "text",
  });

  const res = http.post(`${BASE_URL}/callback`, payload, {
    headers: { ...defaultHeaders, "Idempotency-Key": uuidv4() },
    timeout: "10s",
  });

  const ok = check(res, { "callback 201": (r) => r.status === 201 });
  errorRate.add(!ok);
  callbackDuration.add(res.timings.duration);
  callbackCount.add(1);

  // Collect IDs for subsequent read requests.
  if (ok) {
    try {
      const body = JSON.parse(res.body);
      if (body.id && txIds.length < 500) {
        txIds.push(body.id);
      }
    } catch {
      // ignore parse errors
    }
  }
}

function doRead() {
  if (txIds.length === 0) {
    // No IDs yet — fall back to a callback.
    doCallback();
    return;
  }
  const id = txIds[Math.floor(Math.random() * txIds.length)];
  const res = http.get(`${BASE_URL}/transactions/${id}`, {
    headers: defaultHeaders,
    timeout: "10s",
  });

  const ok = check(res, { "read 200": (r) => r.status === 200 || r.status === 404 });
  errorRate.add(!ok);
  readDuration.add(res.timings.duration);
  readCount.add(1);
}

function doSearch() {
  const params = new URLSearchParams({
    asset_code: ASSETS[Math.floor(Math.random() * ASSETS.length)],
    status: STATUSES[Math.floor(Math.random() * STATUSES.length)],
    limit: String(Math.floor(Math.random() * 20) + 5),
  });

  const res = http.get(`${BASE_URL}/transactions/search?${params}`, {
    headers: defaultHeaders,
    timeout: "10s",
  });

  const ok = check(res, { "search 200": (r) => r.status === 200 });
  errorRate.add(!ok);
  searchDuration.add(res.timings.duration);
  searchCount.add(1);
}

export default function () {
  const roll = Math.random();
  if (roll < 0.60) {
    doCallback();
  } else if (roll < 0.85) {
    doRead();
  } else {
    doSearch();
  }
  sleep(0.01);
}

export function handleSummary(data) {
  return {
    "/results/mixed_load_summary.html": htmlReport(data),
    stdout: textSummary(data),
  };
}

function htmlReport(data) {
  const p95 = data.metrics.http_req_duration?.values?.["p(95)"] ?? "N/A";
  const p99 = data.metrics.http_req_duration?.values?.["p(99)"] ?? "N/A";
  const errRate = ((data.metrics.errors?.values?.rate ?? 0) * 100).toFixed(3);
  const reqs = data.metrics.http_reqs?.values?.count ?? 0;
  const cbReqs = data.metrics.callback_requests?.values?.count ?? 0;
  const rdReqs = data.metrics.read_requests?.values?.count ?? 0;
  const srReqs = data.metrics.search_requests?.values?.count ?? 0;
  return `<!DOCTYPE html><html><head><title>Mixed Load Test</title></head><body>
<h1>Mixed Load Test Results</h1>
<table border="1" cellpadding="6">
  <tr><th>Metric</th><th>Value</th><th>Threshold</th><th>Pass</th></tr>
  <tr><td>p95 latency</td><td>${p95}ms</td><td>&lt;200ms</td><td>${p95 < 200 ? "✅" : "❌"}</td></tr>
  <tr><td>p99 latency</td><td>${p99}ms</td><td>—</td><td>—</td></tr>
  <tr><td>Error rate</td><td>${errRate}%</td><td>&lt;0.1%</td><td>${errRate < 0.1 ? "✅" : "❌"}</td></tr>
  <tr><td>Total requests</td><td>${reqs}</td><td>—</td><td>—</td></tr>
  <tr><td>Callbacks</td><td>${cbReqs}</td><td>~60%</td><td>—</td></tr>
  <tr><td>Reads</td><td>${rdReqs}</td><td>~25%</td><td>—</td></tr>
  <tr><td>Searches</td><td>${srReqs}</td><td>~15%</td><td>—</td></tr>
</table></body></html>`;
}

function textSummary(data) {
  const p95 = data.metrics.http_req_duration?.values?.["p(95)"] ?? "N/A";
  const errRate = ((data.metrics.errors?.values?.rate ?? 0) * 100).toFixed(3);
  return `\n=== Mixed Load Test ===\np95 latency : ${p95}ms (threshold: <200ms)\nError rate  : ${errRate}% (threshold: <0.1%)\n`;
}
