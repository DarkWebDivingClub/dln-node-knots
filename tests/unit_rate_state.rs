//! Unit tests for `RateState` behavior.
//!
//! Why these tests exist:
//! - to lock down refill, debit, and refund math at boundary conditions
//! - to ensure non-negative balance invariants remain enforced
//! - to verify typed error outcomes used by response/error mapping
//! - to prevent regressions while migrating from deprecated methods to phased flow

pub use dln_node::RateLimitRule;

#[path = "../src/state/rate_state.rs"]
mod rate_state_impl;

pub(crate) use rate_state_impl::{RateState, RateStateError};

pub(crate) fn rule(rate_per_micro: u64, max_capacity: i64) -> RateLimitRule {
    RateLimitRule {
        rate_per_micro,
        max_capacity,
    }
}

#[path = "unit/rate_state/amount_exactly_equals_projected_balance.rs"]
mod amount_exactly_equals_projected_balance;
#[path = "unit/rate_state/calculate_refill_caps_at_max_capacity.rs"]
mod calculate_refill_caps_at_max_capacity;
#[path = "unit/rate_state/calculate_refill_saturates_on_large_multiplication.rs"]
mod calculate_refill_saturates_on_large_multiplication;
#[path = "unit/rate_state/check_withdraw_after_refill_err_amount_too_large.rs"]
mod check_withdraw_after_refill_err_amount_too_large;
#[path = "unit/rate_state/check_withdraw_after_refill_err_insufficient_balance.rs"]
mod check_withdraw_after_refill_err_insufficient_balance;
#[path = "unit/rate_state/check_withdraw_after_refill_ok_when_projected_balance_sufficient.rs"]
mod check_withdraw_after_refill_ok_when_projected_balance_sufficient;
#[path = "unit/rate_state/elapsed_zero_no_refill_change.rs"]
mod elapsed_zero_no_refill_change;
#[path = "unit/rate_state/from_rule_rejects_negative_max_capacity.rs"]
mod from_rule_rejects_negative_max_capacity;
#[path = "unit/rate_state/refund_err_amount_too_large.rs"]
mod refund_err_amount_too_large;
#[path = "unit/rate_state/refund_err_invalid_rule.rs"]
mod refund_err_invalid_rule;
#[path = "unit/rate_state/refund_increases_balance_and_clamps_to_max_capacity.rs"]
mod refund_increases_balance_and_clamps_to_max_capacity;
#[path = "unit/rate_state/withdraw_after_refill_applies_refill_then_debit.rs"]
mod withdraw_after_refill_applies_refill_then_debit;
#[path = "unit/rate_state/withdraw_after_refill_does_not_mutate_on_error.rs"]
mod withdraw_after_refill_does_not_mutate_on_error;
#[path = "unit/rate_state/withdraw_after_refill_preserves_non_negative_invariant.rs"]
mod withdraw_after_refill_preserves_non_negative_invariant;
