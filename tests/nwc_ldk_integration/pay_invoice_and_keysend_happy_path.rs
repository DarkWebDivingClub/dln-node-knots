use std::net::TcpListener;
use std::str::FromStr;
use std::time::Duration;

use dln_node::lightning::{LdkService, LdkServiceConfig};
use dln_node::{clear_usage_profiles, run_nwc_service_with_ldk, set_relay_pubkey, UsageProfile};
use ldk_node::bitcoin::Network;
use ldk_node::lightning::ln::msgs::SocketAddress;
use ldk_node::lightning_invoice::{Bolt11InvoiceDescription, Description};
use ldk_node::{Builder, Event};
use nostr_sdk::prelude::*;
use nwc::nostr::nips::nip47::{
    NostrWalletConnectUri, PayInvoiceRequest, PayKeysendRequest, Request, Response,
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

fn wait_for_receiver_payment(receiver: &ldk_node::Node, expected_amount_msat: u64, timeout: Duration) {
    let start = std::time::Instant::now();
    loop {
        if let Some(event) = receiver.next_event() {
            let matched = matches!(
                event,
                Event::PaymentReceived { amount_msat, .. } if amount_msat == expected_amount_msat
            );
            receiver
                .event_handled()
                .expect("mark receiver event handled");
            if matched {
                return;
            }
        }

        assert!(start.elapsed() <= timeout, "receiver did not observe expected payment in time");
        std::thread::sleep(Duration::from_millis(100));
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pay_invoice_and_keysend_happy_path() -> Result<()> {
    let _guard = test_guard();
    clear_usage_profiles();

    let (_relay_container, relay_url) = start_relay().await;
    let relay_pubkey = shared_relay_pubkey();
    set_relay_pubkey(relay_pubkey.clone());

    let bitcoind = crate::nwc_ldk_integration_suite::common::bitcoind::BitcoindHarness::start().await;
    let miner_address = bitcoind.get_new_address().await;
    bitcoind.mine_blocks(101, &miner_address).await;

    let payer_port = free_local_port();
    let payer_addr = format!("127.0.0.1:{payer_port}");

    let payer_cfg = LdkServiceConfig {
        network: "regtest".to_string(),
        bitcoind_rpc_host: bitcoind.rpc_host().to_string(),
        bitcoind_rpc_port: bitcoind.rpc_port(),
        bitcoind_rpc_user: bitcoind.rpc_user().to_string(),
        bitcoind_rpc_password: bitcoind.rpc_password().to_string(),
        ldk_storage_dir: unique_storage_dir("nwc-ldk-payer"),
        ldk_listen_addr: Some(payer_addr.clone()),
        node_alias: None,
        signer_transport: "embedded".to_string(),
        signer_relay: None,
        signer_nsec: None,
        signer_pubkey: None,
    };
    let ldk_service = LdkService::start_from_config(&payer_cfg).expect("payer ldk service starts");

    // Receiver node (direct ldk-node) for end-to-end payment validation.
    let mut receiver_builder = Builder::new();
    receiver_builder.set_network(Network::Regtest);
    receiver_builder.set_chain_source_bitcoind_rpc(
        bitcoind.rpc_host().to_string(),
        bitcoind.rpc_port(),
        bitcoind.rpc_user().to_string(),
        bitcoind.rpc_password().to_string(),
    );
    let receiver_port = free_local_port();
    let receiver_socket = SocketAddress::from_str(&format!("127.0.0.1:{receiver_port}"))
        .expect("valid receiver socket");
    receiver_builder
        .set_listening_addresses(vec![receiver_socket])
        .expect("set receiver listen addr");
    receiver_builder.set_storage_dir_path(unique_storage_dir("nwc-ldk-receiver"));
    let receiver = receiver_builder.build().expect("build receiver node");
    receiver.start().expect("start receiver node");

    // Fund payer (NWC-backed LdkService) so it can open a channel to receiver.
    let payer_funding_addr = ldk_service
        .new_onchain_address()
        .expect("payer funding address");
    bitcoind.send_to_address(&payer_funding_addr, 0.05).await;
    bitcoind.mine_blocks(1, &miner_address).await;

    let funding_timeout = Duration::from_secs(20);
    let funding_start = tokio::time::Instant::now();
    loop {
        ldk_service.sync_wallets().expect("payer sync should succeed");
        if ldk_service
            .get_balance_msat()
            .expect("payer balance read should succeed")
            >= 5_000_000_000
        {
            break;
        }
        assert!(funding_start.elapsed() <= funding_timeout, "payer funding not visible in time");
        tokio::time::sleep(Duration::from_millis(200)).await;
    }

    ldk_service
        .open_channel(
            &receiver.node_id().to_string(),
            &format!("127.0.0.1:{receiver_port}"),
            2_000_000,
            None,
        )
        .expect("payer opens channel to receiver");

    bitcoind.mine_blocks(6, &miner_address).await;

    let channel_timeout = Duration::from_secs(40);
    let channel_start = tokio::time::Instant::now();
    loop {
        receiver.sync_wallets().expect("receiver sync should succeed");
        ldk_service.sync_wallets().expect("payer sync should succeed");

        if receiver.next_event().is_some() {
            receiver
                .event_handled()
                .expect("mark receiver channel event handled");
        }

        let receiver_ready = receiver
            .list_channels()
            .iter()
            .any(|c| c.counterparty_node_id.to_string() == ldk_service.node_id() && c.is_channel_ready);
        let payer_has_channel = ldk_service.has_channel_with(&receiver.node_id().to_string());

        if receiver_ready && payer_has_channel {
            break;
        }

        assert!(channel_start.elapsed() <= channel_timeout, "channel not ready in time");
        bitcoind.mine_blocks(1, &miner_address).await;
        tokio::time::sleep(Duration::from_millis(200)).await;
    }

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

    // 1) pay_invoice via NWC
    let invoice_desc =
        Bolt11InvoiceDescription::Direct(Description::new("receiver invoice".to_string()).unwrap());
    let invoice = receiver
        .bolt11_payment()
        .receive(200_000, &invoice_desc, 3600)
        .expect("receiver invoice generation");

    let pay_invoice_request = Request::pay_invoice(PayInvoiceRequest {
        id: None,
        invoice: invoice.to_string(),
        amount: None,
    });
    let pay_invoice_response =
        send_request_and_read_response(&nwc_client, &uri, service_pubkey, pay_invoice_request).await;
    if let Some(err) = pay_invoice_response.error {
        panic!("pay_invoice returned error: {:?}", err);
    }
    let pay_invoice_result = pay_invoice_response
        .to_pay_invoice()
        .expect("response should decode as pay_invoice");
    assert_eq!(pay_invoice_result.preimage.len(), 64);

    wait_for_receiver_payment(&receiver, 200_000, Duration::from_secs(30));

    // 2) pay_keysend via NWC
    let pay_keysend_request = Request::pay_keysend(PayKeysendRequest {
        id: None,
        amount: 150_000,
        pubkey: receiver.node_id().to_string(),
        preimage: None,
        tlv_records: Vec::new(),
    });
    let pay_keysend_response =
        send_request_and_read_response(&nwc_client, &uri, service_pubkey, pay_keysend_request).await;
    if let Some(err) = pay_keysend_response.error {
        panic!("pay_keysend returned error: {:?}", err);
    }
    let pay_keysend_result = pay_keysend_response
        .to_pay_keysend()
        .expect("response should decode as pay_keysend");
    assert_eq!(pay_keysend_result.preimage.len(), 64);

    wait_for_receiver_payment(&receiver, 150_000, Duration::from_secs(30));

    receiver.stop().expect("receiver should stop cleanly");
    ldk_service.stop().expect("payer ldk service should stop cleanly");

    Ok(())
}
