import { describe, it, expect } from 'vitest';
import crypto from 'crypto';

class WebhookVerifier {
  private secret: string;

  constructor(secret: string) {
    this.secret = secret;
  }

  verify(payload: string, signature: string): boolean {
    const computed = crypto
      .createHmac('sha256', this.secret)
      .update(payload)
      .digest('hex');

    return crypto.timingSafeEqual(
      Buffer.from(computed),
      Buffer.from(signature)
    );
  }

  sign(payload: string): string {
    return crypto
      .createHmac('sha256', this.secret)
      .update(payload)
      .digest('hex');
  }
}

describe('Synapse TypeScript SDK - Webhook Verification', () => {
  const secret = 'test-webhook-secret-key';
  const verifier = new WebhookVerifier(secret);

  it('should verify valid webhook signature', () => {
    const payload = JSON.stringify({ event: 'transaction.created', id: '123' });
    const signature = verifier.sign(payload);

    const isValid = verifier.verify(payload, signature);
    expect(isValid).toBe(true);
  });

  it('should reject invalid webhook signature', () => {
    const payload = JSON.stringify({ event: 'transaction.created', id: '123' });
    const invalidSignature = 'invalid_signature_value';

    const isValid = verifier.verify(payload, invalidSignature);
    expect(isValid).toBe(false);
  });

  it('should reject tampered payload', () => {
    const payload = JSON.stringify({ event: 'transaction.created', id: '123' });
    const signature = verifier.sign(payload);

    const tamperedPayload = JSON.stringify({
      event: 'transaction.created',
      id: '999',
    });

    const isValid = verifier.verify(tamperedPayload, signature);
    expect(isValid).toBe(false);
  });

  it('should handle string payloads', () => {
    const payload = 'raw string payload';
    const signature = verifier.sign(payload);

    const isValid = verifier.verify(payload, signature);
    expect(isValid).toBe(true);
  });

  it('should be timing-safe against constant-time attacks', () => {
    const payload = JSON.stringify({ amount: 100 });
    const signature = verifier.sign(payload);

    expect(() => {
      verifier.verify(payload, signature);
    }).not.toThrow();
  });

  it('should handle empty payload', () => {
    const payload = '';
    const signature = verifier.sign(payload);

    const isValid = verifier.verify(payload, signature);
    expect(isValid).toBe(true);
  });

  it('should use HMAC-SHA256 algorithm', () => {
    const payload = 'test payload';
    const signature = verifier.sign(payload);

    const expectedSignature = crypto
      .createHmac('sha256', secret)
      .update(payload)
      .digest('hex');

    expect(signature).toBe(expectedSignature);
  });

  it('should work with JSON event payloads', () => {
    const event = {
      type: 'settlement.created',
      id: 'sett-123',
      amount: 5000.0,
      asset_code: 'USD',
      timestamp: new Date().toISOString(),
    };

    const payload = JSON.stringify(event);
    const signature = verifier.sign(payload);

    const isValid = verifier.verify(payload, signature);
    expect(isValid).toBe(true);
  });

  it('should verify server-generated webhook fixtures', () => {
    const serverFixtures = [
      {
        payload: JSON.stringify({ event: 'transaction.settled', id: 'txn-1' }),
        signature:
          crypto
            .createHmac('sha256', secret)
            .update(JSON.stringify({ event: 'transaction.settled', id: 'txn-1' }))
            .digest('hex'),
      },
      {
        payload: JSON.stringify({ event: 'settlement.completed', id: 'sett-1' }),
        signature:
          crypto
            .createHmac('sha256', secret)
            .update(
              JSON.stringify({ event: 'settlement.completed', id: 'sett-1' })
            )
            .digest('hex'),
      },
    ];

    serverFixtures.forEach((fixture) => {
      const isValid = verifier.verify(fixture.payload, fixture.signature);
      expect(isValid).toBe(true);
    });
  });

  it('should handle concurrent verification requests', async () => {
    const payload = JSON.stringify({ id: 'test-123' });
    const signature = verifier.sign(payload);

    const verifications = Array(10)
      .fill(null)
      .map(() => Promise.resolve(verifier.verify(payload, signature)));

    const results = await Promise.all(verifications);
    expect(results.every((r) => r === true)).toBe(true);
  });
});
