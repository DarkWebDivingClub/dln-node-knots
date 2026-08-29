//! Validates refund crediting with clamp at `max_capacity`.
//! Success condition: refund succeeds and final balance equals capped maximum.
//! Failure condition: final balance exceeds cap or refund errors unexpectedly.
use crate::{rule, RateState};

#[test]
fn refund_increases_balance_and_clamps_to_max_capacity() {
    let mut state = RateState::new(90, 0);
    state.refund(50, &rule(0, 100)).expect("refund should work");
    assert_eq!(state.balance(), 100);
}
