//! Validates check-phase insufficient-balance path.
//! Success condition: returns `Err(RateStateError::InsufficientBalance)`.
//! Failure condition: returns `Ok(())` or a different error type.
use crate::{rule, RateState, RateStateError};

#[test]
fn check_withdraw_after_refill_err_insufficient_balance() {
    let state = RateState::new(0, 0);
    let result = state.check_withdraw_after_refill(51, 50, &rule(1, 1_000));
    assert!(matches!(result, Err(RateStateError::InsufficientBalance)));
}
