use dln_node::RateLimitRule;

#[test]
fn accepts_i64_max_capacity() {
    let json = format!("{{\"rate_per_micro\":1,\"max_capacity\":{}}}", i64::MAX);
    let rule: RateLimitRule = serde_json::from_str(&json).expect("rule should deserialize");
    assert_eq!(rule.max_capacity, i64::MAX);
}

#[test]
fn default_capacity_is_i64_max() {
    let rule: RateLimitRule = serde_json::from_str("{}").expect("rule should deserialize");
    assert_eq!(rule.max_capacity, i64::MAX);
}
