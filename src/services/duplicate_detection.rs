use serde::{Deserialize, Serialize};
use std::time::{Duration, SystemTime};
use tracing::{error, info, warn};

/// Configuration for duplicate transaction detection
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DuplicateDetectionConfig {
    /// Time window in seconds to look back for potential duplicates
    pub detection_window_secs: u64,
    /// Maximum amount difference (as absolute value) to consider same
    pub amount_tolerance: f64,
    /// Enable duplicate detection
    pub enabled: bool,
}

impl Default for DuplicateDetectionConfig {
    fn default() -> Self {
        Self {
            detection_window_secs: 3600,  // 1 hour
            amount_tolerance: 0.01,       // 0.01 units difference
            enabled: true,
        }
    }
}

/// Metadata about a detected duplicate
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct DuplicateIndicator {
    /// ID of the potentially duplicate transaction
    pub duplicate_transaction_id: String,
    /// Timestamp of the potentially duplicate transaction (as Unix timestamp)
    pub duplicate_timestamp: u64,
    /// Confidence score (0.0 to 1.0) - higher means more likely to be a duplicate
    pub confidence_score: f64,
    /// Human-readable reason for duplicate flag
    pub reason: String,
}

/// Transaction data needed for duplicate detection
#[derive(Clone, Debug)]
pub struct TransactionSnapshot {
    pub id: String,
    pub source_account: String,
    pub amount: f64,
    pub asset: String,
    pub timestamp: u64, // Unix timestamp in seconds
}

/// Detects likely-duplicate transactions based on heuristic matching
pub struct DuplicateDetector {
    config: DuplicateDetectionConfig,
}

impl DuplicateDetector {
    pub fn new(config: DuplicateDetectionConfig) -> Self {
        if config.enabled {
            info!(
                window_secs = config.detection_window_secs,
                amount_tolerance = config.amount_tolerance,
                "Duplicate detector initialized"
            );
        } else {
            info!("Duplicate detector disabled");
        }

        Self { config }
    }

    /// Check if a transaction is a likely duplicate based on heuristics
    ///
    /// Returns a DuplicateIndicator if the transaction is considered a potential duplicate
    pub fn check_duplicate(
        &self,
        current: &TransactionSnapshot,
        recent_transactions: &[TransactionSnapshot],
    ) -> Option<DuplicateIndicator> {
        if !self.config.enabled {
            return None;
        }

        let detection_window = Duration::from_secs(self.config.detection_window_secs);
        let now = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();

        // Filter to transactions within the detection window with same source
        let candidates: Vec<_> = recent_transactions
            .iter()
            .filter(|tx| {
                tx.id != current.id
                    && tx.source_account == current.source_account
                    && tx.asset == current.asset
                    && (now - tx.timestamp) < self.config.detection_window_secs
            })
            .collect();

        if candidates.is_empty() {
            return None;
        }

        // Find the best match
        let mut best_match: Option<(&TransactionSnapshot, f64)> = None;

        for candidate in candidates {
            let score = self.calculate_similarity_score(current, candidate);

            if score > 0.7 {
                // Confidence threshold of 70%
                if best_match.is_none() || score > best_match.as_ref().unwrap().1 {
                    best_match = Some((candidate, score));
                }
            }
        }

        best_match.map(|(candidate, score)| {
            let confidence_score = (score * 100.0).round() / 100.0; // Round to 2 decimals

            let reason = format!(
                "Potential duplicate: {} from {} at {}, {} ({}% confidence)",
                candidate.amount, candidate.source_account, candidate.timestamp, candidate.asset, confidence_score * 100.0
            );

            warn!(
                current_id = &current.id,
                duplicate_id = &candidate.id,
                confidence = confidence_score,
                "Possible duplicate detected"
            );

            DuplicateIndicator {
                duplicate_transaction_id: candidate.id.clone(),
                duplicate_timestamp: candidate.timestamp,
                confidence_score,
                reason,
            }
        })
    }

