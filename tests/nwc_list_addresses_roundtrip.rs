use nostr_sdk::prelude::*;
use nwc::nostr::nips::nip47::{
    ListAddressesRequest, Method, NostrWalletConnectUri, Request, Response,
};
use std::collections::HashMap;
use std::time::Duration;

use dln_node::{
    clear_addresses_for_testing, clear_usage_profiles, seed_address_for_testing,
    seed_address_transaction_for_testing, set_relay_pubkey, MethodAccessRule, UsageProfile,
};

mod common;
use common::{grant_usage_profile, start_relay, test_guard};

/// End-to-end test: seed addresses with transactions, send list_addresses, verify totals.
#[tokio::test]
async fn test_nwc_list_addresses_returns_seeded_addresses() -> Result<()> {
    let _guard = test_guard();
    clear_usage_profiles();
    clear_addresses_for_testing();
    let (_container, relay_url) = start_relay().await;

    let relay_pubkey = Keys::generate().public_key();
    set_relay_pubkey(relay_pubkey.clone());

    // Seed two addresses with transactions
    seed_address_for_testing("bcrt1qaddr_one".to_string());
    seed_address_transaction_for_testing("bcrt1qaddr_one", "tx1".to_string(), 50_000, 1700000000);
    seed_address_transaction_for_testing("bcrt1qaddr_one", "tx2".to_string(), 30_000, 1700000100);

    seed_address_for_testing("bcrt1qaddr_two".to_string());
    seed_address_transaction_for_testing("bcrt1qaddr_two", "tx3".to_string(), 100_000, 1700000200);

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
        Method::ListAddresses,
        MethodAccessRule { access_rate: None },
    );
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

    let params = ListAddressesRequest {
        limit: None,
        offset: None,
    };
    let request_event = Request::list_addresses(params)
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
                        .to_list_addresses()
                        .expect("Response was not a valid list_addresses");

                    assert_eq!(list.addresses.len(), 2, "Expected 2 addresses");

                    let addr_one = list
                        .addresses
                        .iter()
                        .find(|a| a.address == "bcrt1qaddr_one")
                        .expect("addr_one not found");
                    assert_eq!(
                        addr_one.total_received, 80_000,
                        "addr_one should have 50k + 30k = 80k sats"
                    );

                    let addr_two = list
                        .addresses
                        .iter()
                        .find(|a| a.address == "bcrt1qaddr_two")
                        .expect("addr_two not found");
                    assert_eq!(
                        addr_two.total_received, 100_000,
                        "addr_two should have 100k sats"
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
        Err(_) => panic!("Timeout: did not receive NWC response within 10 seconds"),
    }
}
