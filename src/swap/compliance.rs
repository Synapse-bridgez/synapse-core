//! Compliance hold, manual review queue, and release workflow for flagged swaps.
//!
//! A hold is an explicit substate of a swap order, kept separate from order
//! execution: screening integrations open a hold, reviewers claim and decide
//! it, and the result is applied back to the order by the caller. Every
//! operation is a pure state transition that returns the audit events to
//! persist/publish, so the workflow is testable without a database.
//! Persistence lives in `migrations/20260928000001_swap_compliance_holds.sql`.
//!
//! Transitions: `Held -> UnderReview -> Released | Rejected`, plus
//! `Held | UnderReview -> Voided` when the order settles or expires while held.
//! `Released`, `Rejected` and `Voided` are terminal.

use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HoldStatus {
    Held,
    UnderReview,
    Released,
    Rejected,
    Voided,
}

impl HoldStatus {
    pub fn is_terminal(self) -> bool {
        matches!(self, Self::Released | Self::Rejected | Self::Voided)
    }

    pub fn can_transition_to(self, next: HoldStatus) -> bool {
        use HoldStatus::*;
        matches!(
            (self, next),
            (Held, UnderReview)
                | (Held, Voided)
                | (UnderReview, Released)
                | (UnderReview, Rejected)
                | (UnderReview, Voided)
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RiskLevel {
    Standard,
    /// Release requires two distinct senior approvals (four-eyes).
    High,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReasonCode {
    SanctionsMatch,
    AmlAlert,
    KycIncomplete,
    RiskLimit,
    FalsePositive,
    Other,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReviewerRole {
    Reviewer,
    Senior,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reviewer {
    pub id: String,
    pub role: ReviewerRole,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DecisionKind {
    Release,
    Reject,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Decision {
    pub reviewer_id: String,
    pub kind: DecisionKind,
    pub reason_code: ReasonCode,
    pub idempotency_key: String,
    pub decided_at: DateTime<Utc>,
}

/// Audit/notification events emitted by hold operations.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum HoldEvent {
    HoldOpened {
        hold_id: Uuid,
        order_id: Uuid,
        reason: ReasonCode,
    },
    HoldAssigned {
        hold_id: Uuid,
        reviewer_id: String,
    },
    ReleaseApproved {
        hold_id: Uuid,
        reviewer_id: String,
    },
    HoldReleased {
        hold_id: Uuid,
        order_id: Uuid,
    },
    HoldRejected {
        hold_id: Uuid,
        order_id: Uuid,
        reviewer_id: String,
    },
    HoldEscalated {
        hold_id: Uuid,
        order_id: Uuid,
    },
    HoldVoided {
        hold_id: Uuid,
        order_id: Uuid,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum HoldError {
    #[error("hold is already {0:?} and cannot change")]
    Terminal(HoldStatus),
    #[error("transition {from:?} -> {to:?} is not allowed")]
    InvalidTransition { from: HoldStatus, to: HoldStatus },
    #[error("hold is assigned to another reviewer")]
    AssignedToOther,
    #[error("hold must be claimed by the deciding reviewer first")]
    NotAssignee,
    #[error("high-risk release requires senior reviewers")]
    SeniorRequired,
    #[error("reviewer already approved this release; a second reviewer is required")]
    DuplicateApprover,
    #[error("idempotency key was already used for a different decision")]
    IdempotencyConflict,
}

#[derive(Debug, Clone, Serialize)]
pub struct ComplianceHold {
    pub id: Uuid,
    pub order_id: Uuid,
    pub status: HoldStatus,
    pub risk: RiskLevel,
    pub reason: ReasonCode,
    pub assignee: Option<String>,
    pub opened_at: DateTime<Utc>,
    pub sla_deadline: DateTime<Utc>,
    pub escalated: bool,
    pub decisions: Vec<Decision>,
}

impl ComplianceHold {
    pub fn open(
        order_id: Uuid,
        risk: RiskLevel,
        reason: ReasonCode,
        sla: Duration,
        now: DateTime<Utc>,
    ) -> (Self, Vec<HoldEvent>) {
        let hold = Self {
            id: Uuid::new_v4(),
            order_id,
            status: HoldStatus::Held,
            risk,
            reason,
            assignee: None,
            opened_at: now,
            sla_deadline: now + sla,
            escalated: false,
            decisions: Vec::new(),
        };
        let ev = HoldEvent::HoldOpened {
            hold_id: hold.id,
            order_id,
            reason,
        };
        (hold, vec![ev])
    }

    fn transition(&mut self, next: HoldStatus) -> Result<(), HoldError> {
        if self.status.is_terminal() {
            return Err(HoldError::Terminal(self.status));
        }
        if !self.status.can_transition_to(next) {
            return Err(HoldError::InvalidTransition {
                from: self.status,
                to: next,
            });
        }
        self.status = next;
        Ok(())
    }

    /// Claim the hold for review. Re-claiming by the same reviewer is a no-op;
    /// a concurrent claim by a different reviewer is rejected.
    pub fn assign(&mut self, reviewer: &Reviewer) -> Result<Vec<HoldEvent>, HoldError> {
        if self.status.is_terminal() {
            return Err(HoldError::Terminal(self.status));
        }
        match &self.assignee {
            Some(a) if *a == reviewer.id => return Ok(vec![]),
            Some(_) => return Err(HoldError::AssignedToOther),
            None => {}
        }
        self.transition(HoldStatus::UnderReview)?;
        self.assignee = Some(reviewer.id.clone());
        Ok(vec![HoldEvent::HoldAssigned {
            hold_id: self.id,
            reviewer_id: reviewer.id.clone(),
        }])
    }

    /// Record a decision. Idempotent per `idempotency_key`: replaying the same
    /// key and decision returns no events; reusing it for another decision fails.
    pub fn decide(
        &mut self,
        reviewer: &Reviewer,
        kind: DecisionKind,
        reason_code: ReasonCode,
        idempotency_key: &str,
        now: DateTime<Utc>,
    ) -> Result<Vec<HoldEvent>, HoldError> {
        if let Some(prev) = self
            .decisions
            .iter()
            .find(|d| d.idempotency_key == idempotency_key)
        {
            return if prev.kind == kind && prev.reviewer_id == reviewer.id {
                Ok(vec![])
            } else {
                Err(HoldError::IdempotencyConflict)
            };
        }
        if self.status.is_terminal() {
            return Err(HoldError::Terminal(self.status));
        }
        let high_release = kind == DecisionKind::Release && self.risk == RiskLevel::High;
        if high_release && reviewer.role != ReviewerRole::Senior {
            return Err(HoldError::SeniorRequired);
        }
        let is_assignee = self.assignee.as_deref() == Some(reviewer.id.as_str());
        // Under four-eyes the second approver need not be the assignee.
        let second_approver = high_release
            && self
                .decisions
                .iter()
                .any(|d| d.kind == DecisionKind::Release);
        if !is_assignee && !second_approver {
            return Err(HoldError::NotAssignee);
        }
        if high_release
            && self
                .decisions
                .iter()
                .any(|d| d.kind == DecisionKind::Release && d.reviewer_id == reviewer.id)
        {
            return Err(HoldError::DuplicateApprover);
        }

        let decision = Decision {
            reviewer_id: reviewer.id.clone(),
            kind,
            reason_code,
            idempotency_key: idempotency_key.to_string(),
            decided_at: now,
        };
        let (hold_id, order_id) = (self.id, self.order_id);
        match kind {
            DecisionKind::Reject => {
                self.transition(HoldStatus::Rejected)?;
                self.decisions.push(decision);
                Ok(vec![HoldEvent::HoldRejected {
                    hold_id,
                    order_id,
                    reviewer_id: reviewer.id.clone(),
                }])
            }
            DecisionKind::Release => {
                let approvals = self
                    .decisions
                    .iter()
                    .filter(|d| d.kind == DecisionKind::Release)
                    .count()
                    + 1;
                let required = if high_release { 2 } else { 1 };
                let mut events = vec![HoldEvent::ReleaseApproved {
                    hold_id,
                    reviewer_id: reviewer.id.clone(),
                }];
                if approvals >= required {
                    self.transition(HoldStatus::Released)?;
                    events.push(HoldEvent::HoldReleased { hold_id, order_id });
                }
                self.decisions.push(decision);
                Ok(events)
            }
        }
    }

    /// Flag the hold for escalation once past its SLA. Emits at most once.
    pub fn escalate_if_overdue(&mut self, now: DateTime<Utc>) -> Vec<HoldEvent> {
        if self.status.is_terminal() || self.escalated || now < self.sla_deadline {
            return vec![];
        }
        self.escalated = true;
        vec![HoldEvent::HoldEscalated {
            hold_id: self.id,
            order_id: self.order_id,
        }]
    }

    /// The order settled or expired while held; the hold no longer applies.
    pub fn void_for_order_end(&mut self) -> Result<Vec<HoldEvent>, HoldError> {
        self.transition(HoldStatus::Voided)?;
        Ok(vec![HoldEvent::HoldVoided {
            hold_id: self.id,
            order_id: self.order_id,
        }])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rv(id: &str, role: ReviewerRole) -> Reviewer {
        Reviewer {
            id: id.into(),
            role,
        }
    }

    fn open(risk: RiskLevel) -> (ComplianceHold, DateTime<Utc>) {
        let now = Utc::now();
        (
            ComplianceHold::open(
                Uuid::new_v4(),
                risk,
                ReasonCode::AmlAlert,
                Duration::hours(4),
                now,
            )
            .0,
            now,
        )
    }

    #[test]
    fn transition_table() {
        use HoldStatus::*;
        assert!(Held.can_transition_to(UnderReview));
        assert!(!Held.can_transition_to(Released));
        assert!(!Released.can_transition_to(Rejected));
        assert!(UnderReview.can_transition_to(Voided));
    }

    #[test]
    fn standard_release_is_idempotent() {
        let (mut h, now) = open(RiskLevel::Standard);
        let a = rv("a", ReviewerRole::Reviewer);
        h.assign(&a).unwrap();
        let ev = h
            .decide(
                &a,
                DecisionKind::Release,
                ReasonCode::FalsePositive,
                "k1",
                now,
            )
            .unwrap();
        assert_eq!(h.status, HoldStatus::Released);
        assert!(matches!(ev.last(), Some(HoldEvent::HoldReleased { .. })));
        assert!(h
            .decide(
                &a,
                DecisionKind::Release,
                ReasonCode::FalsePositive,
                "k1",
                now
            )
            .unwrap()
            .is_empty());
        assert_eq!(
            h.decide(&a, DecisionKind::Reject, ReasonCode::AmlAlert, "k1", now),
            Err(HoldError::IdempotencyConflict)
        );
    }

    #[test]
    fn high_risk_needs_two_distinct_seniors() {
        let (mut h, now) = open(RiskLevel::High);
        let (s1, s2) = (
            rv("s1", ReviewerRole::Senior),
            rv("s2", ReviewerRole::Senior),
        );
        h.assign(&s1).unwrap();
        assert_eq!(
            h.decide(
                &rv("j", ReviewerRole::Reviewer),
                DecisionKind::Release,
                ReasonCode::FalsePositive,
                "j",
                now
            ),
            Err(HoldError::SeniorRequired)
        );
        h.decide(
            &s1,
            DecisionKind::Release,
            ReasonCode::FalsePositive,
            "k1",
            now,
        )
        .unwrap();
        assert_eq!(h.status, HoldStatus::UnderReview);
        assert_eq!(
            h.decide(
                &s1,
                DecisionKind::Release,
                ReasonCode::FalsePositive,
                "k2",
                now
            ),
            Err(HoldError::DuplicateApprover)
        );
        h.decide(
            &s2,
            DecisionKind::Release,
            ReasonCode::FalsePositive,
            "k3",
            now,
        )
        .unwrap();
        assert_eq!(h.status, HoldStatus::Released);
    }

    #[test]
    fn concurrent_reviewers_and_unassigned_decisions_rejected() {
        let (mut h, now) = open(RiskLevel::Standard);
        let (a, b) = (
            rv("a", ReviewerRole::Reviewer),
            rv("b", ReviewerRole::Reviewer),
        );
        assert_eq!(
            h.decide(&a, DecisionKind::Reject, ReasonCode::AmlAlert, "k", now),
            Err(HoldError::NotAssignee)
        );
        h.assign(&a).unwrap();
        assert_eq!(h.assign(&b), Err(HoldError::AssignedToOther));
        assert_eq!(
            h.decide(&b, DecisionKind::Reject, ReasonCode::AmlAlert, "k", now),
            Err(HoldError::NotAssignee)
        );
    }

    #[test]
    fn terminal_holds_reject_further_changes() {
        let (mut h, now) = open(RiskLevel::Standard);
        let a = rv("a", ReviewerRole::Reviewer);
        h.assign(&a).unwrap();
        h.decide(
            &a,
            DecisionKind::Reject,
            ReasonCode::SanctionsMatch,
            "k",
            now,
        )
        .unwrap();
        assert_eq!(h.status, HoldStatus::Rejected);
        assert_eq!(
            h.void_for_order_end(),
            Err(HoldError::Terminal(HoldStatus::Rejected))
        );
        assert_eq!(
            h.decide(
                &a,
                DecisionKind::Release,
                ReasonCode::FalsePositive,
                "k2",
                now
            ),
            Err(HoldError::Terminal(HoldStatus::Rejected))
        );
    }

    #[test]
    fn sla_escalation_emits_once_and_order_end_voids() {
        let (mut h, now) = open(RiskLevel::Standard);
        assert!(h.escalate_if_overdue(now + Duration::hours(1)).is_empty());
        assert_eq!(h.escalate_if_overdue(now + Duration::hours(5)).len(), 1);
        assert!(h.escalate_if_overdue(now + Duration::hours(6)).is_empty());
        let ev = h.void_for_order_end().unwrap();
        assert!(matches!(ev[0], HoldEvent::HoldVoided { .. }));
        assert!(h.escalate_if_overdue(now + Duration::hours(9)).is_empty());
    }
}
