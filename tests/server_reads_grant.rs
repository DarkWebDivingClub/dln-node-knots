use std::collections::HashMap;
use std::time::Duration;

use dln_node::{
    clear_usage_profiles, get_usage_profile, set_relay_pubkey, MethodAccessRule, UsageProfile,
};
use nostr_sdk::prelude::*;
use nwc::nostr::nips::nip47::Method;

mod common;
use common::{grant_usage_profile, start_relay, test_guard};

/// End-to-end test: server reads a usage profile grant from the relay.
#[tokio::test]
async fn test_server_reads_usage_profile_grant() -> Result<()> {
    let _guard = test_guard();
    clear_usage_profiles();

    let (_container, relay_url) = start_relay().await;

    let relay_pubkey = Keys::generate().public_key();
    set_relay_pubkey(relay_pubkey.clone());

    let service_keys = Keys::generate();
    let _service_client = dln_node::run_nwc_service(service_keys, &relay_url).await?;
    tokio::time::sleep(Duration::from_secs(1)).await;

    let owner_keys = Keys::generate();
    let user_pubkey = Keys::generate().public_key();

    let mut methods = HashMap::new();
    methods.insert(Method::GetInfo, MethodAccessRule { access_rate: None });
    let profile = UsageProfile {
        quota: None,
        methods: Some(methods),
        control: None,
    };
    grant_usage_profile(&owner_keys, &relay_url, relay_pubkey, user_pubkey, &profile).await?;

    let timeout = Duration::from_secs(5);
    let start = tokio::time::Instant::now();
    loop {
        if let Some(profile) = get_usage_profile(&user_pubkey.to_string()) {
            let methods = profile.methods.expect("methods present");
            assert!(methods.contains_key(&Method::GetInfo));
            break;
        }
        if start.elapsed() > timeout {
            panic!("Timeout: server did not store usage profile grant");
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }

    Ok(())
}
