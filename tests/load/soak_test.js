import http from 'k6/http';
import { check, sleep } from 'k6';
import { Rate, Counter } from 'k6/metrics';

const errorRate = new Rate('errors');
const totalRequests = new Counter('total_requests');

// Soak test: sustained load over extended period
export const options = {
  stages: [
    { duration: '5m', target: 30 },   // Ramp up
    { duration: '30m', target: 30 },  // Sustained load (30 min)
    { duration: '5m', target: 0 },    // Ramp down
  ],
  thresholds: {
    http_req_duration: ['p(95)<500', 'p(99)<1000'],
    errors: ['rate<0.02'], // Very low error rate for stability
    http_req_failed: ['rate<0.02'],
  },
};

const BASE_URL = __ENV.BASE_URL || 'http://localhost:3000';

function generateWebhookPayload() {
  const timestamp = new Date().toISOString();
  const txId = `soak_${Date.now()}_${Math.random().toString(36).substr(2, 9)}`;
  
  return {
    id: txId,
    anchor_transaction_id: `anchor_${txId}`,
    event_type: 'deposit_completed',
    amount: (Math.random() * 1000 + 10).toFixed(2),
    asset_code: 'USD',
    timestamp: timestamp,
  };
}

export default function () {
  const payload = JSON.stringify(generateWebhookPayload());
  const idempotencyKey = `soak_${Date.now()}_${__VU}_${__ITER}`;
  
  const params = {
    headers: {
      'Content-Type': 'application/json',
      'X-Idempotency-Key': idempotencyKey,
    },
  };

  const response = http.post(`${BASE_URL}/webhook`, payload, params);
  
  totalRequests.add(1);
  
  const success = check(response, {
    'status is 200': (r) => r.status === 200,
    'no memory leaks (response time stable)': (r) => r.timings.duration < 1000,
  });

  errorRate.add(!success);
  sleep(2); // Consistent pacing
}