    /// Calculate similarity score between two transactions (0.0 to 1.0)
    fn calculate_similarity_score(
        &self,
        current: &TransactionSnapshot,
        candidate: &TransactionSnapshot,
    ) -> f64 {
        let mut score = 0.0;

        // Amount similarity (high score if amounts are nearly identical)
        let amount_diff = (current.amount - candidate.amount).abs();
        if amount_diff <= self.config.amount_tolerance {
            score += 0.6; // 60% of score based on amount match
        } else if amount_diff < self.config.amount_tolerance * 2.0 {
            score += 0.3; // Partial credit for close amounts
        }

        // Timestamp proximity (high score if very close in time)
        let time_diff = (current.timestamp as i64 - candidate.timestamp as i64).abs() as u64;

        // Within 5 minutes is very suspicious
        if time_diff <= 300 {
            score += 0.3;
        } else if time_diff <= 3600 {
            // Within 1 hour is moderately suspicious, decreasing with time
            let hours = time_diff as f64 / 3600.0;
            let time_score = 0.15 * (1.0 - (hours / 24.0).min(1.0));
            score += time_score;
        }

        // Asset match (already filtered, but included for completeness)
        if current.asset == candidate.asset {
            score += 0.1;
        }

        score.clamp(0.0, 1.0)
    }

