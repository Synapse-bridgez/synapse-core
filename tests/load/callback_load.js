import http from 'k6/http';
import { check, sleep } from 'k6';
import { Rate, Trend } from 'k6/metrics';

// Custom metrics
const errorRate = new Rate('errors');
const webhookDuration = new Trend('webhook_duration');

// Test configuration
export const options = {
  stages: [
    { duration: '2m', target: 10 },   // Ramp up to 10 users
    { duration: '5m', target: 10 },   // Stay at 10 users
    { duration: '2m', target: 50 },   // Ramp up to 50 users
    { duration: '5m', target: 50 },   // Stay at 50 users
    { duration: '2m', target: 100 },  // Ramp up to 100 users
    { duration: '5m', target: 100 },  // Stay at 100 users
    { duration: '2m', target: 0 },    // Ramp down to 0 users
  ],
  thresholds: {
    http_req_duration: ['p(95)<500', 'p(99)<1000'], // 95% under 500ms, 99% under 1s
    errors: ['rate<0.05'], // Error rate under 5%
    http_req_failed: ['rate<0.05'],
  },
};

const BASE_URL = __ENV.BASE_URL || 'http://localhost:3000';

// Generate realistic webhook payload
function generateWebhookPayload() {
  const timestamp = new Date().toISOString();
  const txId = `tx_${Date.now()}_${Math.random().toString(36).substr(2, 9)}`;
  
  return {
    id: txId,
    anchor_transaction_id: `anchor_${txId}`,
    event_type: 'deposit_completed',
    amount: (Math.random() * 1000 + 10).toFixed(2),
    asset_code: 'USD',
    user_id: `user_${Math.floor(Math.random() * 10000)}`,
    timestamp: timestamp,
  };
}

export default function () {
  const payload = JSON.stringify(generateWebhookPayload());
  const idempotencyKey = `key_${Date.now()}_${__VU}_${__ITER}`;
  
  const params = {
    headers: {
      'Content-Type': 'application/json',
      'X-Idempotency-Key': idempotencyKey,
    },
    timeout: '30s',
  };

  const response = http.post(`${BASE_URL}/webhook`, payload, params);
  
  // Check response
  const success = check(response, {
    'status is 200': (r) => r.status === 200,
    'response has success field': (r) => JSON.parse(r.body).success === true,
    'response time < 500ms': (r) => r.timings.duration < 500,
  });

  errorRate.add(!success);
  webhookDuration.add(response.timings.duration);

  // Simulate realistic think time between requests
  sleep(Math.random() * 2 + 1); // 1-3 seconds
}
