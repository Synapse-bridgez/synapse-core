import http from 'k6/http';
import { check, sleep } from 'k6';
import { Rate } from 'k6/metrics';

const errorRate = new Rate('errors');

// Spike test: sudden traffic surge
export const options = {
  stages: [
    { duration: '1m', target: 10 },   // Normal load
    { duration: '30s', target: 200 }, // Sudden spike
    { duration: '3m', target: 200 },  // Sustained spike
    { duration: '1m', target: 10 },   // Recovery
    { duration: '1m', target: 0 },    // Ramp down
  ],
  thresholds: {
    http_req_duration: ['p(95)<1000'], // More lenient during spike
    errors: ['rate<0.1'], // Allow 10% error rate during spike
  },
};

const BASE_URL = __ENV.BASE_URL || 'http://localhost:3000';

function generateWebhookPayload() {
  const timestamp = new Date().toISOString();
  const txId = `spike_${Date.now()}_${Math.random().toString(36).substr(2, 9)}`;
  
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
  const idempotencyKey = `spike_${Date.now()}_${__VU}_${__ITER}`;
  
  const params = {
    headers: {
      'Content-Type': 'application/json',
      'X-Idempotency-Key': idempotencyKey,
    },
  };

  const response = http.post(`${BASE_URL}/webhook`, payload, params);
  
  const success = check(response, {
    'status is 200 or 429': (r) => r.status === 200 || r.status === 429,
  });

  errorRate.add(!success);
  sleep(0.5); // Minimal think time during spike
}
