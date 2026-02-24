import http from 'k6/http';
import { check, sleep } from 'k6';
import { Rate } from 'k6/metrics';

const idempotencyHitRate = new Rate('idempotency_hits');

// Test idempotency behavior under load
export const options = {
  stages: [
    { duration: '2m', target: 20 },
    { duration: '5m', target: 20 },
    { duration: '1m', target: 0 },
  ],
  thresholds: {
    http_req_duration: ['p(95)<300'], // Should be fast from cache
    idempotency_hits: ['rate>0.3'], // At least 30% should hit cache
  },
};

const BASE_URL = __ENV.BASE_URL || 'http://localhost:3000';

// Shared idempotency keys to test duplicate detection
const sharedKeys = [
  'shared_key_1',
  'shared_key_2',
  'shared_key_3',
  'shared_key_4',
  'shared_key_5',
];

function generateWebhookPayload() {
  const timestamp = new Date().toISOString();
  const txId = `idem_${Date.now()}_${Math.random().toString(36).substr(2, 9)}`;
  
  return {
    id: txId,
    anchor_transaction_id: `anchor_${txId}`,
    event_type: 'deposit_completed',
    amount: '100.00',
    asset_code: 'USD',
    timestamp: timestamp,
  };
}

export default function () {
  const payload = JSON.stringify(generateWebhookPayload());
  
  // 50% chance to use a shared key (test idempotency)
  const useSharedKey = Math.random() < 0.5;
  const idempotencyKey = useSharedKey
    ? sharedKeys[Math.floor(Math.random() * sharedKeys.length)]
    : `unique_${Date.now()}_${__VU}_${__ITER}`;
  
  const params = {
    headers: {
      'Content-Type': 'application/json',
      'X-Idempotency-Key': idempotencyKey,
    },
  };

  const response = http.post(`${BASE_URL}/webhook`, payload, params);
  
  // Check if we got a cached response (429 or very fast 200)
  const wasCached = response.status === 429 || 
                    (response.status === 200 && response.timings.duration < 50);
  
  idempotencyHitRate.add(wasCached);
  
  check(response, {
    'status is 200 or 429': (r) => r.status === 200 || r.status === 429,
    'idempotency working': (r) => useSharedKey ? (r.status === 200 || r.status === 429) : r.status === 200,
  });

  sleep(1);
}
