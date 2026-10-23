package synapse

import (
	"bytes"
	"context"
	"encoding/json"
	"fmt"
	"io"
	"net/http"
	"net/url"
	"strconv"
	"strings"
	"time"
)

const (
	defaultTimeout    = 30 * time.Second
	defaultMaxRetries = 3
)

// Client is the public API client (X-API-Key authentication).
//
// Construct with NewClient or ClientBuilder.
type Client struct {
	httpClient  *http.Client
	baseURL     string
	apiKey      string
	maxRetries  int
	baseDelayMs int64
}

// NewClient creates a Client with default settings.
func NewClient(baseURL, apiKey string) *Client {
	return NewClientBuilder(baseURL, apiKey).Build()
}

// ClientBuilder builds a Client with custom settings.
type ClientBuilder struct {
	baseURL     string
	apiKey      string
	timeout     time.Duration
	maxRetries  int
	baseDelayMs int64
	httpClient  *http.Client
}

// NewClientBuilder returns a ClientBuilder with defaults.
func NewClientBuilder(baseURL, apiKey string) *ClientBuilder {
	return &ClientBuilder{
		baseURL:     strings.TrimRight(baseURL, "/"),
		apiKey:      apiKey,
		timeout:     defaultTimeout,
		maxRetries:  defaultMaxRetries,
		baseDelayMs: 200,
	}
}

// WithTimeout sets the HTTP request timeout.
func (b *ClientBuilder) WithTimeout(d time.Duration) *ClientBuilder {
	b.timeout = d
	return b
}

// WithMaxRetries sets the maximum number of retry attempts for idempotent requests.
func (b *ClientBuilder) WithMaxRetries(n int) *ClientBuilder {
	b.maxRetries = n
	return b
}

// WithBaseDelayMs sets the base exponential-backoff delay in milliseconds.
func (b *ClientBuilder) WithBaseDelayMs(ms int64) *ClientBuilder {
	b.baseDelayMs = ms
	return b
}

// WithHTTPClient replaces the underlying http.Client (useful in tests).
func (b *ClientBuilder) WithHTTPClient(c *http.Client) *ClientBuilder {
	b.httpClient = c
	return b
}

// Build constructs the Client.
func (b *ClientBuilder) Build() *Client {
	hc := b.httpClient
	if hc == nil {
		hc = &http.Client{Timeout: b.timeout}
	}
	return &Client{
		httpClient:  hc,
		baseURL:     b.baseURL,
		apiKey:      b.apiKey,
		maxRetries:  b.maxRetries,
		baseDelayMs: b.baseDelayMs,
	}
}

// ── Resource accessors ────────────────────────────────────────────────────────

// Transactions returns the transactions resource handle.
func (c *Client) Transactions() *TransactionsResource {
	return &TransactionsResource{client: c}
}

// Settlements returns the settlements resource handle.
func (c *Client) Settlements() *SettlementsResource {
	return &SettlementsResource{client: c}
}

// Stats returns the stats resource handle.
func (c *Client) Stats() *StatsResource {
	return &StatsResource{client: c}
}

// Health returns the health resource handle.
func (c *Client) Health() *HealthResource {
	return &HealthResource{client: c}
}

// GraphQL returns the GraphQL resource handle.
func (c *Client) GraphQL() *GraphQLResource {
	return &GraphQLResource{client: c}
}

// Events returns the events resource handle.
func (c *Client) Events() *EventsResource {
	return &EventsResource{client: c}
}

// ── Internal HTTP helpers ─────────────────────────────────────────────────────

type httpDoer interface {
	Do(*http.Request) (*http.Response, error)
}

// get performs an authenticated GET request and JSON-decodes the response.
func (c *Client) get(ctx context.Context, path string, query url.Values, out interface{}) error {
	return c.doRetry(ctx, http.MethodGet, path, query, nil, out)
}

// post performs an authenticated POST request with a JSON body.
func (c *Client) post(ctx context.Context, path string, body interface{}, out interface{}) error {
	return c.doOnce(ctx, http.MethodPost, path, nil, body, out)
}

// getBytes performs an authenticated GET and returns the raw response bytes.
func (c *Client) getBytes(ctx context.Context, path string, query url.Values) ([]byte, error) {
	req, err := c.buildRequest(ctx, http.MethodGet, path, query, nil, false)
	if err != nil {
		return nil, err
	}
	resp, err := c.httpClient.Do(req)
	if err != nil {
		return nil, &SynapseError{Message: err.Error()}
	}
	defer resp.Body.Close()

	raw, err := io.ReadAll(resp.Body)
	if err != nil {
		return nil, &SynapseError{Message: err.Error()}
	}
	if resp.StatusCode >= 400 {
		return nil, parseErrorBody(resp.StatusCode, raw)
	}
	return raw, nil
}

