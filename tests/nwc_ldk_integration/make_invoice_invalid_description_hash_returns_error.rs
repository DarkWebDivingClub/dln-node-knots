use std::time::Duration;

use dln_node::lightning::{LdkService, LdkServiceConfig};
use dln_node::{clear_usage_profiles, run_nwc_service_with_ldk, set_relay_pubkey, UsageProfile};
use nostr_sdk::prelude::*;
use nwc::nostr::nips::nip47::{MakeInvoiceRequest, NostrWalletConnectUri, Request, Response};

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

async fn send_request_and_read_response(
    nwc_client: &Client,
    uri: &NostrWalletConnectUri,
    service_pubkey: PublicKey,
    request: Request,
) -> Response {
    let request_event = request
        .to_event(uri)
        .expect("failed to create NWC request event");
    nwc_client
        .send_event(&request_event)
        .await
        .expect("failed to publish NWC request");

    let timeout = Duration::from_secs(10);
    let uri_clone = uri.clone();
    tokio::time::timeout(timeout, async {
        let mut notifications = nwc_client.notifications();
        while let Some(notification) = notifications.next().await {
            if let ClientNotification::Event { event, .. } = notification {
                let event = event.as_ref();
                if event.kind == Kind::WalletConnectResponse && event.pubkey == service_pubkey {
                    return Response::from_event(&uri_clone, event)
                        .expect("failed to decrypt NWC response");
                }
            }
        }
        panic!("notification stream ended before response");
    })
    .await
    .expect("timeout waiting for NWC response")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn make_invoice_invalid_description_hash_returns_error() -> Result<()> {
    let _guard = test_guard();
    clear_usage_profiles();

    let (_relay_container, relay_url) = start_relay().await;
    let relay_pubkey = shared_relay_pubkey();
    set_relay_pubkey(relay_pubkey.clone());

    let bitcoind = crate::nwc_ldk_integration_suite::common::bitcoind::BitcoindHarness::start().await;

    let ldk_cfg = LdkServiceConfig {
        network: "regtest".to_string(),
        bitcoind_rpc_host: bitcoind.rpc_host().to_string(),
        bitcoind_rpc_port: bitcoind.rpc_port(),
        bitcoind_rpc_user: bitcoind.rpc_user().to_string(),
        bitcoind_rpc_password: bitcoind.rpc_password().to_string(),
        ldk_storage_dir: unique_storage_dir("nwc-ldk-make-invoice-invalid-description-hash"),
        ldk_listen_addr: None,
        node_alias: None,
        signer_transport: "embedded".to_string(),
        signer_relay: None,
        signer_nsec: None,
        signer_pubkey: None,
    };
    let ldk_service = LdkService::start_from_config(&ldk_cfg).expect("ldk service should start");

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

    let request = Request::make_invoice(MakeInvoiceRequest {
        amount: 123_000,
        description: Some("invoice from nwc".to_string()),
        description_hash: Some("ab".repeat(32)),
        expiry: Some(3600),
    });

    let response = send_request_and_read_response(&nwc_client, &uri, service_pubkey, request).await;
    let err = response
        .error
        .expect("expected make_invoice to fail with unsupported description_hash");
    assert_eq!(err.code, nwc::nostr::nips::nip47::ErrorCode::Other);
    assert!(
        err.message
            .starts_with("ldk make_invoice failed: invalid invoice request:"),
        "unexpected error message: {}",
        err.message
    );

    ldk_service.stop().expect("ldk service should stop cleanly");

    Ok(())
}
