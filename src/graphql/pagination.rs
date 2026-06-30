//! GraphQL Pagination Documentation and Implementation
//!
//! This module provides comprehensive pagination support for GraphQL queries.
//!
//! ## Overview
//!
//! Pagination is essential for efficiently handling large datasets in GraphQL APIs.
//! This implementation supports both cursor-based and offset-based pagination strategies.
//!
//! ## Pagination Strategies
//!
//! ### Offset-Based Pagination
//!
//! Offset-based pagination uses `limit` and `offset` parameters to navigate through results.
//!
//! **Advantages:**
//! - Simple to understand and implement
//! - Works well for small to medium datasets
//! - Supports random access to any page
//!
//! **Disadvantages:**
//! - Performance degrades with large offsets
//! - Inconsistent results if data changes between requests
//! - Not suitable for real-time data
//!
//! **Example Query:**
//! ```graphql
//! query {
//!   transactions(limit: 20, offset: 0) {
//!     id
//!     status
//!     amount
//!   }
//! }
//! ```
//!
//! ### Cursor-Based Pagination
//!
//! Cursor-based pagination uses opaque cursors to navigate through results.
//! This is the recommended approach for large datasets.
//!
//! **Advantages:**
//! - Consistent results regardless of data changes
//! - Efficient for large datasets
//! - Handles real-time data well
//! - Prevents duplicate or missing results
//!
//! **Disadvantages:**
//! - More complex to implement
//! - Cannot jump to arbitrary positions
//! - Requires stable sort order
//!
//! **Example Query:**
//! ```graphql
//! query {
//!   transactions(first: 20, after: "cursor123") {
//!     edges {
//!       node {
//!         id
//!         status
//!       }
//!       cursor
//!     }
//!     pageInfo {
//!       hasNextPage
//!       endCursor
//!     }
//!   }
//! }
//! ```
//!
//! ## Implementation Guidelines
//!
//! ### 1. Validation
//!
//! Always validate pagination parameters:
//! - `limit` should be between 1 and MAX_PAGE_SIZE (typically 100)
//! - `offset` should be non-negative
//! - Cursors should be properly formatted and validated
//!
//! ### 2. Performance Considerations
//!
//! - Use database indexes on sort columns
//! - Implement query result caching for frequently accessed pages
//! - Consider using keyset pagination for very large datasets
//! - Monitor query performance with slow query logs
//!
//! ### 3. Security
//!
//! - Validate all pagination parameters to prevent injection attacks
//! - Enforce maximum page size limits
//! - Implement rate limiting on pagination endpoints
//! - Ensure proper authorization checks on paginated data
//!
//! ### 4. Error Handling
//!
//! Handle these common pagination errors:
//! - Invalid cursor format
//! - Cursor pointing to deleted data
//! - Offset exceeding total results
//! - Invalid limit values
//!
//! ## Best Practices
//!
//! 1. **Always set a maximum page size** to prevent resource exhaustion
//! 2. **Use cursor-based pagination for large datasets** to ensure consistency
//! 3. **Document pagination behavior** in your schema
//! 4. **Test edge cases** like empty results, single item, and boundary conditions
//! 5. **Monitor pagination performance** in production
//! 6. **Provide clear error messages** when pagination parameters are invalid
//!
//! ## Example Implementation
//!
//! ```text
//! use async_graphql::{InputObject, Object, Result};
//!
//! #[derive(InputObject)]
//! pub struct PaginationInput {
//!     /// Maximum number of items to return (1-100)
//!     pub limit: Option<i64>,
//!     /// Number of items to skip (0-based)
//!     pub offset: Option<i64>,
//! }
//!
//! #[derive(Default)]
//! pub struct Query;
//!
//! #[Object]
//! impl Query {
//!     /// Fetch paginated transactions
//!     ///
//!     /// # Arguments
//!     /// * `pagination` - Pagination parameters (limit, offset)
//!     ///
//!     /// # Returns
//!     /// A vector of transactions for the requested page
//!     ///
//!     /// # Errors
//!     /// Returns error if pagination parameters are invalid
//!     async fn transactions(
//!         &self,
//!         pagination: Option<PaginationInput>,
//!     ) -> Result<Vec<Transaction>> {
//!         let limit = pagination
//!             .and_then(|p| p.limit)
//!             .unwrap_or(20)
//!             .min(100)
//!             .max(1);
//!
//!         let offset = pagination
//!             .and_then(|p| p.offset)
//!             .unwrap_or(0)
//!             .max(0);
//!
//!         // Fetch transactions with validated parameters
//!         Ok(vec![])
//!     }
//! }
//! ```
//!
//! ## Testing Pagination
//!
//! When testing pagination, verify:
//! - Correct number of items returned
//! - Proper ordering of results
//! - Correct `hasNextPage` indicator
//! - Cursor validity and consistency
//! - Edge cases (empty results, single item, boundary conditions)
//! - Performance with large datasets
//!
//! ## References
//!
//! - [GraphQL Cursor Connections Specification](https://relay.dev/graphql-cursor-connections-spec/)
//! - [Apollo GraphQL Pagination Guide](https://www.apollographql.com/docs/apollo-server/data/pagination/)
//! - [Offset vs Cursor Pagination](https://www.apollographql.com/docs/apollo-server/data/pagination/#offset-based)

pub mod cursor;
pub mod offset;

pub use cursor::CursorPagination;
pub use offset::OffsetPagination;

/// Maximum allowed page size
pub const MAX_PAGE_SIZE: i64 = 100;

/// Default page size
pub const DEFAULT_PAGE_SIZE: i64 = 20;
