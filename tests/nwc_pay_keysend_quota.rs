use nostr_sdk::prelude::*;
use nwc::nostr::nips::nip47::{
    Method, NostrWalletConnectUri, PayKeysendRequest, Request, Response,
};
use std::collections::HashMap;
use std::time::Duration;

use dln_node::{
    clear_usage_profiles, set_relay_pubkey, MethodAccessRule, RateLimitRule, UsageProfile,
};

mod common;
use common::{grant_usage_profile, start_relay, test_guard};

/// End-to-end test: spend quota allows one payment, second should be quota-exceeded.
#[tokio::test]
async fn test_nwc_pay_keysend_quota_exceeded_after_one_call() -> Result<()> {
    let _guard = test_guard();
    clear_usage_profiles();
    let (_container, relay_url) = start_relay().await;

    let relay_pubkey = Keys::generate().public_key();
    set_relay_pubkey(relay_pubkey.clone());

    let service_keys = Keys::generate();
    let service_pubkey = service_keys.public_key();
    let _service_client = dln_node::run_nwc_service(service_keys, &relay_url).await?;

    tokio::time::sleep(Duration::from_secs(1)).await;

    let client_secret = Keys::generate().secret_key().clone();
    let relay = RelayUrl::parse(&relay_url)?;
    let uri = NostrWalletConnectUri::new(service_pubkey, vec![relay], client_secret.clone(), None);

    let client_keys = Keys::new(client_secret);
    let client_pubkey = client_keys.public_key();

    let mut methods = HashMap::new();
    methods.insert(
        Method::PayKeysend,
        MethodAccessRule {
            access_rate: Some(RateLimitRule {
                rate_per_micro: 1_000_000,
                max_capacity: 10_000_000,
            }),
        },
    );
    let profile = UsageProfile {
        quota: Some(RateLimitRule {
            rate_per_micro: 0,
            max_capacity: 1_000_000,
        }),
        methods: Some(methods),
        control: None,
    };
    let owner_keys = Keys::generate();
    grant_usage_profile(
        &owner_keys,
        &relay_url,
        relay_pubkey,
        client_pubkey,
        &profile,
    )
    .await?;

    let nwc_client = Client::builder().signer(client_keys).build();
    nwc_client.add_relay(&relay_url).await?;
    nwc_client.connect().await;
    tokio::time::sleep(Duration::from_secs(1)).await;

    let filter = Filter::new()
        .kind(Kind::WalletConnectResponse)
        .author(service_pubkey);
    nwc_client.subscribe(filter).await?;

    let params = PayKeysendRequest {
        id: None,
        amount: 1_000_000,
        pubkey: "02".to_string(),
        preimage: None,
        tlv_records: Vec::new(),
    };
    let request_event = Request::pay_keysend(params.clone())
        .to_event(&uri)
        .expect("Failed to create NWC request event");
    nwc_client.send_event(&request_event).await?;

    let timeout = Duration::from_secs(10);
    let uri_clone = uri.clone();
    let result = tokio::time::timeout(timeout, async {
        let mut notifications = nwc_client.notifications();
        while let Some(notification) = notifications.next().await {
            if let ClientNotification::Event { event, .. } = notification {
                let event = event.as_ref();
                if event.kind == Kind::WalletConnectResponse && event.pubkey == service_pubkey {
                    let response = Response::from_event(&uri_clone, event)
                        .expect("Failed to decrypt NWC response");

                    let _pay = response
                        .to_pay_keysend()
                        .expect("First response should be pay_keysend");
                    break;
                }
            }
        }
        Ok::<(), nostr_sdk::client::Error>(())
    })
    .await;

    match result {
        Ok(Ok(())) => {}
        Ok(Err(e)) => panic!("Notification handler error: {}", e),
        Err(_) => panic!("Timeout: did not receive first NWC response within 10 seconds"),
    }

    let request_event = Request::pay_keysend(params)
        .to_event(&uri)
        .expect("Failed to create NWC request event");
    nwc_client.send_event(&request_event).await?;

    let uri_clone = uri.clone();
    let result = tokio::time::timeout(timeout, async {
        let mut notifications = nwc_client.notifications();
        while let Some(notification) = notifications.next().await {
            if let ClientNotification::Event { event, .. } = notification {
                let event = event.as_ref();
                if event.kind == Kind::WalletConnectResponse && event.pubkey == service_pubkey {
                    let response = Response::from_event(&uri_clone, event)
                        .expect("Failed to decrypt NWC response");

                    assert_eq!(response.result_type, Method::PayKeysend);
                    let err = response.error.expect("Expected quota-exceeded error");
                    assert_eq!(err.code, nwc::nostr::nips::nip47::ErrorCode::QuotaExceeded);
                    assert_eq!(err.message, "quota exceeded".to_string());
                    break;
                }
            }
        }
        Ok::<(), nostr_sdk::client::Error>(())
    })
    .await;

    match result {
        Ok(Ok(())) => Ok(()),
        Ok(Err(e)) => panic!("Notification handler error: {}", e),
        Err(_) => panic!("Timeout: did not receive second NWC response within 10 seconds"),
    }
}
