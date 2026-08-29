//! Validates refund rule guard for negative `max_capacity`.
//! Success condition: returns `Err(RateStateError::InvalidRule { .. })`.
//! Failure condition: invalid rule is accepted by refund path.
use crate::{rule, RateState, RateStateError};

#[test]
fn refund_err_invalid_rule() {
    let mut state = RateState::new(0, 0);
    let result = state.refund(1, &rule(0, -1));
    assert!(matches!(
        result,
        Err(RateStateError::InvalidRule { max_capacity: -1 })
    ));
}
