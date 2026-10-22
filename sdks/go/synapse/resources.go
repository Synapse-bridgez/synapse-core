package synapse

import (
	"context"
	"fmt"
	"net/url"
	"strconv"
)

// ── Transactions ──────────────────────────────────────────────────────────────

// TransactionsResource provides access to the /transactions endpoints.
type TransactionsResource struct {
	client *Client
}

// Get fetches a single transaction by ID.
// Returns *SynapseError with StatusCode 404 when the transaction does not exist.
func (r *TransactionsResource) Get(ctx context.Context, id string) (*Transaction, error) {
	var tx Transaction
	if err := r.client.get(ctx, "/transactions/"+id, nil, &tx); err != nil {
		return nil, err
	}
	return &tx, nil
}

// List fetches a page of transactions with optional cursor and date filters.
func (r *TransactionsResource) List(ctx context.Context, params ListParams) (*TransactionList, error) {
	q := url.Values{}
	addOptionalString(q, "cursor", params.Cursor)
	addOptionalInt64(q, "limit", params.Limit)
	addOptionalString(q, "from_date", params.FromDate)
	addOptionalString(q, "to_date", params.ToDate)

	var page TransactionList
	if err := r.client.get(ctx, "/transactions", q, &page); err != nil {
		return nil, err
	}
	return &page, nil
}

// Search filters transactions by the given criteria.
// A search returning zero matches is not an error.
func (r *TransactionsResource) Search(ctx context.Context, p SearchParams) (*TransactionSearch, error) {
	q := url.Values{}
	addOptionalString(q, "status", p.Status)
	addOptionalString(q, "asset_code", p.AssetCode)
	addOptionalString(q, "min_amount", p.MinAmount)
	addOptionalString(q, "max_amount", p.MaxAmount)
	addOptionalString(q, "from", p.From)
	addOptionalString(q, "to", p.To)
	addOptionalString(q, "stellar_account", p.StellarAccount)
	addOptionalString(q, "cursor", p.Cursor)
	addOptionalInt64(q, "limit", p.Limit)

	var page TransactionSearch
	if err := r.client.get(ctx, "/transactions/search", q, &page); err != nil {
		return nil, err
	}
	return &page, nil
}

// Export downloads raw transaction data (CSV or JSON bytes).
func (r *TransactionsResource) Export(ctx context.Context, f TransactionExportFilters) ([]byte, error) {
	q := url.Values{}
	addOptionalString(q, "format", f.Format)
	addOptionalString(q, "from", f.From)
	addOptionalString(q, "to", f.To)
	addOptionalString(q, "status", f.Status)
	addOptionalString(q, "asset_code", f.AssetCode)
	return r.client.getBytes(ctx, "/export", q)
}

// ── Settlements ───────────────────────────────────────────────────────────────

// SettlementsResource provides access to the /settlements endpoints.
type SettlementsResource struct {
	client *Client
}

// Get fetches a single settlement by ID.
func (r *SettlementsResource) Get(ctx context.Context, id string) (*Settlement, error) {
	var s Settlement
	if err := r.client.get(ctx, "/settlements/"+id, nil, &s); err != nil {
		return nil, err
	}
	return &s, nil
}

// List fetches a page of settlements with optional cursor and direction.
func (r *SettlementsResource) List(ctx context.Context, params SettlementParams) (*SettlementList, error) {
	q := url.Values{}
	addOptionalString(q, "cursor", params.Cursor)
	addOptionalInt64(q, "limit", params.Limit)
	addOptionalString(q, "direction", params.Direction)

	var page SettlementList
	if err := r.client.get(ctx, "/settlements", q, &page); err != nil {
		return nil, err
	}
	return &page, nil
}

// ── Stats ─────────────────────────────────────────────────────────────────────

// StatsResource provides access to the /stats/* and /cache/metrics endpoints.
type StatsResource struct {
	client *Client
}

// Status returns per-status transaction counts.
func (r *StatsResource) Status(ctx context.Context) ([]StatusCount, error) {
	var counts []StatusCount
	if err := r.client.get(ctx, "/stats/status", nil, &counts); err != nil {
		return nil, err
	}
	return counts, nil
}

// Daily returns per-day transaction volumes.
func (r *StatsResource) Daily(ctx context.Context, params DailyParams) ([]DailyTotal, error) {
	q := url.Values{}
	if params.Days != nil {
		q.Set("days", strconv.Itoa(int(*params.Days)))
	}
	var totals []DailyTotal
	if err := r.client.get(ctx, "/stats/daily", q, &totals); err != nil {
		return nil, err
	}
	return totals, nil
}

// Assets returns per-asset transaction statistics.
func (r *StatsResource) Assets(ctx context.Context) ([]AssetStats, error) {
	var stats []AssetStats
	if err := r.client.get(ctx, "/stats/assets", nil, &stats); err != nil {
		return nil, err
	}
	return stats, nil
}

