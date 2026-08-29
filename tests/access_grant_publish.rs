use std::collections::HashMap;
use std::time::Duration;

use nostr_sdk::prelude::*;
use nwc::nostr::nips::nip47::Method;

use dln_node::{MethodAccessRule, UsageProfile};

mod common;
use common::{grant_usage_profile, start_relay, test_guard};

/// End-to-end test: publish a UsageProfile grant and re-read it from the relay.
#[tokio::test]
async fn test_publish_and_read_access_grant() -> Result<()> {
    let _guard = test_guard();
    let (_container, relay_url) = start_relay().await;

    let owner_keys = Keys::generate();
    let owner_pubkey = owner_keys.public_key();

    let relay_pubkey = Keys::generate().public_key();
    let user_pubkey = Keys::generate().public_key();
    let d_value = format!("{}:{}", relay_pubkey, user_pubkey);

    let mut methods = HashMap::new();
    methods.insert(Method::GetInfo, MethodAccessRule { access_rate: None });
    let profile = UsageProfile {
        quota: None,
        methods: Some(methods),
        control: None,
    };
    grant_usage_profile(&owner_keys, &relay_url, relay_pubkey, user_pubkey, &profile).await?;

    let reader = Client::builder().signer(Keys::generate()).build();
    reader.add_relay(&relay_url).await?;
    reader.connect().await;
    tokio::time::sleep(Duration::from_secs(1)).await;

    let filter = Filter::new()
        .kind(Kind::Custom(30078))
        .author(owner_pubkey)
        .custom_tag(SingleLetterTag::lowercase(Alphabet::D), d_value);

    let events = reader
        .fetch_events(filter)
        .timeout(Duration::from_secs(5))
        .await?;

    assert_eq!(events.len(), 1);
    let event = events.iter().next().expect("grant event");
    let decoded: UsageProfile = serde_json::from_str(&event.content).expect("decode UsageProfile");
    let methods = decoded.methods.expect("methods present");
    assert!(methods.contains_key(&Method::GetInfo));

    Ok(())
}
