use nostr_sdk::prelude::*;
use nwc::nostr::nips::nip47::{
    DisableOfferRequest, ListOffersRequest, Method, NostrWalletConnectUri, Request, Response,
};
use std::collections::HashMap;
use std::time::Duration;

use dln_node::{
    clear_offers_for_testing, clear_usage_profiles, seed_offer_for_testing, set_relay_pubkey,
    MethodAccessRule, UsageProfile,
};

mod common;
use common::{grant_usage_profile, start_relay, test_guard};

/// End-to-end test: seed an offer, disable it, then list_offers with active_only to verify it's filtered out.
#[tokio::test]
async fn test_nwc_disable_offer_then_list_active() -> Result<()> {
    let _guard = test_guard();
    clear_usage_profiles();
    clear_offers_for_testing();
    let (_container, relay_url) = start_relay().await;

    let relay_pubkey = Keys::generate().public_key();
    set_relay_pubkey(relay_pubkey.clone());

    // Seed an offer
    seed_offer_for_testing(
        "lno1to_disable".to_string(),
        "temp offer".to_string(),
        1_000,
    );

    let service_keys = Keys::generate();
    let service_pubkey = service_keys.public_key();
    let _service_client = dln_node::run_nwc_service(service_keys, &relay_url).await?;

    tokio::time::sleep(Duration::from_secs(1)).await;

    let client_secret = Keys::generate().secret_key().clone();
    let relay = RelayUrl::parse(&relay_url)?;
    let uri = NostrWalletConnectUri::new(service_pubkey, vec![relay], client_secret.clone(), None);

    let client_keys = Keys::new(client_secret);
    let client_pubkey = client_keys.public_key();

    // Grant both methods
    let mut methods = HashMap::new();
    methods.insert(
        Method::DisableOffer,
        MethodAccessRule { access_rate: None },
    );
    methods.insert(Method::ListOffers, MethodAccessRule { access_rate: None });
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

    // Step 1: Disable the offer
    let disable_params = DisableOfferRequest {
        offer: "lno1to_disable".to_string(),
    };
    let request_event = Request::disable_offer(disable_params)
        .to_event(&uri)
        .expect("Failed to create NWC request event");
    let disable_event_id = request_event.id;
    nwc_client.send_event(&request_event).await?;

    let timeout = Duration::from_secs(10);
    let uri_clone = uri.clone();
    let result = tokio::time::timeout(timeout, async {
        let mut notifications = nwc_client.notifications();
        while let Some(notification) = notifications.next().await {
            if let ClientNotification::Event { event, .. } = notification {
                let event = event.as_ref();
                if event.kind == Kind::WalletConnectResponse && event.pubkey == service_pubkey {
                    // Check the e-tag matches our request
                    let references_our_request = event.tags.iter().any(|tag| {
                        let parts = tag.as_slice();
                        parts.get(0).map(|v| v.as_str()) == Some("e")
                            && parts.get(1).map(|v| v.as_str())
                                == Some(&disable_event_id.to_string())
                    });
                    if !references_our_request {
                        continue;
                    }

                    let response = Response::from_event(&uri_clone, event)
                        .expect("Failed to decrypt NWC response");

                    response
                        .to_disable_offer()
                        .expect("Expected successful disable_offer response");
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
        Err(_) => panic!("Timeout: did not receive disable_offer response within 10 seconds"),
    }

    // Step 2: List offers with active_only=true — should be empty
    let list_params = ListOffersRequest {
        active_only: Some(true),
        limit: None,
        offset: None,
    };
    let request_event = Request::list_offers(list_params)
        .to_event(&uri)
        .expect("Failed to create NWC request event");
    let list_event_id = request_event.id;
    nwc_client.send_event(&request_event).await?;

    let uri_clone2 = uri.clone();
    let result = tokio::time::timeout(Duration::from_secs(10), async {
        let mut notifications = nwc_client.notifications();
        while let Some(notification) = notifications.next().await {
            if let ClientNotification::Event { event, .. } = notification {
                let event = event.as_ref();
                if event.kind == Kind::WalletConnectResponse && event.pubkey == service_pubkey {
                    // Check the e-tag matches our request
                    let references_our_request = event.tags.iter().any(|tag| {
                        let parts = tag.as_slice();
                        parts.get(0).map(|v| v.as_str()) == Some("e")
                            && parts.get(1).map(|v| v.as_str())
                                == Some(&list_event_id.to_string())
                    });
                    if !references_our_request {
                        continue;
                    }

                    let response = Response::from_event(&uri_clone2, event)
                        .expect("Failed to decrypt NWC response");

                    let list = response
                        .to_list_offers()
                        .expect("Response was not a valid list_offers");

                    assert!(
                        list.offers.is_empty(),
                        "Expected no active offers after disabling, got {}",
                        list.offers.len()
                    );
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
        Err(_) => panic!("Timeout: did not receive list_offers response within 10 seconds"),
    }
}
