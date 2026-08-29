//! Validates execution-phase order: refill first, then debit.
//! Success condition: final balance matches `projected_refill - amount`.
//! Failure condition: wrong final balance or unexpected error.
use crate::{rule, RateState};

#[test]
fn withdraw_after_refill_applies_refill_then_debit() {
    let mut state = RateState::new(10, 0);
    state
        .withdraw_after_refill(50, 30, &rule(2, 1_000))
        .expect("withdraw_after_refill should work");
    // Refill: 10 + (2 * 30) = 70, then debit 50 => 20
    assert_eq!(state.balance(), 20);
}
