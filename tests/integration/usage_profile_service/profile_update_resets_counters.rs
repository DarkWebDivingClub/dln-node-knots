use std::collections::HashMap;
use std::time::Duration;

use dln_node::{
    clear_usage_profiles, set_relay_pubkey, MethodAccessRule, RateLimitRule, UsageProfile,
};
use nostr_sdk::prelude::*;
use nwc::nostr::nips::nip47::{ErrorCode, Method, NostrWalletConnectUri, Request, Response};

use crate::integration_suite::common::{grant_usage_profile, start_relay, test_guard};
use crate::integration_suite::shared_relay_pubkey;

async fn send_get_info_and_read_response(
    nwc_client: &Client,
    uri: &NostrWalletConnectUri,
    service_pubkey: PublicKey,
) -> Response {
    let request_event = Request::get_info()
        .to_event(uri)
        .expect("failed to create get_info request");
    nwc_client
        .send_event(&request_event)
        .await
        .expect("failed to publish get_info request");

    let timeout = Duration::from_secs(10);
    let uri_clone = uri.clone();
    tokio::time::timeout(timeout, async {
        let mut notifications = nwc_client.notifications();
        while let Some(notification) = notifications.next().await {
            if let ClientNotification::Event { event, .. } = notification {
                let event = event.as_ref();
                if event.kind == Kind::WalletConnectResponse && event.pubkey == service_pubkey {
                    return Response::from_event(&uri_clone, event)
                        .expect("failed to decrypt get_info response");
                }
            }
        }
        panic!("notification stream ended before get_info response");
    })
    .await
    .expect("timeout waiting for get_info response")
}

/// Ensures counters reset when a profile is updated for the same pubkey.
#[tokio::test]
async fn profile_update_resets_counters() -> Result<()> {
    let _guard = test_guard();
    clear_usage_profiles();

    let (_container, relay_url) = start_relay().await;
    let relay_pubkey = shared_relay_pubkey();
    set_relay_pubkey(relay_pubkey.clone());

    let service_keys = Keys::generate();
    let service_pubkey = service_keys.public_key();
    let _service_client = dln_node::run_nwc_service(service_keys, &relay_url).await?;
    tokio::time::sleep(Duration::from_secs(1)).await;

    let user_keys = Keys::generate();
    let user_pubkey = user_keys.public_key();

    let mut methods = HashMap::new();
    methods.insert(
        Method::GetInfo,
        MethodAccessRule {
            access_rate: Some(RateLimitRule {
                rate_per_micro: 0,
                max_capacity: 1_000_000,
            }),
        },
    );
    let profile = UsageProfile {
        quota: None,
        methods: Some(methods),
        control: None,
    };

    let owner_keys = Keys::generate();
    grant_usage_profile(&owner_keys, &relay_url, relay_pubkey, user_pubkey, &profile).await?;

    let relay = RelayUrl::parse(&relay_url)?;
    let client_secret = user_keys.secret_key().clone();
    let uri = NostrWalletConnectUri::new(service_pubkey, vec![relay], client_secret, None);

    let nwc_client = Client::builder().signer(user_keys).build();
    nwc_client.add_relay(&relay_url).await?;
    nwc_client.connect().await;
    tokio::time::sleep(Duration::from_secs(1)).await;
    nwc_client
        .subscribe(
            Filter::new()
                .kind(Kind::WalletConnectResponse)
                .author(service_pubkey),
        )
        .await?;

    let pre_limit_timeout = Duration::from_secs(10);
    let start = tokio::time::Instant::now();
    loop {
        let first = send_get_info_and_read_response(&nwc_client, &uri, service_pubkey).await;
        if first.error.is_none() {
            first
                .to_get_info()
                .expect("first request should decode as get_info");
            break;
        }
        if start.elapsed() > pre_limit_timeout {
            panic!(
                "request never became authorized before rate-limit check, last response: {:?}",
                first.error
            );
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }

    let second = send_get_info_and_read_response(&nwc_client, &uri, service_pubkey).await;
    let second_err = second.error.expect("second request should be rate-limited");
    assert_eq!(second_err.code, ErrorCode::RateLimited);

    grant_usage_profile(&owner_keys, &relay_url, relay_pubkey, user_pubkey, &profile).await?;

    let reset_timeout = Duration::from_secs(8);
    let start = tokio::time::Instant::now();
    loop {
        let response = send_get_info_and_read_response(&nwc_client, &uri, service_pubkey).await;
        if response.error.is_none() {
            response
                .to_get_info()
                .expect("post-update success should decode as get_info");
            break;
        }
        let err = response.error.expect("error expected when not yet reset");
        if start.elapsed() > reset_timeout {
            panic!(
                "profile update did not reset counters in time, last error: {:?}",
                err
            );
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }

    Ok(())
}
