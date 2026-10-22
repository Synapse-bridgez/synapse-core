// Package synapse provides a Go client SDK for the Synapse API with full
// parity to the Rust SDK (sdks/rust).
//
// # Quick start
//
//	client := synapse.NewClient("https://api.example.com", "your-api-key")
//	tx, err := client.Transactions().Get(ctx, "550e8400-e29b-41d4-a716-446655440000")
//
// Admin routes use Authorization: Bearer <key> — use NewAdminClient:
//
//	admin := synapse.NewAdminClient("https://api.example.com", "admin-key")
//	locks, err := admin.Locks().List(ctx)
package synapse

import "time"

// Transaction represents a single Synapse fiat-to-Stellar deposit transaction.
type Transaction struct {
	ID                  string      `json:"id"`
	StellarAccount      string      `json:"stellar_account"`
	Amount              string      `json:"amount"`
	AssetCode           string      `json:"asset_code"`
	Status              string      `json:"status"`
	CreatedAt           time.Time   `json:"created_at"`
	UpdatedAt           time.Time   `json:"updated_at"`
	AnchorTransactionID *string     `json:"anchor_transaction_id"`
	CallbackType        *string     `json:"callback_type"`
	CallbackStatus      *string     `json:"callback_status"`
	SettlementID        *string     `json:"settlement_id"`
	Memo                *string     `json:"memo"`
	MemoType            *string     `json:"memo_type"`
	Metadata            interface{} `json:"metadata"`
}

// ListMeta contains pagination metadata included in list responses.
type ListMeta struct {
	NextCursor *string `json:"next_cursor"`
	HasMore    bool    `json:"has_more"`
}

// TransactionList is a paginated list of transactions.
type TransactionList struct {
	Data []Transaction `json:"data"`
	Meta ListMeta      `json:"meta"`
}

// TransactionSearch is a single page of search results.
type TransactionSearch struct {
	Total      int64         `json:"total"`
	Results    []Transaction `json:"results"`
	NextCursor *string       `json:"next_cursor"`
}

// ListParams contains optional pagination and date-filter parameters.
type ListParams struct {
	Cursor   *string
	Limit    *int64
	FromDate *string
	ToDate   *string
}

// SearchParams contains filter criteria for transaction search.
type SearchParams struct {
	Status         *string
	AssetCode      *string
	MinAmount      *string
	MaxAmount      *string
	From           *string
	To             *string
	StellarAccount *string
	Cursor         *string
	Limit          *int64
}

// TransactionExportFilters specifies filters for the export endpoint.
type TransactionExportFilters struct {
	Format    *string // "csv" or "json"
	From      *string
	To        *string
	Status    *string
	AssetCode *string
}

// Settlement represents a single settlement record.
type Settlement struct {
	ID                  string     `json:"id"`
	AssetCode           string     `json:"asset_code"`
	TotalAmount         string     `json:"total_amount"`
	TxCount             int32      `json:"tx_count"`
	PeriodStart         time.Time  `json:"period_start"`
	PeriodEnd           time.Time  `json:"period_end"`
	Status              string     `json:"status"`
	CreatedAt           time.Time  `json:"created_at"`
	UpdatedAt           time.Time  `json:"updated_at"`
	DisputeReason       *string    `json:"dispute_reason"`
	OriginalTotalAmount *string    `json:"original_total_amount"`
	ReviewedBy          *string    `json:"reviewed_by"`
	ReviewedAt          *time.Time `json:"reviewed_at"`
}

// SettlementList is a paginated list of settlements.
type SettlementList struct {
	Settlements []Settlement `json:"settlements"`
	NextCursor  *string      `json:"next_cursor"`
	HasMore     bool         `json:"has_more"`
}

// SettlementParams contains optional pagination parameters for listings.
type SettlementParams struct {
	Cursor    *string
	Limit     *int64
	Direction *string
}

// StatusCount represents the count of transactions in a particular status.
type StatusCount struct {
	Status string `json:"status"`
	Count  int64  `json:"count"`
}

// DailyTotal represents per-day transaction volume.
type DailyTotal struct {
	Date        string `json:"date"`
	Count       int64  `json:"count"`
	TotalAmount string `json:"total_amount"`
}

// AssetStats represents per-asset transaction statistics.
type AssetStats struct {
	AssetCode   string `json:"asset_code"`
	Count       int64  `json:"count"`
	TotalAmount string `json:"total_amount"`
}