func (c *Client) doRetry(ctx context.Context, method, path string, query url.Values, body, out interface{}) error {
	var lastErr error
	delay := time.Duration(c.baseDelayMs) * time.Millisecond
	for attempt := 0; attempt < c.maxRetries; attempt++ {
		if attempt > 0 {
			select {
			case <-ctx.Done():
				return ctx.Err()
			case <-time.After(delay):
			}
			delay *= 2
		}
		lastErr = c.doOnce(ctx, method, path, query, body, out)
		if lastErr == nil {
			return nil
		}
		// Only retry on 5xx or network errors.
		se, ok := lastErr.(*SynapseError)
		if ok && se.StatusCode >= 400 && se.StatusCode < 500 {
			return lastErr
		}
	}
	return lastErr
}

func (c *Client) doOnce(ctx context.Context, method, path string, query url.Values, body, out interface{}) error {
	req, err := c.buildRequest(ctx, method, path, query, body, false)
	if err != nil {
		return err
	}
	resp, err := c.httpClient.Do(req)
	if err != nil {
		return &SynapseError{Message: err.Error()}
	}
	defer resp.Body.Close()

	raw, err := io.ReadAll(resp.Body)
	if err != nil {
		return &SynapseError{Message: err.Error()}
	}
	if resp.StatusCode >= 400 {
		return parseErrorBody(resp.StatusCode, raw)
	}
	if out != nil && len(raw) > 0 {
		if err := json.Unmarshal(raw, out); err != nil {
			return &SynapseError{Message: fmt.Sprintf("decode error: %v", err)}
		}
	}
	return nil
}

// buildRequest constructs an http.Request, serialising body as JSON when non-nil.
func (c *Client) buildRequest(ctx context.Context, method, path string, query url.Values, body interface{}, admin bool) (*http.Request, error) {
	u := c.baseURL + path
	if len(query) > 0 {
		u += "?" + query.Encode()
	}

	var bodyReader io.Reader
	if body != nil {
		b, err := json.Marshal(body)
		if err != nil {
			return nil, &SynapseError{Message: fmt.Sprintf("marshal error: %v", err)}
		}
		bodyReader = bytes.NewReader(b)
	}

	req, err := http.NewRequestWithContext(ctx, method, u, bodyReader)
	if err != nil {
		return nil, &SynapseError{Message: err.Error()}
	}
	if body != nil {
		req.Header.Set("Content-Type", "application/json")
	}
	if admin {
		req.Header.Set("Authorization", "Bearer "+c.apiKey)
	} else {
		req.Header.Set("X-API-Key", c.apiKey)
	}
	return req, nil
}

// execRequest executes a pre-built request, reading the body and decoding JSON.
func (c *Client) execRequest(req *http.Request, out interface{}) error {
	resp, err := c.httpClient.Do(req)
	if err != nil {
		return &SynapseError{Message: err.Error()}
	}
	defer resp.Body.Close()

	raw, err := io.ReadAll(resp.Body)
	if err != nil {
		return &SynapseError{Message: err.Error()}
	}
	if resp.StatusCode >= 400 {
		return parseErrorBody(resp.StatusCode, raw)
	}
	if out != nil && len(raw) > 0 {
		if err := json.Unmarshal(raw, out); err != nil {
			return &SynapseError{Message: fmt.Sprintf("decode error: %v", err)}
		}
	}
	return nil
}

func parseErrorBody(status int, body []byte) *SynapseError {
	var envelope struct {
		Error   string `json:"error"`
		Message string `json:"message"`
		Code    string `json:"code"`
	}
	_ = json.Unmarshal(body, &envelope)
	msg := envelope.Error
	if msg == "" {
		msg = envelope.Message
	}
	if msg == "" {
		msg = string(body)
	}
	return &SynapseError{
		StatusCode: status,
		Message:    msg,
		Code:       envelope.Code,
	}
}

func ptrString(s string) *string { return &s }
func ptrInt64(n int64) *int64    { return &n }

func addOptionalInt64(q url.Values, key string, v *int64) {
	if v != nil {
		q.Set(key, strconv.FormatInt(*v, 10))
	}
}

func addOptionalString(q url.Values, key string, v *string) {
	if v != nil {
		q.Set(key, *v)
	}
}
