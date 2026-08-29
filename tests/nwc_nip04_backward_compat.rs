use nostr_sdk::prelude::*;
use nwc::nostr::nips::nip04;
use nwc::nostr::nips::nip47::{Method, Request, Response};
use std::collections::HashMap;
use std::time::Duration;

use dln_node::{clear_usage_profiles, set_relay_pubkey, MethodAccessRule, UsageProfile};

mod common;
use common::{grant_usage_profile, start_relay, test_guard};

/// Send a NWC request with NIP-04 (no encryption tag) — backward compat.
/// Verify the response has NO encryption tag and is NIP-04 encrypted.
#[tokio::test]
async fn test_nwc_nip04_backward_compat() -> Result<()> {
    let _guard = test_guard();
    clear_usage_profiles();
    let (_container, relay_url) = start_relay().await;

    let relay_pubkey = Keys::generate().public_key();
    set_relay_pubkey(relay_pubkey.clone());

    let service_keys = Keys::generate();
    let service_pubkey = service_keys.public_key();
    let _service_client = dln_node::run_nwc_service(service_keys, &relay_url).await?;
    tokio::time::sleep(Duration::from_secs(1)).await;

    // Client keys
    let client_keys = Keys::generate();
    let client_pubkey = client_keys.public_key();

    // Grant get_info access
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

    // Build a NIP-04 encrypted request (no encryption tag — legacy)
    let request = Request::get_info();
    let encrypted_content = nip04::encrypt(
        client_keys.secret_key(),
        &service_pubkey,
        request.as_json(),
    )
    .expect("nip04 encrypt");
    let request_event = EventBuilder::new(Kind::WalletConnectRequest, encrypted_content)
        .tag(Tag::public_key(service_pubkey))
        .sign_with_keys(&client_keys)?;

    let nwc_client = Client::builder().signer(client_keys.clone()).build();
    nwc_client.add_relay(&relay_url).await?;
    nwc_client.connect().await;
    tokio::time::sleep(Duration::from_secs(1)).await;

    let filter = Filter::new()
        .kind(Kind::WalletConnectResponse)
        .author(service_pubkey);
    nwc_client.subscribe(filter).await?;

    nwc_client.send_event(&request_event).await?;

    let timeout = Duration::from_secs(10);
    let result = tokio::time::timeout(timeout, async {
        let mut notifications = nwc_client.notifications();
        while let Some(notification) = notifications.next().await {
            if let ClientNotification::Event { event, .. } = notification {
                let event = event.as_ref();
                if event.kind == Kind::WalletConnectResponse && event.pubkey == service_pubkey {
                    // Verify NO encryption tag on response (NIP-04 backward compat)
                    let enc_tag = event
                        .tags
                        .iter()
                        .find(|tag| tag.as_slice().first().map(|v| v.as_str()) == Some("encryption"));
                    assert!(
                        enc_tag.is_none(),
                        "Response should NOT have encryption tag for legacy NIP-04 request"
                    );

                    // Decrypt with NIP-04
                    let decrypted = nip04::decrypt(
                        client_keys.secret_key(),
                        &event.pubkey,
                        &event.content,
                    )
                    .expect("nip04 decrypt response");

                    let response = Response::from_json(&decrypted)
                        .expect("parse NWC response");
                    let info = response
                        .to_get_info()
                        .expect("Response was not a valid get_info");

                    assert_eq!(info.alias, Some("dln-node".to_string()));

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
