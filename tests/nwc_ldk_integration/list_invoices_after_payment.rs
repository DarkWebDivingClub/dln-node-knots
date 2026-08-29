use std::net::TcpListener;
use std::str::FromStr;
use std::time::Duration;

use dln_node::lightning::{LdkService, LdkServiceConfig};
use dln_node::{clear_usage_profiles, run_nwc_service_with_ldk, set_relay_pubkey, UsageProfile};
use ldk_node::bitcoin::Network;
use ldk_node::lightning::ln::msgs::SocketAddress;
use ldk_node::lightning_invoice::Bolt11Invoice;
use ldk_node::Builder;
use nostr_sdk::prelude::*;
use nwc::nostr::nips::nip47::{
    ListInvoicesRequest, MakeInvoiceRequest, NostrWalletConnectUri, Request, Response,
};

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

fn free_local_port() -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind ephemeral port");
    let port = listener.local_addr().expect("read local addr").port();
    drop(listener);
    port
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
    let request_id = request_event.id;
    nwc_client
        .send_event(&request_event)
        .await
        .expect("failed to publish NWC request");

    let timeout = Duration::from_secs(45);
    let uri_clone = uri.clone();
    tokio::time::timeout(timeout, async {
        let mut notifications = nwc_client.notifications();
        while let Some(notification) = notifications.next().await {
            if let ClientNotification::Event { event, .. } = notification {
                let event = event.as_ref();
                if event.kind == Kind::WalletConnectResponse && event.pubkey == service_pubkey {
                    // Only process responses that reference our request (e-tag)
                    let refs_our_request = event.tags.iter().any(|tag| {
                        let parts = tag.as_slice();
                        parts.get(0).map(|v| v.as_str()) == Some("e")
                            && parts.get(1).map(|v| v.as_str())
                                == Some(&request_id.to_string())
                    });
                    if !refs_our_request {
                        continue;
                    }
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

/// Full e2e test: NWC node creates invoice via make_invoice, external payer pays it,
/// then list_invoices returns the settled payment.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn list_invoices_after_payment() -> Result<()> {
    let _guard = test_guard();
    clear_usage_profiles();

    let (_relay_container, relay_url) = start_relay().await;
    let relay_pubkey = shared_relay_pubkey();
    set_relay_pubkey(relay_pubkey.clone());

    let bitcoind = crate::nwc_ldk_integration_suite::common::bitcoind::BitcoindHarness::start().await;
    let miner_address = bitcoind.get_new_address().await;
    bitcoind.mine_blocks(101, &miner_address).await;

    // NWC-backed node (receiver) — this is the node under test
    let receiver_port = free_local_port();
    let receiver_cfg = LdkServiceConfig {
        network: "regtest".to_string(),
        bitcoind_rpc_host: bitcoind.rpc_host().to_string(),
        bitcoind_rpc_port: bitcoind.rpc_port(),
        bitcoind_rpc_user: bitcoind.rpc_user().to_string(),
        bitcoind_rpc_password: bitcoind.rpc_password().to_string(),
        ldk_storage_dir: unique_storage_dir("nwc-ldk-list-inv-receiver"),
        ldk_listen_addr: Some(format!("127.0.0.1:{receiver_port}")),
        node_alias: None,
        signer_transport: "embedded".to_string(),
        signer_relay: None,
        signer_nsec: None,
        signer_pubkey: None,
    };
    let ldk_service = LdkService::start_from_config(&receiver_cfg).expect("receiver ldk service starts");

    // External payer node (raw ldk-node, not NWC-backed)
    let payer_port = free_local_port();
    let payer_socket = SocketAddress::from_str(&format!("127.0.0.1:{payer_port}"))
        .expect("valid payer socket");
    let mut payer_builder = Builder::new();
    payer_builder.set_network(Network::Regtest);
    payer_builder.set_chain_source_bitcoind_rpc(
        bitcoind.rpc_host().to_string(),
        bitcoind.rpc_port(),
        bitcoind.rpc_user().to_string(),
        bitcoind.rpc_password().to_string(),
    );
    payer_builder
        .set_listening_addresses(vec![payer_socket])
        .expect("set payer listen addr");
    payer_builder.set_storage_dir_path(unique_storage_dir("nwc-ldk-list-inv-payer"));
    let payer = payer_builder.build().expect("build payer node");
    payer.start().expect("start payer node");

    // Fund payer so it can open a channel to the NWC receiver
    let payer_funding_addr = payer
        .onchain_payment()
        .new_address()
        .expect("payer funding address")
        .to_string();
    bitcoind.send_to_address(&payer_funding_addr, 0.05).await;
    bitcoind.mine_blocks(1, &miner_address).await;

    let funding_timeout = Duration::from_secs(20);
    let funding_start = tokio::time::Instant::now();
    loop {
        payer.sync_wallets().expect("payer sync");
        if payer.list_balances().spendable_onchain_balance_sats >= 5_000_000 {
            break;
        }
        assert!(funding_start.elapsed() <= funding_timeout, "payer funding not visible in time");
        tokio::time::sleep(Duration::from_millis(200)).await;
    }

    // Open channel payer → receiver (NWC node)
    let receiver_socket_addr = SocketAddress::from_str(&format!("127.0.0.1:{receiver_port}"))
        .expect("receiver socket");
    payer
        .open_channel(
            ldk_node::bitcoin::secp256k1::PublicKey::from_str(&ldk_service.node_id())
                .expect("parse receiver node id"),
            receiver_socket_addr,
            2_000_000,
            None,
            None,
        )
        .expect("payer opens channel to receiver");

    bitcoind.mine_blocks(6, &miner_address).await;

    // Wait for channel to be ready
    let channel_timeout = Duration::from_secs(40);
    let channel_start = tokio::time::Instant::now();
    loop {
        payer.sync_wallets().expect("payer sync after open");
        ldk_service.sync_wallets().expect("receiver sync after open");

        // Drain events on both sides
        while payer.next_event().is_some() {
            payer.event_handled().expect("payer event handled");
        }

        let payer_ready = payer
            .list_channels()
            .iter()
            .any(|c| c.counterparty_node_id.to_string() == ldk_service.node_id() && c.is_channel_ready);
        let receiver_ready = ldk_service.has_ready_channel_with(
            &payer.node_id().to_string(),
        );

        if payer_ready && receiver_ready {
            break;
        }

        assert!(channel_start.elapsed() <= channel_timeout, "channel not ready in time");
        bitcoind.mine_blocks(1, &miner_address).await;
        tokio::time::sleep(Duration::from_millis(250)).await;
    }

    // Start NWC service on the receiver node
    let service_keys = Keys::generate();
    let service_pubkey = service_keys.public_key();
    let _service_client =
        run_nwc_service_with_ldk(service_keys, &relay_url, ldk_service.clone(), None).await?;

    tokio::time::sleep(Duration::from_secs(1)).await;

    // Set up NWC client
    let client_secret = Keys::generate().secret_key().clone();
    let relay = RelayUrl::parse(&relay_url)?;
    let uri = NostrWalletConnectUri::new(service_pubkey, vec![relay], client_secret.clone(), None);

    let client_keys = Keys::new(client_secret);
    let client_pubkey = client_keys.public_key();

    let profile = UsageProfile {
        quota: None,
        methods: None, // all methods allowed
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

    // Step 1: Create invoice via NWC make_invoice
    let make_inv_response = send_request_and_read_response(
        &nwc_client,
        &uri,
        service_pubkey,
        Request::make_invoice(MakeInvoiceRequest {
            amount: 100_000,
            description: Some("e2e test payment".to_string()),
            description_hash: None,
            expiry: Some(3600),
        }),
    )
    .await;
    let invoice_result = make_inv_response
        .to_make_invoice()
        .expect("make_invoice should succeed");
    let bolt11 = Bolt11Invoice::from_str(&invoice_result.invoice).expect("parse bolt11");

    // Step 2: External payer pays the invoice
    payer
        .bolt11_payment()
        .send(&bolt11, None)
        .expect("payer sends payment");

    // Wait for payment to settle
    let pay_timeout = Duration::from_secs(30);
    let pay_start = tokio::time::Instant::now();
    loop {
        let payments = ldk_service.list_payments_filtered(
            None, None, None, None, None,
            Some(dln_node::lightning::PaymentDirection::Inbound),
            None,
        );
        let settled = payments.iter().any(|p| {
            p.status == dln_node::lightning::PaymentStatus::Succeeded
                && p.amount_msat == Some(100_000)
        });
        if settled {
            break;
        }
        assert!(pay_start.elapsed() <= pay_timeout, "payment did not settle in time");
        tokio::time::sleep(Duration::from_millis(100)).await;
    }

    // Step 3: Call list_invoices via NWC and verify the payment appears
    let list_response = send_request_and_read_response(
        &nwc_client,
        &uri,
        service_pubkey,
        Request::list_invoices(ListInvoicesRequest {
            from: None,
            until: None,
            limit: None,
            offset: None,
            state: None,
        }),
    )
    .await;
    let list_result = list_response
        .to_list_invoices()
        .expect("list_invoices should succeed");

    assert!(
        !list_result.invoices.is_empty(),
        "list_invoices should return at least one invoice after payment"
    );

    // Find the settled invoice matching our amount
    let settled_invoices: Vec<_> = list_result
        .invoices
        .iter()
        .filter(|inv| inv.state == "settled" && inv.amount == 100_000)
        .collect();
    assert_eq!(
        settled_invoices.len(),
        1,
        "expected exactly one settled invoice for 100_000 msat, got {}",
        settled_invoices.len()
    );

    let inv = &settled_invoices[0];
    assert!(!inv.payment_hash.is_empty(), "payment_hash should be present");
    assert!(inv.preimage.is_some(), "preimage should be present for settled invoice");
    assert!(inv.settled_at.is_some(), "settled_at should be present");

    // Step 4: Filter by state=settled
    let settled_response = send_request_and_read_response(
        &nwc_client,
        &uri,
        service_pubkey,
        Request::list_invoices(ListInvoicesRequest {
            from: None,
            until: None,
            limit: None,
            offset: None,
            state: Some("settled".to_string()),
        }),
    )
    .await;
    let settled_result = settled_response
        .to_list_invoices()
        .expect("list_invoices with state=settled should succeed");
    assert!(
        settled_result.invoices.iter().all(|inv| inv.state == "settled"),
        "all invoices should be settled when filtering by state=settled"
    );

    payer.stop().expect("payer stops");
    ldk_service.stop().expect("receiver ldk service stops");

    Ok(())
}
