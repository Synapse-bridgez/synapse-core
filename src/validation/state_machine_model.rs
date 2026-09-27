//! Model-checking harness for the transaction state machine (issue #1316).
//!
//! The model's states and guarded transitions are derived from
//! `TRANSACTION_TRANSITIONS` and checked through the real
//! `validate_status_transition` guard:
//!
//! * exhaustively, over every `(from, to)` pair of known and unknown states;
//! * by breadth-first reachability from `pending`;
//! * by `proptest` over arbitrary sequential event sequences;
//! * by `proptest` over interleaved writers that each act on a stale read,
//!   the TOCTOU shape of concurrent status updates.
//!
//! Invariant checked everywhere: every persisted status change is either a
//! same-state no-op or an edge of `TRANSACTION_TRANSITIONS`, and the status
//! never leaves the known state set. With a compare-and-set write
//! (`UPDATE ... WHERE status = <read status>`) the invariant holds for every
//! interleaving. A read-validate-write without that guard violates it; the
//! counterexample is pinned in
//! `unguarded_read_validate_write_persists_an_invalid_transition`.

use super::state_machine::validate_status_transition;
use super::state_transitions::TRANSACTION_TRANSITIONS;
use proptest::prelude::*;
use std::collections::{BTreeSet, VecDeque};

const INITIAL: &str = "pending";

/// Strings that are not states, to check the guard never admits them.
const NOT_STATES: &[&str] = &["", "PENDING", "complete", "cancelled", "dlq "];

// === Model

fn states() -> BTreeSet<&'static str> {
    TRANSACTION_TRANSITIONS
        .iter()
        .flat_map(|t| [t.from, t.to])
        .collect()
}

fn is_edge(from: &str, to: &str) -> bool {
    TRANSACTION_TRANSITIONS
        .iter()
        .any(|t| t.from == from && t.to == to)
}

/// The invariant every persisted change must satisfy.
fn is_allowed_change(from: &str, to: &str) -> bool {
    from == to || is_edge(from, to)
}

fn accepted(from: &str, to: &str) -> bool {
    validate_status_transition(from, to).is_ok()
}

fn reachable_from(start: &'static str) -> BTreeSet<&'static str> {
    let all = states();
    let mut seen = BTreeSet::from([start]);
    let mut queue = VecDeque::from([start]);
    while let Some(state) = queue.pop_front() {
        for &next in &all {
            if accepted(state, next) && seen.insert(next) {
                queue.push_back(next);
            }
        }
    }
    seen
}

/// Requested targets: every known state plus strings that aren't states.
fn target_strategy() -> impl Strategy<Value = &'static str> {
    let mut targets: Vec<&'static str> = states().into_iter().collect();
    targets.extend_from_slice(NOT_STATES);
    proptest::sample::select(targets)
}

/// One step of an interleaving: a worker reads the current status, or tries
/// to write `target` based on whatever it last read.
#[derive(Debug, Clone)]
enum Op {
    Read(usize),
    Write(usize, &'static str),
}

const WORKERS: usize = 3;

fn op_strategy() -> impl Strategy<Value = Op> {
    prop_oneof![
        (0..WORKERS).prop_map(Op::Read),
        (0..WORKERS, target_strategy()).prop_map(|(w, t)| Op::Write(w, t)),
    ]
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum WriteDiscipline {
    /// `UPDATE ... SET status = $to WHERE id = $id AND status = $read`
    CompareAndSet,
    /// Validate against the read, then `UPDATE ... WHERE id = $id`.
    Unguarded,
}

/// Runs `ops` against one row and returns every persisted `(from, to)`
/// change, in order.
fn run_interleaving(ops: &[Op], discipline: WriteDiscipline) -> Vec<(&'static str, &'static str)> {
    let mut current: &'static str = INITIAL;
    let mut snapshot: [&'static str; WORKERS] = [INITIAL; WORKERS];
    let mut changes = Vec::new();
    for op in ops {
        match *op {
            Op::Read(w) => snapshot[w] = current,
            Op::Write(w, target) => {
                let read = snapshot[w];
                if !accepted(read, target) {
                    continue;
                }
                let applies = match discipline {
                    WriteDiscipline::CompareAndSet => current == read,
                    WriteDiscipline::Unguarded => true,
                };
                if applies {
                    changes.push((current, target));
                    current = target;
                }
            }
        }
    }
    changes
}

// === Exhaustive checks

#[test]
fn guard_matches_the_transition_table_exhaustively() {
    let known = states();
    let mut candidates: Vec<&str> = known.iter().copied().collect();
    candidates.extend_from_slice(NOT_STATES);

    for &from in &known {
        for &to in &candidates {
            assert_eq!(
                accepted(from, to),
                is_allowed_change(from, to),
                "guard disagrees with TRANSACTION_TRANSITIONS on {from:?} -> {to:?}"
            );
        }
    }
}

#[test]
fn only_the_documented_states_are_reachable_from_pending() {
    // `dlq` has an outgoing edge (requeue) but no incoming one: rows enter
    // it through DLQ tooling outside this state machine, never via a
    // validated transition.
    let expected = BTreeSet::from([
        "completed",
        "failed",
        "pending",
        "pending_review",
        "processing",
    ]);
    assert_eq!(reachable_from(INITIAL), expected);
    assert!(states().contains("dlq"));
}

#[test]
fn completed_is_absorbing() {
    for &to in &states() {
        if to != "completed" {
            assert!(
                !accepted("completed", to),
                "completed must be terminal, but completed -> {to} is accepted"
            );
        }
    }
    assert!(reachable_from("completed") == BTreeSet::from(["completed"]));
}

// === Interleaving counterexample (regression)

#[test]
fn unguarded_read_validate_write_persists_an_invalid_transition() {
    // Worker 0 and worker 1 both read `pending`. Worker 0 completes the
    // transaction; worker 1, still acting on its stale read, validates
    // `pending -> failed` and overwrites the row.
    let ops = [
        Op::Read(0),
        Op::Read(1),
        Op::Write(0, "completed"),
        Op::Write(1, "failed"),
    ];

    let unguarded = run_interleaving(&ops, WriteDiscipline::Unguarded);
    assert_eq!(
        unguarded,
        vec![("pending", "completed"), ("completed", "failed")]
    );
    assert!(!is_allowed_change("completed", "failed"));

    // Compare-and-set rejects the stale write.
    let guarded = run_interleaving(&ops, WriteDiscipline::CompareAndSet);
    assert_eq!(guarded, vec![("pending", "completed")]);
}

// === Randomised exploration

proptest! {
    #![proptest_config(ProptestConfig { cases: 512, ..ProptestConfig::default() })]

    #[test]
    fn sequential_events_never_reach_an_invalid_state(
        targets in proptest::collection::vec(target_strategy(), 0..40)
    ) {
        let known = states();
        let mut current: &str = INITIAL;
        let mut completed = false;
        for target in targets {
            if accepted(current, target) {
                prop_assert!(is_allowed_change(current, target));
                current = target;
            }
            prop_assert!(known.contains(current), "left the state set: {current:?}");
            if completed {
                prop_assert_eq!(current, "completed");
            }
            completed |= current == "completed";
        }
    }

    #[test]
    fn compare_and_set_keeps_every_interleaving_valid(
        ops in proptest::collection::vec(op_strategy(), 0..60)
    ) {
        for (from, to) in run_interleaving(&ops, WriteDiscipline::CompareAndSet) {
            prop_assert!(
                is_allowed_change(from, to),
                "compare-and-set persisted {from:?} -> {to:?}"
            );
        }
    }
}
