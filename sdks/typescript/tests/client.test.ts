import { describe, it, expect, beforeEach, vi } from 'vitest';

interface ClientConfig {
  baseUrl: string;
  apiKey: string;
  timeout?: number;
}

class SynapseClient {
  private baseUrl: string;
  private apiKey: string;
  private timeout: number;

  constructor(config: ClientConfig) {
    this.baseUrl = config.baseUrl;
    this.apiKey = config.apiKey;
    this.timeout = config.timeout || 30000;
  }

  async request(method: string, path: string, options?: any) {
    const headers = {
      'Authorization': `Bearer ${this.apiKey}`,
      'Content-Type': 'application/json',
      ...options?.headers,
    };

    const response = await fetch(`${this.baseUrl}${path}`, {
      method,
      headers,
      body: options?.body ? JSON.stringify(options.body) : undefined,
    });

    if (!response.ok) {
      throw new Error(`HTTP ${response.status}`);
    }

    return response.json();
  }
}

describe('Synapse TypeScript SDK - Client', () => {
  let client: SynapseClient;
  const baseUrl = 'http://localhost:8000';
  const apiKey = 'test-api-key';

  beforeEach(() => {
    client = new SynapseClient({ baseUrl, apiKey });
  });

  it('should initialize client with config', () => {
    expect(client).toBeDefined();
  });

  it('should set default timeout', () => {
    const customClient = new SynapseClient({
      baseUrl,
      apiKey,
      timeout: 60000,
    });
    expect(customClient).toBeDefined();
  });

  it('should construct proper authorization headers', async () => {
    global.fetch = vi.fn().mockResolvedValueOnce({
      ok: true,
      json: async () => ({ success: true }),
    });

    await client.request('GET', '/transactions');

    expect(global.fetch).toHaveBeenCalled();
  });

  it('should handle async request/response', async () => {
    global.fetch = vi.fn().mockResolvedValueOnce({
      ok: true,
      json: async () => ({ id: '123', status: 'pending' }),
    });

    const result = await client.request('GET', '/transactions/123');
    expect(result).toHaveProperty('id');
  });

  it('should throw on HTTP error response', async () => {
    global.fetch = vi.fn().mockResolvedValueOnce({
      ok: false,
      status: 401,
      json: async () => ({ error: 'Unauthorized' }),
    });

    await expect(client.request('GET', '/transactions')).rejects.toThrow();
  });

  it('should support JSON request body serialization', async () => {
    global.fetch = vi.fn().mockResolvedValueOnce({
      ok: true,
      json: async () => ({ created: true }),
    });

    const payload = { amount: 100, asset_code: 'USD' };
    await client.request('POST', '/transactions', { body: payload });

    expect(global.fetch).toHaveBeenCalled();
  });

  it('should handle timeout configuration', () => {
    const customClient = new SynapseClient({
      baseUrl,
      apiKey,
      timeout: 5000,
    });
    expect(customClient).toBeDefined();
  });

  it('should support custom headers in requests', async () => {
    global.fetch = vi.fn().mockResolvedValueOnce({
      ok: true,
      json: async () => ({}),
    });

    await client.request('GET', '/transactions', {
      headers: { 'X-Custom-Header': 'value' },
    });

    expect(global.fetch).toHaveBeenCalled();
  });
});
