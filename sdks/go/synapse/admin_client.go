package synapse

import (
	"context"
	"net/http"
	"net/url"
	"strings"
	"time"
)

// AdminClient is the admin API client (Authorization: Bearer <key>).
//
// Construct with NewAdminClient or AdminClientBuilder.
type AdminClient struct {
	httpClient  *http.Client
	baseURL     string
	apiKey      string
	maxRetries  int
	baseDelayMs int64
}

// NewAdminClient creates an AdminClient with default settings.
func NewAdminClient(baseURL, apiKey string) *AdminClient {
	return NewAdminClientBuilder(baseURL, apiKey).Build()
}

// AdminClientBuilder builds an AdminClient with custom settings.
type AdminClientBuilder struct {
	baseURL     string
	apiKey      string
	timeout     time.Duration
	maxRetries  int
	baseDelayMs int64
	httpClient  *http.Client
}

// NewAdminClientBuilder returns an AdminClientBuilder with defaults.
func NewAdminClientBuilder(baseURL, apiKey string) *AdminClientBuilder {
	return &AdminClientBuilder{
		baseURL:     strings.TrimRight(baseURL, "/"),
		apiKey:      apiKey,
		timeout:     defaultTimeout,
		maxRetries:  defaultMaxRetries,
		baseDelayMs: 200,
	}
}

// WithTimeout sets the HTTP request timeout.
func (b *AdminClientBuilder) WithTimeout(d time.Duration) *AdminClientBuilder {
	b.timeout = d
	return b
}

// WithMaxRetries sets the maximum retry attempts for idempotent requests.
func (b *AdminClientBuilder) WithMaxRetries(n int) *AdminClientBuilder {
	b.maxRetries = n
	return b
}

// WithHTTPClient replaces the underlying http.Client (useful in tests).
func (b *AdminClientBuilder) WithHTTPClient(c *http.Client) *AdminClientBuilder {
	b.httpClient = c
	return b
}

// Build constructs the AdminClient.
func (b *AdminClientBuilder) Build() *AdminClient {
	hc := b.httpClient
	if hc == nil {
		hc = &http.Client{Timeout: b.timeout}
	}
	return &AdminClient{
		httpClient:  hc,
		baseURL:     b.baseURL,
		apiKey:      b.apiKey,
		maxRetries:  b.maxRetries,
		baseDelayMs: b.baseDelayMs,
	}
}

// ── Resource accessors ────────────────────────────────────────────────────────

// Locks returns the distributed locks admin resource handle.
func (a *AdminClient) Locks() *AdminLocksResource {
	return &AdminLocksResource{admin: a}
}

// Reconciliation returns the reconciliation admin resource handle.
func (a *AdminClient) Reconciliation() *AdminReconciliationResource {
	return &AdminReconciliationResource{admin: a}
}

// Settlements returns the admin settlements resource handle.
func (a *AdminClient) Settlements() *AdminSettlementsResource {
	return &AdminSettlementsResource{admin: a}
}

// BulkStatus returns the bulk-status admin resource handle.
func (a *AdminClient) BulkStatus() *AdminBulkStatusResource {
	return &AdminBulkStatusResource{admin: a}
}

// Dlq returns the DLQ admin resource handle.
func (a *AdminClient) Dlq() *AdminDlqResource {
	return &AdminDlqResource{admin: a}
}

// WebhookReplay returns the webhook-replay admin resource handle.
func (a *AdminClient) WebhookReplay() *AdminWebhookReplayResource {
	return &AdminWebhookReplayResource{admin: a}
}

// ── Internal HTTP helpers (admin uses Authorization: Bearer) ─────────────────

func (a *AdminClient) get(ctx context.Context, path string, query url.Values, out interface{}) error {
	c := &Client{
		httpClient:  a.httpClient,
		baseURL:     a.baseURL,
		apiKey:      a.apiKey,
		maxRetries:  a.maxRetries,
		baseDelayMs: a.baseDelayMs,
	}
	req, err := c.buildRequest(ctx, "GET", path, query, nil, true)
	if err != nil {
		return err
	}
	return c.execRequest(req, out)
}

func (a *AdminClient) post(ctx context.Context, path string, body interface{}, out interface{}) error {
	c := &Client{
		httpClient:  a.httpClient,
		baseURL:     a.baseURL,
		apiKey:      a.apiKey,
		maxRetries:  a.maxRetries,
		baseDelayMs: a.baseDelayMs,
	}
	req, err := c.buildRequest(ctx, "POST", path, nil, body, true)
	if err != nil {
		return err
	}
	return c.execRequest(req, out)
}

func (a *AdminClient) patch(ctx context.Context, path string, body interface{}, out interface{}) error {
	c := &Client{
		httpClient:  a.httpClient,
		baseURL:     a.baseURL,
		apiKey:      a.apiKey,
		maxRetries:  a.maxRetries,
		baseDelayMs: a.baseDelayMs,
	}
	req, err := c.buildRequest(ctx, "PATCH", path, nil, body, true)
	if err != nil {
		return err
	}
	return c.execRequest(req, out)
}
