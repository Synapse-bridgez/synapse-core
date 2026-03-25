# Implementation Plan: Stellar Memo Verification

## Overview

This implementation plan breaks down the Stellar memo verification feature into discrete coding tasks. The feature adds memo verification to prevent memo substitution attacks by comparing on-chain transaction memos with expected values before crediting funds. The implementation follows a bottom-up approach: first building the core memo verification module, then integrating it into the transaction processor, and finally adding security event logging and database support.

## Tasks

- [ ] 1. Create core memo module structure and types
  - Create `src/stellar/memo.rs` file
  - Define `MemoType` enum (Text, Id, Hash)
  - Define `MemoValue` enum with variants for Text(String), Id(u64), Hash([u8; 32]), and None
  - Implement `Display` trait for `MemoValue` with format "type:value"
  - Implement `memo_type_str()` method returning static string for each variant
  - Define `MemoMismatchError` and `MemoParseError` error types using thiserror
  - Add `pub mod memo;` to `src/stellar/mod.rs`
  - _Requirements: 3.1, 3.2_

- [ ] 2. Implement memo parsing functionality
  - [ ] 2.1 Implement `parse_memo` function in `MemoVerifier`
    - Accept `value: &str` and `memo_type: MemoType` parameters
    - Return `Result<MemoValue, MemoParseError>`
    - Handle empty string as `MemoValue::None`
    - Implement text memo parsing with 28-byte length validation
    - Implement id memo parsing with u64 conversion
    - Implement hash memo parsing with base64 decoding and 32-byte validation
    - _Requirements: 3.1, 5.1, 5.2_
  
  - [ ]* 2.2 Write unit tests for memo parsing
    - Test valid text memo parsing
    - Test text memo exceeding 28 bytes returns error
    - Test valid id memo parsing
    - Test invalid id memo (non-numeric) returns error
    - Test valid hash memo parsing from base64
    - Test invalid base64 returns error
    - Test hash with wrong length returns error
    - Test empty string returns MemoValue::None
    - _Requirements: 3.1, 5.1, 5.2_

- [ ] 3. Implement hash normalization
  - [ ] 3.1 Implement `normalize_hash` private function
    - Accept `hash: &[u8; 32]` parameter
    - Return normalized base64 string using STANDARD_NO_PAD encoding
    - Add `base64` crate dependency if not present
    - _Requirements: 2.4, 5.4_
  
  - [ ]* 3.2 Write unit tests for hash normalization
    - Test same hash always produces same normalized output
    - Test different padding variants normalize to same value
    - Test URL-safe vs standard base64 variants
    - _Requirements: 2.4, 5.4_
  
  - [ ]* 3.3 Write property test for hash normalization
    - **Property 2: Hash Encoding Normalization**
    - **Validates: Requirements 2.4, 5.4**
    - Generate random 32-byte arrays
    - Verify normalized representations are equal
    - _Requirements: 2.4, 5.4_

- [ ] 4. Implement core memo verification logic
  - [ ] 4.1 Implement `verify_memo` function in `MemoVerifier`
    - Accept `on_chain: &MemoValue` and `expected: &MemoValue` parameters
    - Return `Result<(), MemoMismatchError>`
    - Implement None-None comparison (success)
    - Implement Text-Text comparison with string equality
    - Implement Id-Id comparison with numeric equality
    - Implement Hash-Hash comparison with normalization
    - Implement type mismatch detection (always fail)
    - _Requirements: 1.1, 1.2, 1.3, 2.1, 2.2, 2.3, 2.4, 3.1, 3.2, 3.3_
  
  - [ ]* 4.2 Write unit tests for memo verification
    - Test matching text memos succeed
    - Test mismatched text memos fail
    - Test matching id memos succeed
    - Test mismatched id memos fail
    - Test matching hash memos succeed
    - Test mismatched hash memos fail
    - Test both None memos succeed
    - Test type mismatch fails
    - Test special characters in text memos
    - _Requirements: 1.1, 1.2, 1.3, 2.1, 2.2, 2.3, 3.3, 5.3_
  
  - [ ]* 4.3 Write property test for memo identity
    - **Property 1: Memo Identity**
    - **Validates: Requirements 2.1, 2.2, 2.3**
    - Generate arbitrary memo values
    - Verify each memo against itself always succeeds
    - _Requirements: 2.1, 2.2, 2.3_
  
  - [ ]* 4.4 Write property test for memo mismatch detection
    - **Property 3: Memo Mismatch Detection**
    - **Validates: Requirements 1.3**
    - Generate pairs of distinct memo values of same type
    - Verify verification always fails
    - _Requirements: 1.3_
  
  - [ ]* 4.5 Write property test for type mismatch detection
    - **Property 4: Type Mismatch Detection**
    - **Validates: Requirements 1.3**
    - Generate pairs of memo values with different types
    - Verify verification always fails
    - _Requirements: 1.3_
  
  - [ ]* 4.6 Write property test for empty memo equivalence
    - **Property 7: Empty Memo Equivalence**
    - **Validates: Requirements 3.4, 5.2**
    - Verify MemoValue::None against MemoValue::None always succeeds
    - _Requirements: 3.4, 5.2_

- [ ] 5. Checkpoint - Ensure core memo verification tests pass
  - Ensure all tests pass, ask the user if questions arise.

