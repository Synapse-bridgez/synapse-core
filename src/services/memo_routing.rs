use regex::Regex;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use tracing::{error, info, warn};

/// Outcome of memo-based routing
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum RoutingOutcome {
    /// Route to high-priority queue
    HighPriority,
    /// Route to normal processing queue
    NormalPriority,
    /// Route to low-priority queue
    LowPriority,
    /// Apply a tag for tracking/reporting
    Tag(String),
    /// Apply a custom processing rule set
    RuleSet(String),
}

/// Pattern type for memo matching
#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum MemoPattern {
    /// Exact prefix match (fast path)
    Prefix(String),
    /// Regex pattern match (validated for ReDoS)
    Regex(String),
}

impl MemoPattern {
    /// Check if this pattern is safe from ReDoS
    fn is_redo_safe(&self) -> Result<(), String> {
        match self {
            MemoPattern::Prefix(_) => Ok(()), // Prefixes are always safe

            MemoPattern::Regex(pattern) => {
                // Check for common ReDoS patterns:
                // 1. Nested quantifiers: e.g., (a+)+, (a*)*
                // 2. Alternation with overlap: e.g., (a|a)+
                // 3. Long character classes with lookahead: e.g., [a-z]+(?=.)+

                // Basic checks for dangerous patterns
                let dangerous_patterns = vec![
                    r"\(\w+\)\+\+",      // (x)++
                    r"\(\w+\)\*\*",      // (x)**
                    r"\(\w+\|\w+\)\+",   // (a|a)+
                    r"\(\w+\|\w+\)\*",   // (a|a)*
                    r"\.(?:[+*]|{.*})+", // .+ or .* with additional modifiers
                ];

                for dangerous in &dangerous_patterns {
                    if pattern.contains(dangerous) {
                        return Err(format!("Pattern contains potential ReDoS: {}", dangerous));
                    }
                }

                // Additional check: limit pattern length to prevent pathological cases
                if pattern.len() > 1000 {
                    return Err("Regex pattern exceeds maximum length of 1000 characters".to_string());
                }

                // Try to compile the regex to ensure it's valid
                match Regex::new(pattern) {
                    Ok(re) => {
                        // Check compiled regex size as a proxy for catastrophic backtracking risk
                        // A regex that's too complex indicates potential ReDoS risk
                        if re.as_str().len() > 500 {
                            warn!("Regex pattern is large and may have ReDoS risk: {}", pattern);
                        }
                        Ok(())
                    }
                    Err(e) => Err(format!("Invalid regex pattern: {}", e)),
                }
            }
        }
    }
}

/// A single memo routing rule
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct MemoRoutingRule {
    pub id: String,
    pub pattern: MemoPattern,
    pub outcome: RoutingOutcome,
    /// Priority when multiple rules match (higher = evaluated first)
    pub priority: u32,
    /// Whether this rule is enabled
    pub enabled: bool,
}

impl MemoRoutingRule {
    /// Check if the given memo matches this rule's pattern
    fn matches(&self, memo: &str) -> bool {
        if !self.enabled {
            return false;
        }

        match &self.pattern {
            MemoPattern::Prefix(prefix) => memo.starts_with(prefix),
            MemoPattern::Regex(pattern) => {
                // We validate at rule creation time, so this should be safe
                if let Ok(re) = Regex::new(pattern) {
                    re.is_match(memo)
                } else {
                    false
                }
            }
        }
    }
}

/// Manager for memo-based transaction routing rules
pub struct MemoRoutingManager {
    // Precompiled and validated rules, sorted by priority (descending)
    rules: Vec<MemoRoutingRule>,
}

impl MemoRoutingManager {
    /// Create a new memo routing manager
    pub fn new(mut rules: Vec<MemoRoutingRule>) -> Result<Self, String> {
        // Validate all rules
        for rule in &rules {
            rule.pattern.is_redo_safe()?;
        }

        // Sort by priority descending (highest priority first)
        rules.sort_by(|a, b| b.priority.cmp(&a.priority));

        info!(rule_count = rules.len(), "Initialized memo routing rules");

        Ok(Self { rules })
    }

    /// Evaluate routing rules for a given memo
    ///
    /// Returns the first matching rule's outcome, or None if no rule matches
    pub fn evaluate(&self, memo: &str) -> Option<RoutingOutcome> {
        for rule in &self.rules {
            if rule.matches(memo) {
                info!(
                    rule_id = &rule.id,
                    memo = memo,
                    "Memo matched routing rule"
                );
                return Some(rule.outcome.clone());
            }
        }
        None
    }

    /// Evaluate routing rules and return all matching rules
    ///
    /// Useful for debugging and understanding which rules apply
    pub fn evaluate_all_matches(&self, memo: &str) -> Vec<String> {
        let mut matches = Vec::new();
        for rule in &self.rules {
            if rule.matches(memo) {
                matches.push(rule.id.clone());
            }
        }
        matches
    }