// CacheMetrics returns combined cache metrics from /cache/metrics.
func (r *StatsResource) CacheMetrics(ctx context.Context) (*CombinedCacheMetrics, error) {
	var m CombinedCacheMetrics
	if err := r.client.get(ctx, "/cache/metrics", nil, &m); err != nil {
		return nil, err
	}
	return &m, nil
}

// ── Health ────────────────────────────────────────────────────────────────────

// HealthResource provides access to the /health endpoint.
type HealthResource struct {
	client *Client
}

// HealthResponse is the shape of the /health response.
type HealthResponse struct {
	Status  string                 `json:"status"`
	Version string                 `json:"version,omitempty"`
	Checks  map[string]interface{} `json:"checks,omitempty"`
}

// Check calls GET /health and returns the raw health response.
func (r *HealthResource) Check(ctx context.Context) (*HealthResponse, error) {
	var h HealthResponse
	if err := r.client.get(ctx, "/health", nil, &h); err != nil {
		return nil, err
	}
	return &h, nil
}

// ── GraphQL ───────────────────────────────────────────────────────────────────

// GraphQLResource provides access to the /graphql endpoint.
type GraphQLResource struct {
	client *Client
}

// Query issues a raw GraphQL query and returns the response.
// Check GraphQLResponse.Errors for application-level GraphQL errors even when
// err is nil (GraphQL returns 200 with errors in the payload).
func (r *GraphQLResource) Query(ctx context.Context, query string, variables interface{}) (*GraphQLResponse, error) {
	req := GraphQLRequest{Query: query, Variables: variables}
	var resp GraphQLResponse
	if err := r.client.post(ctx, "/graphql", req, &resp); err != nil {
		return nil, err
	}
	return &resp, nil
}

// ── Events ────────────────────────────────────────────────────────────────────

// EventsResource provides access to the /events, /reconnect, and
// /reconnect/status endpoints.
type EventsResource struct {
	client *Client
}

// Event is a single transaction status-change event.
type Event struct {
	TransactionID string  `json:"transaction_id"`
	Status        string  `json:"status"`
	Timestamp     string  `json:"timestamp"`
	Message       *string `json:"message"`
}

// Watch fetches recent transaction status-change events from GET /events.
func (r *EventsResource) Watch(ctx context.Context) ([]Event, error) {
	var events []Event
	if err := r.client.get(ctx, "/events", nil, &events); err != nil {
		return nil, err
	}
	return events, nil
}

// Reconnect attempts to reconnect a WebSocket session (POST /reconnect).
func (r *EventsResource) Reconnect(ctx context.Context, sessionID string) (*ReconnectResponse, error) {
	body := map[string]string{"session_id": sessionID}
	var resp ReconnectResponse
	if err := r.client.post(ctx, "/reconnect", body, &resp); err != nil {
		return nil, err
	}
	return &resp, nil
}

// ReconnectStatus checks the reconnect status without committing an attempt.
func (r *EventsResource) ReconnectStatus(ctx context.Context, token *string) (*ReconnectResponse, error) {
	q := url.Values{}
	if token != nil {
		q.Set("token", *token)
	}
	var resp ReconnectResponse
	if err := r.client.get(ctx, "/reconnect/status", q, &resp); err != nil {
		return nil, err
	}
	return &resp, nil
}

// ── Admin: Locks ──────────────────────────────────────────────────────────────

// AdminLocksResource provides access to the /admin/locks endpoints.
type AdminLocksResource struct {
	admin *AdminClient
}

// List returns all currently held distributed locks.
func (r *AdminLocksResource) List(ctx context.Context) (*LocksListResponse, error) {
	var resp LocksListResponse
	if err := r.admin.get(ctx, "/admin/locks", nil, &resp); err != nil {
		return nil, err
	}
	return &resp, nil
}

// ForceRelease forcibly releases a lock regardless of its current owner.
func (r *AdminLocksResource) ForceRelease(ctx context.Context, resource string) (*ForceReleaseLockResponse, error) {
	var resp ForceReleaseLockResponse
	if err := r.admin.post(ctx, fmt.Sprintf("/admin/locks/%s/force-release", resource), map[string]interface{}{}, &resp); err != nil {
		return nil, err
	}
	return &resp, nil
}

// ── Admin: Reconciliation ─────────────────────────────────────────────────────

// AdminReconciliationResource provides access to the /admin/reconciliation endpoints.
type AdminReconciliationResource struct {
	admin *AdminClient
}

// ListReports returns a paginated list of reconciliation reports.
func (r *AdminReconciliationResource) ListReports(ctx context.Context, params ListReportsParams) (*ListReconciliationReports, error) {
	q := url.Values{}
	if params.Limit != nil {
		q.Set("limit", strconv.Itoa(int(*params.Limit)))
	}
	if params.Offset != nil {
		q.Set("offset", strconv.Itoa(int(*params.Offset)))
	}
	var resp ListReconciliationReports
	if err := r.admin.get(ctx, "/admin/reconciliation/reports", q, &resp); err != nil {
		return nil, err
	}
	return &resp, nil
}

