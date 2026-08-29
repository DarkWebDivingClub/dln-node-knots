use dln_node::RateLimitRule;

#[test]
fn rejects_negative_capacity() {
    let json = "{\"rate_per_micro\":1,\"max_capacity\":-1}";
    let result = serde_json::from_str::<RateLimitRule>(json);
    assert!(result.is_err(), "negative capacity must be rejected");
}

#[test]
fn rejects_capacity_above_i64_max() {
    let json = format!(
        "{{\"rate_per_micro\":1,\"max_capacity\":{}}}",
        (i64::MAX as u128) + 1
    );
    let result = serde_json::from_str::<RateLimitRule>(&json);
    assert!(result.is_err(), "capacity above i64::MAX must be rejected");
}
