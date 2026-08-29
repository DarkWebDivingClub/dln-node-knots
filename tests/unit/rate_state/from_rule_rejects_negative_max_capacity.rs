//! Validates rule-construction guard for invalid negative `max_capacity`.
//! Success condition: `from_rule` returns `Err(RateStateError::InvalidRule { .. })`.
//! Failure condition: negative capacity is accepted.
use crate::{rule, RateState, RateStateError};

#[test]
fn from_rule_rejects_negative_max_capacity() {
    let result = RateState::from_rule(0, &rule(1, -1));
    assert!(matches!(
        result,
        Err(RateStateError::InvalidRule { max_capacity: -1 })
    ));
}