// Run triggers a reconciliation pass for the given account.
func (r *AdminReconciliationResource) Run(ctx context.Context, req RunReconciliationRequest) (*RunReconciliationResponse, error) {
	var resp RunReconciliationResponse
	if err := r.admin.post(ctx, "/admin/reconciliation/run", req, &resp); err != nil {
		return nil, err
	}
	return &resp, nil
}

// ── Admin: Settlements ────────────────────────────────────────────────────────

// AdminSettlementsResource provides access to the /admin/settlements endpoints.
type AdminSettlementsResource struct {
	admin *AdminClient
}

// UpdateStatus updates a settlement's status.
func (r *AdminSettlementsResource) UpdateStatus(ctx context.Context, settlementID string, req UpdateSettlementStatusRequest) (*AdminSettlement, error) {
	var resp AdminSettlement
	path := fmt.Sprintf("/admin/settlements/%s/status", settlementID)
	if err := r.admin.patch(ctx, path, req, &resp); err != nil {
		return nil, err
	}
	return &resp, nil
}

// ── Admin: Bulk Status ────────────────────────────────────────────────────────

// AdminBulkStatusResource provides access to the /admin/transactions/bulk-status endpoint.
type AdminBulkStatusResource struct {
	admin *AdminClient
}

// Update bulk-updates the status of multiple transactions.
func (r *AdminBulkStatusResource) Update(ctx context.Context, ids []string, status string) (*BulkStatusResponse, error) {
	body := map[string]interface{}{"transaction_ids": ids, "status": status}
	var resp BulkStatusResponse
	if err := r.admin.patch(ctx, "/admin/transactions/bulk-status", body, &resp); err != nil {
		return nil, err
	}
	return &resp, nil
}

// ── Admin: DLQ ────────────────────────────────────────────────────────────────

// AdminDlqResource provides access to the /dlq admin endpoints.
type AdminDlqResource struct {
	admin *AdminClient
}

// List returns dead-letter queue entries.
func (r *AdminDlqResource) List(ctx context.Context) (*DlqListResponse, error) {
	var resp DlqListResponse
	if err := r.admin.get(ctx, "/dlq", nil, &resp); err != nil {
		return nil, err
	}
	return &resp, nil
}

// Requeue re-enqueues a DLQ entry for processing.
func (r *AdminDlqResource) Requeue(ctx context.Context, id string) (*RequeueResponse, error) {
	var resp RequeueResponse
	if err := r.admin.post(ctx, fmt.Sprintf("/dlq/%s/requeue", id), map[string]interface{}{}, &resp); err != nil {
		return nil, err
	}
	return &resp, nil
}

// ── Admin: Webhook Replay ─────────────────────────────────────────────────────

// AdminWebhookReplayResource provides access to the /admin/webhooks replay endpoints.
type AdminWebhookReplayResource struct {
	admin *AdminClient
}

// ListFailed returns failed webhook deliveries.
func (r *AdminWebhookReplayResource) ListFailed(ctx context.Context, filters ListFailedWebhooksFilters) (*FailedWebhooksResponse, error) {
	q := url.Values{}
	addOptionalInt64(q, "limit", filters.Limit)
	addOptionalInt64(q, "offset", filters.Offset)
	addOptionalString(q, "asset_code", filters.AssetCode)
	addOptionalString(q, "from_date", filters.FromDate)
	addOptionalString(q, "to_date", filters.ToDate)

	var resp FailedWebhooksResponse
	if err := r.admin.get(ctx, "/admin/webhooks/failed", q, &resp); err != nil {
		return nil, err
	}
	return &resp, nil
}

// Replay replays a single failed webhook delivery.
func (r *AdminWebhookReplayResource) Replay(ctx context.Context, transactionID string, dryRun bool) (*WebhookReplayResult, error) {
	body := map[string]interface{}{"dry_run": dryRun}
	var resp WebhookReplayResult
	if err := r.admin.post(ctx, fmt.Sprintf("/admin/webhooks/%s/replay", transactionID), body, &resp); err != nil {
		return nil, err
	}
	return &resp, nil
}

// ReplayBatch replays multiple failed webhook deliveries in a single request.
func (r *AdminWebhookReplayResource) ReplayBatch(ctx context.Context, transactionIDs []string, dryRun bool) (*BatchWebhookReplayResponse, error) {
	body := map[string]interface{}{"transaction_ids": transactionIDs, "dry_run": dryRun}
	var resp BatchWebhookReplayResponse
	if err := r.admin.post(ctx, "/admin/webhooks/replay/batch", body, &resp); err != nil {
		return nil, err
	}
	return &resp, nil
}