    /// Batch check multiple transactions for duplicates
    pub fn check_duplicates(
        &self,
        transactions: &[TransactionSnapshot],
    ) -> Vec<(String, Option<DuplicateIndicator>)> {
        transactions
            .iter()
            .map(|tx| {
                let others: Vec<_> = transactions.iter().filter(|t| t.id != tx.id).cloned().collect();
                let duplicate = self.check_duplicate(tx, &others);
                (tx.id.clone(), duplicate)
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn create_tx(id: &str, source: &str, amount: f64, asset: &str, timestamp: u64) -> TransactionSnapshot {
        TransactionSnapshot {
            id: id.to_string(),
            source_account: source.to_string(),
            amount,
            asset: asset.to_string(),
            timestamp,
        }
    }

    #[test]
    fn test_exact_duplicate_detection() {
        let config = DuplicateDetectionConfig::default();
        let detector = DuplicateDetector::new(config);

        let now = 1000000u64;
        let current = create_tx("tx1", "ACCOUNT_A", 100.0, "USD", now);
        let duplicate = create_tx("tx2", "ACCOUNT_A", 100.0, "USD", now - 60); // 1 minute apart

        let result = detector.check_duplicate(&current, &[duplicate.clone()]);
        assert!(result.is_some());
        assert_eq!(result.as_ref().unwrap().duplicate_transaction_id, "tx2");
        assert!(result.as_ref().unwrap().confidence_score > 0.7);
    }

    #[test]
    fn test_near_duplicate_within_tolerance() {
        let config = DuplicateDetectionConfig {
            detection_window_secs: 3600,
            amount_tolerance: 0.1,
            enabled: true,
        };
        let detector = DuplicateDetector::new(config);

        let now = 1000000u64;
        let current = create_tx("tx1", "ACCOUNT_A", 100.0, "USD", now);
        let near_dup = create_tx("tx2", "ACCOUNT_A", 100.05, "USD", now - 120); // Within tolerance

        let result = detector.check_duplicate(&current, &[near_dup]);
        assert!(result.is_some());
    }

    #[test]
    fn test_different_amounts_not_duplicate() {
        let config = DuplicateDetectionConfig {
            detection_window_secs: 3600,
            amount_tolerance: 0.1,
            enabled: true,
        };
        let detector = DuplicateDetector::new(config);

        let now = 1000000u64;
        let current = create_tx("tx1", "ACCOUNT_A", 100.0, "USD", now);
        let different = create_tx("tx2", "ACCOUNT_A", 50.0, "USD", now - 120); // Different amount

        let result = detector.check_duplicate(&current, &[different]);
        assert!(result.is_none());
    }

    #[test]
    fn test_different_accounts_not_duplicate() {
        let config = DuplicateDetectionConfig::default();
        let detector = DuplicateDetector::new(config);

        let now = 1000000u64;
        let current = create_tx("tx1", "ACCOUNT_A", 100.0, "USD", now);
        let different_account = create_tx("tx2", "ACCOUNT_B", 100.0, "USD", now - 60);

        let result = detector.check_duplicate(&current, &[different_account]);
        assert!(result.is_none());
    }

    #[test]
    fn test_outside_detection_window() {
        let config = DuplicateDetectionConfig {
            detection_window_secs: 300, // 5 minutes
            amount_tolerance: 0.01,
            enabled: true,
        };
        let detector = DuplicateDetector::new(config);

        let now = 1000000u64;
        let current = create_tx("tx1", "ACCOUNT_A", 100.0, "USD", now);
        let old_tx = create_tx("tx2", "ACCOUNT_A", 100.0, "USD", now - 3600); // 1 hour ago

        let result = detector.check_duplicate(&current, &[old_tx]);
        assert!(result.is_none());
    }

    #[test]
    fn test_disabled_detector_returns_none() {
        let config = DuplicateDetectionConfig {
            detection_window_secs: 3600,
            amount_tolerance: 0.01,
            enabled: false,
        };
        let detector = DuplicateDetector::new(config);

        let now = 1000000u64;
        let current = create_tx("tx1", "ACCOUNT_A", 100.0, "USD", now);
        let duplicate = create_tx("tx2", "ACCOUNT_A", 100.0, "USD", now - 60);

        let result = detector.check_duplicate(&current, &[duplicate]);
        assert!(result.is_none());
    }

    #[test]
    fn test_legitimate_repeat_transactions() {
        let config = DuplicateDetectionConfig {
            detection_window_secs: 3600,
            amount_tolerance: 0.01,
            enabled: true,
        };
        let detector = DuplicateDetector::new(config);

        let now = 1000000u64;
        // Same source, but 2+ hours apart - legitimate repeat
        let current = create_tx("tx1", "ACCOUNT_A", 50.0, "USD", now);
        let previous = create_tx("tx2", "ACCOUNT_A", 50.0, "USD", now - 7200);

        let result = detector.check_duplicate(&current, &[previous]);
        assert!(result.is_none()); // Outside detection window
    }

    #[test]
    fn test_batch_duplicate_detection() {
        let config = DuplicateDetectionConfig::default();
        let detector = DuplicateDetector::new(config);

        let now = 1000000u64;
        let tx1 = create_tx("tx1", "ACCOUNT_A", 100.0, "USD", now);
        let tx2 = create_tx("tx2", "ACCOUNT_A", 100.0, "USD", now - 60); // Duplicate of tx1
        let tx3 = create_tx("tx3", "ACCOUNT_B", 50.0, "EUR", now); // Different account/asset

        let results = detector.check_duplicates(&[tx1, tx2, tx3]);
        assert_eq!(results.len(), 3);

        // tx1 and tx2 should show as potential duplicates
        assert!(results[0].1.is_some()); // tx1 matches tx2
        assert!(results[1].1.is_some()); // tx2 matches tx1
        assert!(results[2].1.is_none()); // tx3 is unique
    }

    #[test]
    fn test_similarity_score_calculation() {
        let config = DuplicateDetectionConfig::default();
        let detector = DuplicateDetector::new(config);

        let current = TransactionSnapshot {
            id: "current".to_string(),
            source_account: "ACC".to_string(),
            amount: 100.0,
            asset: "USD".to_string(),
            timestamp: 1000000,
        };

        let exact_match = TransactionSnapshot {
            id: "exact".to_string(),
            source_account: "ACC".to_string(),
            amount: 100.0,
            asset: "USD".to_string(),
            timestamp: 1000000,
        };

        let score = detector.calculate_similarity_score(&current, &exact_match);
        assert!(score > 0.9); // Should be very high for exact match
    }
}
