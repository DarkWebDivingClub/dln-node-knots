use std::collections::HashMap;

use dln_node::{MethodAccessRule, RateLimitRule, UsageProfile};
use nwc::nostr::nips::nip47::Method;

#[test]
fn usage_profile_json_roundtrip() {
    let mut methods = HashMap::new();
    methods.insert(
        Method::GetInfo,
        MethodAccessRule {
            access_rate: Some(RateLimitRule {
                rate_per_micro: 1,
                max_capacity: 1_000_000,
            }),
        },
    );

    let profile = UsageProfile {
        quota: Some(RateLimitRule {
            rate_per_micro: 2,
            max_capacity: 2_000_000,
        }),
        methods: Some(methods),
        control: None,
    };

    let json = serde_json::to_string(&profile).expect("serialize UsageProfile");
    println!("{}", json);
    let decoded: UsageProfile = serde_json::from_str(&json).expect("deserialize UsageProfile");
    assert_eq!(decoded, profile);
}