// DailyParams specifies the number of days to include in /stats/daily.
type DailyParams struct {
	Days *int32
}

// QueryCacheMetrics contains metrics for the in-process LRU query-result cache.
type QueryCacheMetrics struct {
	Hits      uint64  `json:"hits"`
	Misses    uint64  `json:"misses"`
	HitRate   float64 `json:"hit_rate"`
	Evictions uint64  `json:"evictions"`
	Size      uint64  `json:"size"`
	Capacity  uint64  `json:"capacity"`
}

// CombinedCacheMetrics contains combined cache metrics from GET /cache/metrics.
type CombinedCacheMetrics struct {
	QueryCache                QueryCacheMetrics `json:"query_cache"`
	IdempotencyCacheHits      uint64            `json:"idempotency_cache_hits"`
	IdempotencyCacheMisses    uint64            `json:"idempotency_cache_misses"`
	IdempotencyLockAcquired   uint64            `json:"idempotency_lock_acquired"`
	IdempotencyLockContention uint64            `json:"idempotency_lock_contention"`
	IdempotencyErrors         uint64            `json:"idempotency_errors"`
	IdempotencyFallbackCount  uint64            `json:"idempotency_fallback_count"`
}

// GraphQLRequest is the request body for POST /graphql.
type GraphQLRequest struct {
	Query     string      `json:"query"`
	Variables interface{} `json:"variables,omitempty"`
}

// GraphQLError is a single application-level error in a GraphQL 200 response.
type GraphQLError struct {
	Message string `json:"message"`
}

// GraphQLResponse is the response from POST /graphql.
type GraphQLResponse struct {
	Data   interface{}    `json:"data"`
	Errors []GraphQLError `json:"errors,omitempty"`
}

// ReconnectResponse is the response from POST /reconnect or GET /reconnect/status.
type ReconnectResponse struct {
	Kind           string      `json:"type"`
	Status         interface{} `json:"status"`
	BackoffSeconds *uint64     `json:"backoff_seconds"`
	RequiresResync *bool       `json:"requires_resync"`
	Message        *string     `json:"message"`
}

// ActiveLock represents a single distributed lock currently held by the server.
type ActiveLock struct {
	Resource             string `json:"resource"`
	Token                string `json:"token"`
	AcquiredAt           uint64 `json:"acquired_at"`
	TtlSecs              uint64 `json:"ttl_secs"`
	ExpectedDurationSecs uint64 `json:"expected_duration_secs"`
	Overdue              bool   `json:"overdue"`
}

// LocksListResponse is the response from GET /admin/locks.
type LocksListResponse struct {
	ActiveLocks []ActiveLock `json:"active_locks"`
	Total       int64        `json:"total"`
	Overdue     int64        `json:"overdue"`
}

// ForceReleaseLockResponse is the response from POST /admin/locks/:resource/force-release.
type ForceReleaseLockResponse struct {
	Resource string `json:"resource"`
	Released bool   `json:"released"`
	Message  string `json:"message"`
}

// ReconciliationReportSummary is a summary of a reconciliation report.
type ReconciliationReportSummary struct {
	ID                    string    `json:"id"`
	GeneratedAt           time.Time `json:"generated_at"`
	PeriodStart           time.Time `json:"period_start"`
	PeriodEnd             time.Time `json:"period_end"`
	TotalDbTransactions   int32     `json:"total_db_transactions"`
	TotalChainPayments    int32     `json:"total_chain_payments"`
	MissingOnChainCount   int32     `json:"missing_on_chain_count"`
	OrphanedPaymentsCount int32     `json:"orphaned_payments_count"`
	AmountMismatchesCount int32     `json:"amount_mismatches_count"`
	HasDiscrepancies      bool      `json:"has_discrepancies"`
}

// ListReconciliationReports is the paginated list of reconciliation reports.
type ListReconciliationReports struct {
	Reports []ReconciliationReportSummary `json:"reports"`
	Total   int64                         `json:"total"`
	Limit   int32                         `json:"limit"`
	Offset  int32                         `json:"offset"`
}

// ListReportsParams contains optional pagination parameters for listing reports.
type ListReportsParams struct {
	Limit  *int32
	Offset *int32
}

