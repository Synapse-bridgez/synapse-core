impl std::error::Error for QuoteError {}

/// A rejected swap order. Carries the policy version that made the decision
/// so the rejection can be replayed against the same configuration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RiskRejection {
    pub code: RiskRejectionCode,
    pub policy_version: String,
}

impl fmt::Display for RiskRejection {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.code
        }
    }
}

impl std::error::Error for QuoteError {}

/// A rejected swap order. Carries the policy version that made the decision
/// so the rejection can be replayed against the same configuration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RiskRejection {
    pub code: RiskRejectionCode,
    pub policy_version: String,
}

impl fmt::Display for RiskRejection {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.code.code(), self.code.client_message())
    }
}

impl std::error::Error for RiskRejection {}

