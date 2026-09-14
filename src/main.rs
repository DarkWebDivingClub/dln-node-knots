use nostr_sdk::prelude::*;
use serde::Deserialize;
use std::fs;
use std::sync::Arc;

use dln_node::lightning::{LdkService, LdkServiceConfig};

#[derive(Debug, Deserialize)]
struct Config {
    node: NodeConfig,
    nostr: NostrConfig,
    wallet: WalletConfig,
    bitcoind: Option<BitcoindConfig>,
    signer: Option<SignerConfig>,
}

#[derive(Debug, Deserialize)]
struct NodeConfig {
    network: String,
    listening_port: u16,
    data_dir: String,
    #[serde(default)]
    alias: Option<String>,
}

#[derive(Debug, Deserialize)]
struct NostrConfig {
    relay: String,
    private_key: String,
    /// Whose grants this node accepts. **Required, and an empty list
    /// answers nothing** — absent configuration fails closed rather than
    /// being read as "any owner", which is dln-node#1.
    #[serde(default)]
    owners: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct WalletConfig {
    max_channel_size_sats: u64,
    min_channel_size_sats: u64,
    auto_accept_channels: bool,
}

#[derive(Debug, Deserialize)]
struct BitcoindConfig {
    rpc_host: String,
    rpc_port: u16,
    rpc_user: String,
    rpc_password: String,
}

#[derive(Debug, Deserialize)]
struct SignerConfig {
    transport: String,
    relay: Option<String>,
    nsec: Option<String>,
    signer_pubkey: Option<String>,
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::from_default_env()
        )
        .init();

    let contents = fs::read_to_string("config.toml").expect("Failed to read config.toml");
    let config: Config = toml::from_str(&contents).expect("Failed to parse config.toml");

    println!("Loaded config:");
    println!("  Network:        {}", config.node.network);
    println!("  Listening port: {}", config.node.listening_port);
    println!("  Data dir:       {}", config.node.data_dir);
    println!("  Relay:          {}", config.nostr.relay);
    println!(
        "  Max channel:    {} sats",
        config.wallet.max_channel_size_sats
    );
    println!(
        "  Min channel:    {} sats",
        config.wallet.min_channel_size_sats
    );
    println!("  Auto accept:    {}", config.wallet.auto_accept_channels);

    let keys = match Keys::parse(&config.nostr.private_key) {
        Ok(keys) => {
            println!("Using keys from config");
            keys
        }
        Err(_) => {
            let keys = Keys::generate();
            println!("Generated new keys (config key invalid)");
            println!("  Public key: {}", keys.public_key().to_bech32()?);
            keys
        }
    };

    let owners: Vec<nostr_sdk::prelude::PublicKey> = config
        .nostr
        .owners
        .iter()
        .map(|o| {
            nostr_sdk::prelude::PublicKey::parse(o)
                .unwrap_or_else(|e| panic!("nostr.owners: {o} is not a public key: {e}"))
        })
        .collect();
    if owners.is_empty() {
        eprintln!(
            "nostr.owners is empty: this node will accept no grants and \
             answer nothing. Set it to the owner's public key."
        );
    }
    println!("  Owners:         {}", owners.len());

    let bitcoind = config.bitcoind.as_ref().expect(
        "bitcoind configuration is required: this node serves a Lightning \
         node and has nothing to answer with when there is none",
    );
    let signer_transport = config
        .signer
        .as_ref()
        .map(|s| s.transport.as_str())
        .unwrap_or("embedded");
    let ldk_cfg = LdkServiceConfig {
        network: config.node.network.clone(),
        bitcoind_rpc_host: bitcoind.rpc_host.clone(),
        bitcoind_rpc_port: bitcoind.rpc_port,
        bitcoind_rpc_user: bitcoind.rpc_user.clone(),
        bitcoind_rpc_password: bitcoind.rpc_password.clone(),
        ldk_storage_dir: config.node.data_dir.clone(),
        ldk_listen_addr: Some(format!("0.0.0.0:{}", config.node.listening_port)),
        node_alias: config.node.alias.clone(),
        signer_transport: signer_transport.to_string(),
        signer_relay: config.signer.as_ref().and_then(|s| s.relay.clone()),
        signer_nsec: config.signer.as_ref().and_then(|s| s.nsec.clone()),
        signer_pubkey: config.signer.as_ref().and_then(|s| s.signer_pubkey.clone()),
    };
    let ldk = LdkService::start_from_config(&ldk_cfg).expect("Failed to start LDK service");

    println!("Serving. Press Ctrl+C to stop.\n");
    let service = dln_node::service::run(
        dln_node::service::NodeConfig {
            keys,
            relays: vec![config.nostr.relay.clone()],
            owners,
            alias: config.node.alias.clone(),
        },
        ldk,
    );

    tokio::select! {
        r = service => r.expect("service stopped"),
        _ = tokio::signal::ctrl_c() => println!("stopping"),
    }

    Ok(())
}
