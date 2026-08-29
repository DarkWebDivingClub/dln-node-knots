//! Validates zero-elapsed refill behavior.
//! Success condition: phased refill+debit with zero debit at identical timestamp leaves balance unchanged.
//! Failure condition: balance changes when elapsed time is zero.
use crate::{rule, RateState};

#[test]
fn elapsed_zero_no_refill_change() {
    let mut state = RateState::new(42, 100);
    state
        .withdraw_after_refill(0, 100, &rule(1000, 1_000_000))
        .expect("phased refill should work");
    assert_eq!(state.balance(), 42);
}
