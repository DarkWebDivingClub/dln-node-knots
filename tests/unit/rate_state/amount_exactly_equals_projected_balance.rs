//! Validates execution when debit amount equals projected post-refill balance.
//! Success condition: `withdraw_after_refill` returns `Ok(())` and final balance is `0`.
//! Failure condition: returns an error or leaves a non-zero balance.
use crate::{rule, RateState};

#[test]
fn amount_exactly_equals_projected_balance() {
    let mut state = RateState::new(0, 0);
    state
        .withdraw_after_refill(50, 50, &rule(1, 1_000))
        .expect("exact projected balance should be withdrawable");
    assert_eq!(state.balance(), 0);
}
