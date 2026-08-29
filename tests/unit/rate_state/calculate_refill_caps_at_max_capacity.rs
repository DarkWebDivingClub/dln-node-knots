//! Validates refill capping behavior when computed refill exceeds `max_capacity`.
//! Success condition: phased refill+debit with zero debit leaves balance at `max_capacity`.
//! Failure condition: operation errors or resulting balance is above/below expected cap.
use crate::{rule, RateState};

#[test]
fn calculate_refill_caps_at_max_capacity() {
    let mut state = RateState::new(90, 0);
    state
        .withdraw_after_refill(0, 20, &rule(2, 100))
        .expect("phased refill should work");
    assert_eq!(state.balance(), 100);
}