// RunReconciliationRequest is the request body for running a reconciliation.
type RunReconciliationRequest struct {
	Account     string `json:"account"`
	PeriodHours *int32 `json:"period_hours,omitempty"`
}

// RunReconciliationResponse is the response from running a reconciliation.
type RunReconciliationResponse struct {
	Message string                      `json:"message"`
	Report  ReconciliationReportSummary `json:"report"`
}

// AdminSettlement is a settlement as returned by the admin status-update endpoint.
type AdminSettlement struct {
	ID          string    `json:"id"`
	Status      string    `json:"status"`
	TotalAmount string    `json:"total_amount"`
	Reason      *string   `json:"reason"`
	Actor       *string   `json:"actor"`
	CreatedAt   time.Time `json:"created_at"`
	UpdatedAt   time.Time `json:"updated_at"`
}

// UpdateSettlementStatusRequest is the request body for updating settlement status.
type UpdateSettlementStatusRequest struct {
	Status   string  `json:"status"`
	Reason   *string `json:"reason,omitempty"`
	NewTotal *string `json:"new_total,omitempty"`
	Actor    *string `json:"actor,omitempty"`
}

// BulkUpdateError is a per-item failure from a bulk status update.
type BulkUpdateError struct {
	ID    string `json:"id"`
	Error string `json:"error"`
}

// BulkStatusResponse is the response from PATCH /admin/transactions/bulk-status.
type BulkStatusResponse struct {
	Updated int               `json:"updated"`
	Failed  int               `json:"failed"`
	Errors  []BulkUpdateError `json:"errors"`
}

// DlqEntry is a single dead-letter queue entry.
type DlqEntry struct {
	ID                  string     `json:"id"`
	TransactionID       string     `json:"transaction_id"`
	StellarAccount      string     `json:"stellar_account"`
	Amount              string     `json:"amount"`
	AssetCode           string     `json:"asset_code"`
	AnchorTransactionID *string    `json:"anchor_transaction_id"`
	ErrorReason         string     `json:"error_reason"`
	StackTrace          *string    `json:"stack_trace"`
	RetryCount          int32      `json:"retry_count"`
	OriginalCreatedAt   time.Time  `json:"original_created_at"`
	MovedToDlqAt        time.Time  `json:"moved_to_dlq_at"`
	LastRetryAt         *time.Time `json:"last_retry_at"`
}

// DlqListResponse is the response from GET /dlq.
type DlqListResponse struct {
	DlqEntries []DlqEntry `json:"dlq_entries"`
	Count      int        `json:"count"`
}

// RequeueResponse is the response from POST /dlq/:id/requeue.
type RequeueResponse struct {
	Message string `json:"message"`
	DlqID   string `json:"dlq_id"`
}

// FailedWebhookInfo is a single failed webhook delivery from the audit log.
type FailedWebhookInfo struct {
	TransactionID       string    `json:"transaction_id"`
	StellarAccount      string    `json:"stellar_account"`
	Amount              string    `json:"amount"`
	AssetCode           string    `json:"asset_code"`
	AnchorTransactionID *string   `json:"anchor_transaction_id"`
	Status              string    `json:"status"`
	CreatedAt           time.Time `json:"created_at"`
	LastError           *string   `json:"last_error"`
	RetryCount          int32     `json:"retry_count"`
}

// FailedWebhooksResponse is the response from GET /admin/webhooks/failed.
type FailedWebhooksResponse struct {
	Total    int64               `json:"total"`
	Webhooks []FailedWebhookInfo `json:"webhooks"`
}

// WebhookReplayResult is the result of a single webhook replay attempt.
type WebhookReplayResult struct {
	TransactionID string     `json:"transaction_id"`
	Success       bool       `json:"success"`
	Message       string     `json:"message"`
	DryRun        bool       `json:"dry_run"`
	ReplayedAt    *time.Time `json:"replayed_at"`
}

// BatchWebhookReplayResponse is the response from POST /admin/webhooks/replay/batch.
type BatchWebhookReplayResponse struct {
	Total      int                   `json:"total"`
	Successful int                   `json:"successful"`
	Failed     int                   `json:"failed"`
	Results    []WebhookReplayResult `json:"results"`
}

// ListFailedWebhooksFilters contains optional filters for listing failed webhooks.
type ListFailedWebhooksFilters struct {
	Limit     *int64
	Offset    *int64
	AssetCode *string
	FromDate  *string
	ToDate    *string
}
