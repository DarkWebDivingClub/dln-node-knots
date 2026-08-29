//! Validates non-negative balance invariant in execution phase.
//! Success condition: insufficient debit returns error and balance stays non-negative/unchanged.
//! Failure condition: negative balance is produced or debit succeeds unexpectedly.
use crate::{rule, RateState, RateStateError};

#[test]
fn withdraw_after_refill_preserves_non_negative_invariant() {
    let mut state = RateState::new(10, 0);
    let result = state.withdraw_after_refill(71, 30, &rule(2, 1_000));
    assert!(matches!(result, Err(RateStateError::InsufficientBalance)));
    assert_eq!(state.balance(), 10);
}
