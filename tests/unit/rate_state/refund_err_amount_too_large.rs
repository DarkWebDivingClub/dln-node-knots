//! Validates refund amount conversion guard for values above `i64::MAX`.
//! Success condition: returns `Err(RateStateError::AmountTooLarge { .. })`.
//! Failure condition: accepts oversized refund amount or returns unrelated error.
use crate::{rule, RateState, RateStateError};

#[test]
fn refund_err_amount_too_large() {
    let mut state = RateState::new(0, 0);
    let amount = (i64::MAX as u64) + 1;
    let result = state.refund(amount, &rule(0, i64::MAX));
    assert!(matches!(result, Err(RateStateError::AmountTooLarge { .. })));
}