- [ ] 6. Create database migration for security events
  - Create new migration file in `migrations/` directory
  - Add `CREATE TABLE memo_security_events` with columns: id (UUID), transaction_id (UUID FK), on_chain_memo (TEXT), expected_memo (TEXT), memo_type (VARCHAR), created_at (TIMESTAMP)
  - Add index on transaction_id
  - Add index on created_at
  - _Requirements: 4.1, 4.2, 4.3, 4.4, 4.5, 4.6_

- [ ] 7. Extend Transaction model for memo fields
  - Add `expected_memo: Option<String>` field to Transaction struct
  - Add `expected_memo_type: Option<String>` field to Transaction struct
  - Update any existing queries or builders to include new fields
  - _Requirements: 1.1, 3.1_

- [ ] 8. Implement security event logging
  - [ ] 8.1 Add `log_security_event` method to `TransactionProcessor`
    - Accept transaction_id, on_chain memo, expected memo parameters
    - Insert record into memo_security_events table using sqlx
    - Log warning message with transaction ID and memo values
    - Return `Result<(), AppError>`
    - _Requirements: 4.1, 4.2, 4.3, 4.4, 4.5, 4.6_
  
  - [ ]* 8.2 Write unit test for security event logging
    - Test security event is inserted with all required fields
    - Test timestamp is automatically set
    - Verify warning log is emitted
    - _Requirements: 4.1, 4.2, 4.3, 4.4, 4.5, 4.6_
  
  - [ ]* 8.3 Write property test for security event completeness
    - **Property 5: Security Event Completeness**
    - **Validates: Requirements 4.2, 4.3, 4.4, 4.5, 4.6**
    - Generate arbitrary memo mismatch scenarios
    - Verify logged events contain all required fields
    - _Requirements: 4.2, 4.3, 4.4, 4.5, 4.6_

- [ ] 9. Implement memo mismatch handler
  - [ ] 9.1 Add `handle_memo_mismatch` method to `TransactionProcessor`
    - Accept transaction_id, on_chain memo, expected memo, error parameters
    - Call `log_security_event` to record the mismatch
    - Call `move_to_dlq` with "Memo mismatch" reason
    - Return `Result<(), AppError>`
    - _Requirements: 1.3, 1.4, 4.1_
  
  - [ ]* 9.2 Write unit test for memo mismatch handler
    - Test security event is logged
    - Test transaction is moved to DLQ
    - Test error is returned
    - _Requirements: 1.3, 1.4, 4.1_

- [ ] 10. Integrate memo verification into TransactionProcessor
  - [ ] 10.1 Modify `try_process` method to add verification step
    - After fetching on-chain transaction, check if expected_memo exists
    - If expected_memo exists, parse both on-chain and expected memos
    - Call `MemoVerifier::verify_memo` with parsed values
    - On success, log info message and continue processing
    - On failure, call `handle_memo_mismatch` and return validation error
    - Ensure verification happens before `complete_transaction` call
    - _Requirements: 1.1, 1.2, 1.3, 6.1, 6.2, 6.3_
  
  - [ ]* 10.2 Write integration test for successful verification
    - Test transaction with matching memo completes successfully
    - Test transaction without memo continues normal processing
    - _Requirements: 1.2, 6.3_
  
  - [ ]* 10.3 Write integration test for failed verification
    - Test transaction with mismatched memo is rejected
    - Test security event is created
    - Test transaction is moved to DLQ
    - Test processing halts before fund crediting
    - _Requirements: 1.3, 1.4, 6.2_

- [ ] 11. Add Horizon transaction response parsing
  - Define `HorizonTransaction` struct with id, hash, memo, memo_type fields
  - Implement deserialization from Horizon API JSON response
  - Add helper method to convert Horizon memo fields to `MemoValue`
  - Update `fetch_on_chain_transaction` to return parsed memo
  - _Requirements: 1.1, 2.1, 2.2, 2.3_

- [ ] 12. Add proptest generators for property tests
  - [ ] 12.1 Implement `any_memo_value` strategy
    - Use `prop_oneof!` to generate any MemoValue variant
    - Include Text, Id, Hash, and None variants
    - _Requirements: Testing infrastructure_
  
  - [ ] 12.2 Implement `any_text_memo` strategy
    - Generate strings up to 28 bytes with printable ASCII characters
    - Use regex pattern `[\x20-\x7E]{0,28}`
    - _Requirements: 5.3_
  
  - [ ] 12.3 Implement `any_id_memo` strategy
    - Generate arbitrary u64 values
    - _Requirements: Testing infrastructure_
  
  - [ ] 12.4 Implement `any_hash_memo` strategy
    - Generate uniform 32-byte arrays
    - _Requirements: Testing infrastructure_
  
  - [ ]* 12.5 Write property test for parse-verify round trip
    - **Property 6: Parse-Verify Round Trip**
    - **Validates: Requirements 3.1, 3.2**
    - Generate valid memo strings and types
    - Parse then verify against original
    - _Requirements: 3.1, 3.2_

- [ ] 13. Final checkpoint - Run all tests and verify integration
  - Ensure all tests pass, ask the user if questions arise.

## Notes

- Tasks marked with `*` are optional and can be skipped for faster MVP
- Each task references specific requirements for traceability
- Property tests use proptest library with minimum 100 iterations
- All property tests are tagged with format: `// Feature: stellar-memo-verification, Property {number}: {property_text}`
- Core verification logic (tasks 1-5) should be completed before integration (tasks 6-11)
- Database migration (task 6) must be run before testing integration
- Checkpoints ensure incremental validation at key milestones
