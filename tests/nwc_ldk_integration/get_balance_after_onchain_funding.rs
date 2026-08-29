use std::time::Duration;

use dln_node::lightning::{LdkService, LdkServiceConfig};
use dln_node::{clear_usage_profiles, run_nwc_service_with_ldk, set_relay_pubkey, UsageProfile};
use nostr_sdk::prelude::*;
use nwc::nostr::nips::nip47::{NostrWalletConnectUri, Request, Response};

use crate::nwc_ldk_integration_suite::common::{grant_usage_profile, start_relay, test_guard};
use crate::nwc_ldk_integration_suite::shared_relay_pubkey;

fn unique_storage_dir(prefix: &str) -> String {
    format!(
        "/tmp/{prefix}-{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock should be monotonic")
            .as_nanos()
    )
}

async fn send_get_balance_and_read_response(
    nwc_client: &Client,
    uri: &NostrWalletConnectUri,
    service_pubkey: PublicKey,
) -> Response {
    let request_event = Request::get_balance()
        .to_event(uri)
        .expect("failed to create get_balance request");
    nwc_client
        .send_event(&request_event)
        .await
        .expect("failed to publish get_balance request");

    let timeout = Duration::from_secs(10);
    let uri_clone = uri.clone();
    tokio::time::timeout(timeout, async {
        let mut notifications = nwc_client.notifications();
        while let Some(notification) = notifications.next().await {
            if let ClientNotification::Event { event, .. } = notification {
                let event = event.as_ref();
                if event.kind == Kind::WalletConnectResponse && event.pubkey == service_pubkey {
                    return Response::from_event(&uri_clone, event)
                        .expect("failed to decrypt get_balance response");
                }
            }
        }
        panic!("notification stream ended before get_balance response");
    })
    .await
    .expect("timeout waiting for get_balance response")
}

/// Happy-path E2E test:
/// fund LDK wallet on regtest and verify NWC get_balance reflects it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn get_balance_after_onchain_funding() -> Result<()> {
    let _guard = test_guard();
    clear_usage_profiles();

    let (_relay_container, relay_url) = start_relay().await;
    let relay_pubkey = shared_relay_pubkey();
    set_relay_pubkey(relay_pubkey.clone());

    let bitcoind = crate::nwc_ldk_integration_suite::common::bitcoind::BitcoindHarness::start().await;
    let miner_address = bitcoind.get_new_address().await;
    bitcoind.mine_blocks(101, &miner_address).await;

    let ldk_cfg = LdkServiceConfig {
        network: "regtest".to_string(),
        bitcoind_rpc_host: bitcoind.rpc_host().to_string(),
        bitcoind_rpc_port: bitcoind.rpc_port(),
        bitcoind_rpc_user: bitcoind.rpc_user().to_string(),
        bitcoind_rpc_password: bitcoind.rpc_password().to_string(),
        ldk_storage_dir: unique_storage_dir("nwc-ldk-e2e"),
        ldk_listen_addr: None,
        node_alias: None,
        signer_transport: "embedded".to_string(),
        signer_relay: None,
        signer_nsec: None,
        signer_pubkey: None,
    };
    let ldk_service = LdkService::start_from_config(&ldk_cfg).expect("ldk service should start");

    let funding_address = ldk_service
        .new_onchain_address()
        .expect("ldk address generation should work");
    bitcoind.send_to_address(&funding_address, 1.0).await;
    bitcoind.mine_blocks(1, &miner_address).await;

    let service_keys = Keys::generate();
    let service_pubkey = service_keys.public_key();
    let _service_client =
        run_nwc_service_with_ldk(service_keys, &relay_url, ldk_service.clone(), None).await?;

    tokio::time::sleep(Duration::from_secs(1)).await;

    let client_secret = Keys::generate().secret_key().clone();
    let relay = RelayUrl::parse(&relay_url)?;
    let uri = NostrWalletConnectUri::new(service_pubkey, vec![relay], client_secret.clone(), None);

    let client_keys = Keys::new(client_secret);
    let client_pubkey = client_keys.public_key();

    // Unrestricted profile: allow all methods, no method/quota rate policies.
    let profile = UsageProfile {
        quota: None,
        methods: None,
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

    nwc_client
        .subscribe(
            Filter::new()
                .kind(Kind::WalletConnectResponse)
                .author(service_pubkey),
        )
        .await?;

    let expected_msat = 100_000_000_000u64;
    let timeout = Duration::from_secs(20);
    let start = tokio::time::Instant::now();
    loop {
        let response = send_get_balance_and_read_response(&nwc_client, &uri, service_pubkey).await;
        if let Some(err) = response.error {
            panic!("get_balance returned error: {:?}", err);
        }

        let balance = response
            .to_get_balance()
            .expect("response should decode as get_balance")
            .balance;

        if balance == expected_msat {
            break;
        }

        if start.elapsed() > timeout {
            panic!(
                "balance did not reach exact expected value in time, observed={balance}, expected={expected_msat}"
            );
        }

        tokio::time::sleep(Duration::from_millis(200)).await;
    };

    ldk_service.stop().expect("ldk service should stop cleanly");

    Ok(())
}