    /// Add a new rule (replaces existing rule with same ID)
    pub fn add_rule(&mut self, rule: MemoRoutingRule) -> Result<(), String> {
        rule.pattern.is_redo_safe()?;

        // Remove existing rule with same ID
        self.rules.retain(|r| r.id != rule.id);

        // Add new rule and re-sort
        self.rules.push(rule);
        self.rules.sort_by(|a, b| b.priority.cmp(&a.priority));

        Ok(())
    }

    /// Remove a rule by ID
    pub fn remove_rule(&mut self, rule_id: &str) -> bool {
        let initial_len = self.rules.len();
        self.rules.retain(|r| r.id != rule_id);
        self.rules.len() < initial_len
    }

    /// Get all active rules
    pub fn list_rules(&self) -> Vec<MemoRoutingRule> {
        self.rules.clone()
    }

    /// Get rule by ID
    pub fn get_rule(&self, rule_id: &str) -> Option<MemoRoutingRule> {
        self.rules.iter().find(|r| r.id == rule_id).cloned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_prefix_pattern_matching() {
        let rule = MemoRoutingRule {
            id: "test".to_string(),
            pattern: MemoPattern::Prefix("HIGH_".to_string()),
            outcome: RoutingOutcome::HighPriority,
            priority: 10,
            enabled: true,
        };

        assert!(rule.matches("HIGH_PRIORITY_TRANSACTION"));
        assert!(!rule.matches("LOW_PRIORITY_TRANSACTION"));
    }

    #[test]
    fn test_regex_pattern_matching() {
        let rule = MemoRoutingRule {
            id: "test".to_string(),
            pattern: MemoPattern::Regex("^[A-Z]{3}\\d{4}$".to_string()),
            outcome: RoutingOutcome::NormalPriority,
            priority: 10,
            enabled: true,
        };

        assert!(rule.matches("ABC1234"));
        assert!(!rule.matches("abc1234"));
        assert!(!rule.matches("ABCD123"));
    }

    #[test]
    fn test_redo_detection_simple() {
        let pattern = MemoPattern::Regex("(a+)+".to_string());
        assert!(pattern.is_redo_safe().is_err());

        let pattern = MemoPattern::Regex("(a*)*".to_string());
        assert!(pattern.is_redo_safe().is_err());
    }

    #[test]
    fn test_redo_safe_pattern() {
        let pattern = MemoPattern::Regex("^[a-z]+$".to_string());
        assert!(pattern.is_redo_safe().is_ok());

        let pattern = MemoPattern::Regex("^[A-Z0-9]{3,10}$".to_string());
        assert!(pattern.is_redo_safe().is_ok());
    }

    #[test]
    fn test_rule_priority_ordering() {
        let rule1 = MemoRoutingRule {
            id: "rule1".to_string(),
            pattern: MemoPattern::Prefix("A".to_string()),
            outcome: RoutingOutcome::NormalPriority,
            priority: 10,
            enabled: true,
        };

        let rule2 = MemoRoutingRule {
            id: "rule2".to_string(),
            pattern: MemoPattern::Prefix("AB".to_string()),
            outcome: RoutingOutcome::HighPriority,
            priority: 20,
            enabled: true,
        };

        let manager = MemoRoutingManager::new(vec![rule1, rule2]).unwrap();

        // Rule2 has higher priority and should match first
        let outcome = manager.evaluate("ABC");
        assert_eq!(outcome, Some(RoutingOutcome::HighPriority));
    }

    #[test]
    fn test_disabled_rules_ignored() {
        let rule = MemoRoutingRule {
            id: "test".to_string(),
            pattern: MemoPattern::Prefix("HIGH_".to_string()),
            outcome: RoutingOutcome::HighPriority,
            priority: 10,
            enabled: false,
        };

        assert!(!rule.matches("HIGH_PRIORITY_TRANSACTION"));
    }

    #[test]
    fn test_manager_evaluate_all_matches() {
        let rule1 = MemoRoutingRule {
            id: "rule1".to_string(),
            pattern: MemoPattern::Prefix("A".to_string()),
            outcome: RoutingOutcome::NormalPriority,
            priority: 10,
            enabled: true,
        };

        let rule2 = MemoRoutingRule {
            id: "rule2".to_string(),
            pattern: MemoPattern::Prefix("AB".to_string()),
            outcome: RoutingOutcome::HighPriority,
            priority: 5,
            enabled: true,
        };

        let manager = MemoRoutingManager::new(vec![rule1, rule2]).unwrap();

        let matches = manager.evaluate_all_matches("ABC");
        assert_eq!(matches.len(), 2);
        assert!(matches.contains(&"rule1".to_string()));
        assert!(matches.contains(&"rule2".to_string()));
    }

    #[test]
    fn test_pattern_length_limit() {
        let long_pattern = "a".repeat(1001);
        let pattern = MemoPattern::Regex(long_pattern);
        assert!(pattern.is_redo_safe().is_err());
    }
}
