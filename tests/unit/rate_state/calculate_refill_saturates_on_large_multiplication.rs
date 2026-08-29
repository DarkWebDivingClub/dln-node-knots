//! Validates overflow-safe refill math under extremely large rate/time inputs.
//! Success condition: phased refill+debit with zero debit does not panic and caps at `i64::MAX`.
//! Failure condition: panic, overflow behavior, or incorrect final balance.
use crate::{rule, RateState};

#[test]
fn calculate_refill_saturates_on_large_multiplication() {
    let mut state = RateState::new(0, 0);
    state
        .withdraw_after_refill(0, u64::MAX, &rule(u64::MAX, i64::MAX))
        .expect("phased refill should saturate and cap");
    assert_eq!(state.balance(), i64::MAX);
}
