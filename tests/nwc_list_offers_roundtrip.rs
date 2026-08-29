use nostr_sdk::prelude::*;
use nwc::nostr::nips::nip47::{
    ListOffersRequest, Method, NostrWalletConnectUri, Request, Response,
};
use std::collections::HashMap;
use std::time::Duration;

use dln_node::{
    clear_offers_for_testing, clear_usage_profiles, seed_offer_for_testing, set_relay_pubkey,
    MethodAccessRule, UsageProfile,
};

mod common;
use common::{grant_usage_profile, start_relay, test_guard};

/// End-to-end test: seed two offers, send list_offers, expect both returned.
#[tokio::test]
async fn test_nwc_list_offers_returns_seeded_offers() -> Result<()> {
    let _guard = test_guard();
    clear_usage_profiles();
    clear_offers_for_testing();
    let (_container, relay_url) = start_relay().await;

    let relay_pubkey = Keys::generate().public_key();
    set_relay_pubkey(relay_pubkey.clone());

    // Seed two offers into the store
    seed_offer_for_testing(
        "lno1offer_a".to_string(),
        "coffee".to_string(),
        5_000,
    );
    seed_offer_for_testing(
        "lno1offer_b".to_string(),
        "donation".to_string(),
        0,
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

    let mut methods = HashMap::new();
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

    let params = ListOffersRequest {
        active_only: None,
        limit: None,
        offset: None,
    };
    let request_event = Request::list_offers(params)
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

                    let list = response
                        .to_list_offers()
                        .expect("Response was not a valid list_offers");

                    assert_eq!(list.offers.len(), 2, "Expected 2 offers");

                    let mut offers: Vec<_> = list.offers.iter().map(|o| o.offer.clone()).collect();
                    offers.sort();
                    assert_eq!(offers, vec!["lno1offer_a", "lno1offer_b"]);

                    // Verify fields on one of them
                    let coffee = list
                        .offers
                        .iter()
                        .find(|o| o.offer == "lno1offer_a")
                        .expect("offer_a not found");
                    assert_eq!(coffee.description.as_deref(), Some("coffee"));
                    assert_eq!(coffee.amount, Some(5_000));
                    assert!(coffee.active);
                    assert_eq!(coffee.num_payments_received, 0);
                    assert_eq!(coffee.total_received, 0);

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
