//! Validates state immutability when execution-phase debit fails before mutation.
//! Success condition: method returns error and original balance remains unchanged.
//! Failure condition: balance changes despite error.
use crate::{rule, RateState, RateStateError};

#[test]
fn withdraw_after_refill_does_not_mutate_on_error() {
    let mut state = RateState::new(5, 123);
    let amount = (i64::MAX as u64) + 1;
    let result = state.withdraw_after_refill(amount, 200, &rule(1, i64::MAX));
    assert!(matches!(result, Err(RateStateError::AmountTooLarge { .. })));
    assert_eq!(state.balance(), 5);
}
