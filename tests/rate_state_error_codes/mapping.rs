use dln_node::{map_rate_state_error, AccessErrorContext, RateStateError};
use nwc::nostr::nips::nip47::ErrorCode;

#[test]
fn insufficient_balance_maps_to_rate_limited_for_access_rate() {
    let err = map_rate_state_error(
        &RateStateError::InsufficientBalance,
        AccessErrorContext::AccessRate,
    );
    assert_eq!(err.code, ErrorCode::RateLimited);
}

#[test]
fn insufficient_balance_maps_to_quota_exceeded_for_quota() {
    let err = map_rate_state_error(
        &RateStateError::InsufficientBalance,
        AccessErrorContext::Quota,
    );
    assert_eq!(err.code, ErrorCode::QuotaExceeded);
}

#[test]
fn amount_too_large_maps_to_other() {
    let err = map_rate_state_error(
        &RateStateError::AmountTooLarge {
            amount: (i64::MAX as u64) + 1,
        },
        AccessErrorContext::Quota,
    );
    assert_eq!(err.code, ErrorCode::Other);
    assert_eq!(err.message, "invalid amount: exceeds i64::MAX");
}

#[test]
fn invalid_rule_maps_to_other() {
    let err = map_rate_state_error(
        &RateStateError::InvalidRule { max_capacity: -1 },
        AccessErrorContext::AccessRate,
    );
    assert_eq!(err.code, ErrorCode::Other);
    assert_eq!(err.message, "invalid rate limit rule");
}
