use nostr_sdk::prelude::*;
use nwc::nostr::nips::nip47::{Method, NostrWalletConnectUri, Request, Response};
use std::collections::HashMap;
use std::time::Duration;

use dln_node::{clear_usage_profiles, set_relay_pubkey, MethodAccessRule, UsageProfile};

mod common;
use common::{grant_usage_profile, start_relay, test_guard};

/// End-to-end test: send a NWC get_info request, expect a valid response.
#[tokio::test]
async fn test_nwc_get_info_roundtrip() -> Result<()> {
    let _guard = test_guard();
    clear_usage_profiles();
    let (_container, relay_url) = start_relay().await;

    let relay_pubkey = Keys::generate().public_key();
    set_relay_pubkey(relay_pubkey.clone());

    // Wallet service keys — the NWC service we're testing
    let service_keys = Keys::generate();
    let service_pubkey = service_keys.public_key();
    let _service_client = dln_node::run_nwc_service(service_keys, &relay_url).await?;

    tokio::time::sleep(Duration::from_secs(1)).await;

    // Verify the service published its capabilities (Kind 13194)
    let info_filter = Filter::new()
        .kind(Kind::WalletConnectInfo)
        .author(service_pubkey);
    let info_client = Client::builder().signer(Keys::generate()).build();
    info_client.add_relay(&relay_url).await?;
    info_client.connect().await;
    let info_events = info_client
        .fetch_events(info_filter)
        .timeout(Duration::from_secs(5))
        .await?;
    assert_eq!(info_events.len(), 1);
    let info_event = info_events.iter().next().unwrap();
    assert_eq!(info_event.pubkey, service_pubkey);
    assert!(info_event.content.contains("get_info"));
    assert!(info_event.content.contains("get_balance"));

    // Build a NWC URI
    let client_secret = Keys::generate().secret_key().clone();
    let relay = RelayUrl::parse(&relay_url)?;
    let uri = NostrWalletConnectUri::new(service_pubkey, vec![relay], client_secret.clone(), None);

    // Create the NWC client and grant get_info access via profile.
    let client_keys = Keys::new(client_secret);
    let client_pubkey = client_keys.public_key();

    let mut methods = HashMap::new();
    methods.insert(Method::GetInfo, MethodAccessRule { access_rate: None });
    let profile = UsageProfile {
        quota: None,
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

    let request_event = Request::get_info()
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

                    let info = response
                        .to_get_info()
                        .expect("Response was not a valid get_info");

                    assert_eq!(info.alias, Some("dln-node".to_string()));
                    assert_eq!(info.network, Some("regtest".to_string()));
                    assert_eq!(info.methods, vec![Method::GetInfo]);

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
        Err(_) => panic!("Timeout: did not receive NWC response within 10 seconds"),
    }
}
