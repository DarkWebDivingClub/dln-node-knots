use std::collections::HashMap;
use std::net::TcpListener;
use std::str::FromStr;
use std::time::Duration;

use dln_node::lightning::{LdkService, LdkServiceConfig};
use dln_node::{
    clear_usage_profiles, run_nwc_service_with_ldk, set_relay_pubkey, MethodAccessRule,
    UsageProfile, CONTROL_REQUEST_KIND, CONTROL_RESPONSE_KIND,
};
use ldk_node::bitcoin::Network;
use ldk_node::lightning::ln::msgs::SocketAddress;
use ldk_node::lightning_invoice::{Bolt11Invoice, Bolt11InvoiceDescription, Description};
use ldk_node::payment::{PaymentDirection, PaymentStatus};
use ldk_node::Builder;
use nostr_sdk::prelude::*;
use nwc::nostr::nips::nip04;
use serde_json::{json, Value};

#[path = "common/mod.rs"]
mod common;

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

async fn read_control_response_event(client: &Client, service_pubkey: PublicKey) -> Event {
    let timeout = Duration::from_secs(10);
    let maybe_event = tokio::time::timeout(timeout, async {
        let mut notifications = client.notifications();
        while let Some(notification) = notifications.next().await {
            if let ClientNotification::Event { event, .. } = notification {
                let event = event.as_ref();
                if event.kind == Kind::Custom(CONTROL_RESPONSE_KIND) && event.pubkey == service_pubkey
                {
                    return Some(event.clone());
                }
            }
        }
        None
    })
    .await
    .expect("timeout waiting for control response");
    if let Some(event) = maybe_event {
        return event;
    }
    panic!("notification stream ended before control response")
}

