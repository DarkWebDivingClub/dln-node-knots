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
async fn control_connect_then_disconnect_peer() -> Result<()> {
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

    let ldk_cfg = LdkServiceConfig {
        network: "regtest".to_string(),
        bitcoind_rpc_host: bitcoind.rpc_host().to_string(),
        bitcoind_rpc_port: bitcoind.rpc_port(),
        bitcoind_rpc_user: bitcoind.rpc_user().to_string(),
        bitcoind_rpc_password: bitcoind.rpc_password().to_string(),
        ldk_storage_dir: unique_storage_dir("control-peer-node-a"),
        ldk_listen_addr: Some(format!("127.0.0.1:{port_a}")),
        node_alias: None,
        signer_transport: "embedded".to_string(),
        signer_relay: None,
        signer_nsec: None,
        signer_pubkey: None,
    };
    let ldk_service = LdkService::start_from_config(&ldk_cfg).expect("ldk service should start");

    let mut builder_b = Builder::new();
    builder_b.set_network(Network::Regtest);
    builder_b.set_chain_source_bitcoind_rpc(
        bitcoind.rpc_host().to_string(),
        bitcoind.rpc_port(),
        bitcoind.rpc_user().to_string(),
        bitcoind.rpc_password().to_string(),
    );
    builder_b
        .set_listening_addresses(vec![addr_b.clone()])
        .expect("set node B listening address");
    builder_b.set_storage_dir_path(unique_storage_dir("control-peer-node-b"));
    let node_b = builder_b.build().expect("build node B");
    node_b.start().expect("start node B");

    let service_keys = Keys::generate();
    let service_pubkey = service_keys.public_key();
    let _service_client =
        run_nwc_service_with_ldk(service_keys, &relay_url, ldk_service.clone(), None).await?;
    tokio::time::sleep(Duration::from_secs(1)).await;

    let controller_keys = Keys::generate();
    let controller_secret = controller_keys.secret_key().clone();
    let controller_pubkey = controller_keys.public_key();

    let mut control = HashMap::new();
    control.insert("connect_peer".to_string(), MethodAccessRule { access_rate: None });
    control.insert("disconnect_peer".to_string(), MethodAccessRule { access_rate: None });
    control.insert("list_peers".to_string(), MethodAccessRule { access_rate: None });
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

    let node_b_pubkey = node_b.node_id().to_string();
    let connect_response = send_control_request(
        &controller,
        &controller_secret,
        service_pubkey,
        json!({
            "method": "connect_peer",
            "params": {
                "pubkey": node_b_pubkey,
                "host": format!("127.0.0.1:{port_b}")
            }
        }),
    )
    .await?;
    assert!(
        connect_response["error"].is_null(),
        "connect_peer returned error: {:?}",
        connect_response
    );
    assert_eq!(connect_response["result"]["status"], "connected");

    let list_response = send_control_request(
        &controller,
        &controller_secret,
        service_pubkey,
        json!({
            "method": "list_peers",
            "params": {}
        }),
    )
    .await?;
    assert!(list_response["error"].is_null(), "list_peers errored: {:?}", list_response);
    let peers = list_response["result"]["peers"]
        .as_array()
        .expect("list_peers result.peers should be array");
    assert!(
        peers.iter().any(|p| {
            p["pubkey"].as_str().map(|id| id == node_b.node_id().to_string()).unwrap_or(false)
        }),
        "expected list_peers to include node B"
    );

    let disconnect_response = send_control_request(
        &controller,
        &controller_secret,
        service_pubkey,
        json!({
            "method": "disconnect_peer",
            "params": {
                "pubkey": node_b.node_id().to_string()
            }
        }),
    )
    .await?;
    assert!(
        disconnect_response["error"].is_null(),
        "disconnect_peer returned error: {:?}",
        disconnect_response
    );
    assert_eq!(disconnect_response["result"]["status"], "disconnected");

    node_b.stop().expect("stop node B");
    ldk_service.stop().expect("stop node A service");
    Ok(())
}
