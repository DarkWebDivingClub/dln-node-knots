//! Validates check-phase amount conversion guard for values above `i64::MAX`.
//! Success condition: returns `Err(RateStateError::AmountTooLarge { .. })`.
//! Failure condition: accepts oversized amount or returns unrelated error.
use crate::{rule, RateState, RateStateError};

#[test]
fn check_withdraw_after_refill_err_amount_too_large() {
    let state = RateState::new(0, 0);
    let amount = (i64::MAX as u64) + 1;
    let result = state.check_withdraw_after_refill(amount, 0, &rule(1, i64::MAX));
    assert!(matches!(result, Err(RateStateError::AmountTooLarge { .. })));
}