async fn send_control_request(
    controller: &Client,
    controller_secret: &SecretKey,
    service_pubkey: PublicKey,
    payload: Value,
) -> Result<Value> {
    let encrypted = nip04::encrypt(controller_secret, &service_pubkey, payload.to_string())?;
    let request_event = EventBuilder::new(Kind::Custom(CONTROL_REQUEST_KIND), encrypted)
        .tag(Tag::public_key(service_pubkey));
    controller.send_event_builder(request_event).await?;

    let response_event = read_control_response_event(controller, service_pubkey).await;
    let decrypted = nip04::decrypt(controller_secret, &service_pubkey, &response_event.content)?;
    let response: Value = serde_json::from_str(&decrypted)?;
    Ok(response)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn alice_opens_channel_then_both_directions_pay() -> Result<()> {
    let _guard = common::test_guard();
    clear_usage_profiles();

    let (_relay_container, relay_url) = common::start_relay().await;
    let relay_pubkey = Keys::generate().public_key();
    set_relay_pubkey(relay_pubkey);

    let bitcoind = common::bitcoind::BitcoindHarness::start().await;
    let miner_address = bitcoind.get_new_address().await;
    bitcoind.mine_blocks(101, &miner_address).await;

    let port_a = free_local_port();
    let port_b = free_local_port();
    let addr_b = SocketAddress::from_str(&format!("127.0.0.1:{port_b}"))
        .expect("valid node B socket address");

    let alice_cfg = LdkServiceConfig {
        network: "regtest".to_string(),
        bitcoind_rpc_host: bitcoind.rpc_host().to_string(),
        bitcoind_rpc_port: bitcoind.rpc_port(),
        bitcoind_rpc_user: bitcoind.rpc_user().to_string(),
        bitcoind_rpc_password: bitcoind.rpc_password().to_string(),
        ldk_storage_dir: unique_storage_dir("scenario-alice"),
        ldk_listen_addr: Some(format!("127.0.0.1:{port_a}")),
        node_alias: None,
        signer_transport: "embedded".to_string(),
        signer_relay: None,
        signer_nsec: None,
        signer_pubkey: None,
    };
    let alice = LdkService::start_from_config(&alice_cfg).expect("start alice");

    let mut bob_builder = Builder::new();
    bob_builder.set_network(Network::Regtest);
    bob_builder.set_chain_source_bitcoind_rpc(
        bitcoind.rpc_host().to_string(),
        bitcoind.rpc_port(),
        bitcoind.rpc_user().to_string(),
        bitcoind.rpc_password().to_string(),
    );
    bob_builder
        .set_listening_addresses(vec![addr_b.clone()])
        .expect("set bob listen addr");
    bob_builder.set_storage_dir_path(unique_storage_dir("scenario-bob"));
    let bob = bob_builder.build().expect("build bob");
    bob.start().expect("start bob");

    // Fund Alice for channel open.
    let alice_addr = alice.new_onchain_address().expect("alice address");
    bitcoind.send_to_address(&alice_addr, 0.05).await;
    bitcoind.mine_blocks(1, &miner_address).await;

    let sync_timeout = Duration::from_secs(20);
    let sync_start = tokio::time::Instant::now();
    loop {
        alice.sync_wallets().expect("alice sync");
        if alice.get_balance_msat().expect("alice balance") >= 5_000_000_000 {
            break;
        }
        assert!(sync_start.elapsed() <= sync_timeout, "alice funding timeout");
        tokio::time::sleep(Duration::from_millis(200)).await;
    }

    // Start service on Alice and authorize controller for open/list.
    let service_keys = Keys::generate();
    let service_pubkey = service_keys.public_key();
    let _service_client = run_nwc_service_with_ldk(service_keys, &relay_url, alice.clone(), None).await?;
    tokio::time::sleep(Duration::from_secs(1)).await;

    let controller_keys = Keys::generate();
    let controller_secret = controller_keys.secret_key().clone();
    let controller_pubkey = controller_keys.public_key();
    let controller = Client::builder().signer(controller_keys).build();
    controller.add_relay(&relay_url).await?;
    controller.connect().await;
    tokio::time::sleep(Duration::from_secs(1)).await;
    controller
        .subscribe(
            Filter::new()
                .kind(Kind::Custom(CONTROL_RESPONSE_KIND))
                .author(service_pubkey),
        )
        .await?;

    let mut control = HashMap::new();
    control.insert("open_channel".to_string(), MethodAccessRule { access_rate: None });
    control.insert("list_channels".to_string(), MethodAccessRule { access_rate: None });
    let profile = UsageProfile {
        quota: None,
        methods: None,
        control: Some(control),
    };
    let owner_keys = Keys::generate();
    common::grant_usage_profile(
        &owner_keys,
        &relay_url,
        relay_pubkey,
        controller_pubkey,
        &profile,
    )
    .await?;
    tokio::time::sleep(Duration::from_secs(1)).await;

    // Open channel via control API, pushing liquidity to Bob for reverse payment.
    let bob_pubkey = bob.node_id().to_string();
    let open_response = send_control_request(
        &controller,
        &controller_secret,
        service_pubkey,
        json!({
            "method": "open_channel",
            "params": {
                "pubkey": bob_pubkey,
                "host": format!("127.0.0.1:{port_b}"),
                "amount": 2_000_000,
                "push_amount": 1_000_000u64
            }
        }),
    )
    .await?;
    assert!(open_response["error"].is_null(), "open_channel failed: {:?}", open_response);

    // Confirm funding and wait until channel is ready enough for both directions.
    bitcoind.mine_blocks(1, &miner_address).await;
    let ready_timeout = Duration::from_secs(45);
    let ready_start = tokio::time::Instant::now();
    loop {
        alice.sync_wallets().expect("alice sync after open");
        bob.sync_wallets().expect("bob sync after open");

        let bob_ready = bob
            .list_channels()
            .iter()
            .any(|c| c.counterparty_node_id.to_string() == alice.node_id() && c.is_channel_ready);
        let alice_ready = alice.has_ready_channel_with(&bob.node_id().to_string());
        if alice_ready && bob_ready {
            break;
        }

        assert!(ready_start.elapsed() <= ready_timeout, "channel did not become ready in time");
        bitcoind.mine_blocks(1, &miner_address).await;
        tokio::time::sleep(Duration::from_millis(250)).await;
    }

    // Alice -> Bob payment.
    let bob_desc = Bolt11InvoiceDescription::Direct(
        Description::new("alice-to-bob".to_string()).expect("invoice desc"),
    );
    let bob_invoice = bob
        .bolt11_payment()
        .receive(250_000, &bob_desc, 3600)
        .expect("bob invoice");
    alice
        .pay_invoice(&bob_invoice.to_string(), None)
        .expect("alice pays bob invoice");

    // Bob -> Alice payment.
    let alice_invoice = alice
        .make_invoice(150_000, Some("bob-to-alice"), None, Some(3600))
        .expect("alice creates invoice");
    let parsed_alice_invoice =
        Bolt11Invoice::from_str(&alice_invoice.invoice).expect("parse alice invoice");
    let bob_payment_id = bob
        .bolt11_payment()
        .send(&parsed_alice_invoice, None)
        .expect("bob sends payment to alice");

    let pay_timeout = Duration::from_secs(30);
    let pay_start = tokio::time::Instant::now();
    loop {
        let succeeded = bob
            .list_payments()
            .into_iter()
            .any(|p| p.id == bob_payment_id && p.direction == PaymentDirection::Outbound && p.status == PaymentStatus::Succeeded);
        if succeeded {
            break;
        }
        assert!(pay_start.elapsed() <= pay_timeout, "bob->alice payment did not succeed in time");
        tokio::time::sleep(Duration::from_millis(100)).await;
    }

    bob.stop().expect("stop bob");
    alice.stop().expect("stop alice");
    Ok(())
}
