//! Validates check-phase success when projected post-refill balance can cover amount.
//! Success condition: `check_withdraw_after_refill` returns `Ok(())`.
//! Failure condition: returns an error despite sufficient projected balance.
use crate::{rule, RateState};

#[test]
fn check_withdraw_after_refill_ok_when_projected_balance_sufficient() {
    let state = RateState::new(0, 0);
    let result = state.check_withdraw_after_refill(50, 50, &rule(1, 1_000));
    assert!(result.is_ok());
}
