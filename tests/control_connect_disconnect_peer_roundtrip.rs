//! Connecting to a peer, seeing it listed, and disconnecting.
//!
//! **Ported by 25.3.** It used to build NIP-XX requests by hand, encrypt
//! them, publish a grant with `MethodAccessRule { access_rate }` and read
//! responses off a notification stream. It now drives the node with the
//! crate's own NNC client, which is what a controller would do.
//!
//! It survives the cull because nothing else covers these three methods.
//! `dln-node-e2e-test/two_dln_nodes` connects only as a side effect of
//! opening a channel, and never disconnects.

mod common;

use std::sync::Arc;
use std::time::Duration;

use dln_node::lightning::{LdkService, LdkServiceConfig};
use nostr_ln::nnc::client::NostrNodeControl;
use nostr_ln::nnc::methods::{ConnectPeerRequest, DisconnectPeerRequest};
use nostr_sdk::prelude::*;

use common::bitcoind::BitcoindHarness;
use common::{start_relay, test_guard};

fn storage(prefix: &str) -> String {
    format!(
        "/tmp/{prefix}-{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_nanos()
    )
}

async fn node(b: &BitcoindHarness, prefix: &str, port: Option<u16>) -> Arc<LdkService> {
    let cfg = LdkServiceConfig {
        network: "regtest".to_string(),
        bitcoind_rpc_host: b.rpc_host().to_string(),
        bitcoind_rpc_port: b.rpc_port(),
        bitcoind_rpc_user: b.rpc_user().to_string(),
        bitcoind_rpc_password: b.rpc_password().to_string(),
        ldk_storage_dir: storage(prefix),
        ldk_listen_addr: port.map(|p| format!("127.0.0.1:{p}")),
        node_alias: None,
        signer_transport: "embedded".to_string(),
        signer_relay: None,
        signer_nsec: None,
        signer_pubkey: None,
    };
    LdkService::start_from_config(&cfg).expect("ldk starts")
}

fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .expect("bind")
        .local_addr()
        .expect("addr")
        .port()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn connect_list_and_disconnect_a_peer() -> Result<()> {
    let _guard = test_guard();
    let (_relay, relay_url) = start_relay().await;
    let b = BitcoindHarness::start().await;

    let peer_port = free_port();
    let peer = node(&b, "peer-target", Some(peer_port)).await;
    let peer_id = peer.node_id();

    let ldk = node(&b, "peer-driver", None).await;

    // The node, served by the crate.
    let node_keys = Keys::generate();
    let node_pubkey = node_keys.public_key();
    let owner = Keys::generate();
    let controller = Keys::generate();

    let handle = {
        let cfg = dln_node::service::NodeConfig {
            keys: node_keys.clone(),
            relays: vec![relay_url.clone()],
            // **Required now.** An empty list accepts no grants and the
            // node answers nothing — dln-node#1 closed by construction.
            owners: vec![owner.public_key()],
            alias: None,
        };
        tokio::spawn(async move {
            let _ = dln_node::service::run(cfg, ldk).await;
        })
    };
    tokio::time::sleep(Duration::from_secs(1)).await;

    // A grant, written as a conforming issuer writes it — `rate`, not
    // `access_rate`, which is what the old harness emitted.
    common::grant_usage_profile(
        &owner,
        &relay_url,
        node_pubkey,
        controller.public_key(),
        r#"{"control":{"OTHERS":{}}}"#,
    )
    .await?;
    tokio::time::sleep(Duration::from_secs(1)).await;

    let uri = format!(
        "nostr+nodecontrol://{}?relay={}",
        node_pubkey.to_hex(),
        urlencode(&relay_url)
    );
    let nnc = NostrNodeControl::new(uri.parse()?, controller.clone());

    let connected = nnc
        .connect_peer(ConnectPeerRequest {
            pubkey: peer_id.clone(),
            host: Some(format!("127.0.0.1:{peer_port}")),
        })
        .await?;
    let _ = connected;

    let peers = nnc.list_peers().await?;
    assert!(
        peers.peers.iter().any(|p| p.pubkey == peer_id && p.connected),
        "the peer we just connected to should be listed as connected: {:?}",
        peers.peers
    );

    nnc.disconnect_peer(DisconnectPeerRequest { pubkey: peer_id.clone() }).await?;
    tokio::time::sleep(Duration::from_secs(1)).await;

    let peers = nnc.list_peers().await?;
    assert!(
        !peers.peers.iter().any(|p| p.pubkey == peer_id && p.connected),
        "and not as connected afterwards: {:?}",
        peers.peers
    );

    handle.abort();
    Ok(())
}

fn urlencode(s: &str) -> String {
    s.bytes()
        .map(|c| match c {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                (c as char).to_string()
            }
            other => format!("%{other:02X}"),
        })
        .collect()
}
