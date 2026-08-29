use ldk_node::bitcoin::Network;
use ldk_node::lightning::ln::channelmanager::PaymentId;
use ldk_node::lightning::ln::msgs::SocketAddress;
use ldk_node::bitcoin::secp256k1::PublicKey;
use ldk_node::lightning_invoice::{Bolt11Invoice, Bolt11InvoiceDescription, Description};
use ldk_node::bitcoin::Address;
use ldk_node::lightning::offers::offer::Offer;
use ldk_node::lightning::routing::gossip::NodeId;
use ldk_node::payment::{PaymentDetails, PaymentDirection, PaymentKind, PaymentStatus};
use nwc::nostr::nips::nip47::PaymentMethod;
use ldk_node::{Builder, Node};
use serde::Serialize;
use std::fmt;
use std::str::FromStr;
use std::sync::Arc;
use std::time::Duration;

#[derive(Debug, Clone)]
pub struct LdkServiceConfig {
    pub network: String,
    pub bitcoind_rpc_host: String,
    pub bitcoind_rpc_port: u16,
    pub bitcoind_rpc_user: String,
    pub bitcoind_rpc_password: String,
    pub ldk_storage_dir: String,
    pub ldk_listen_addr: Option<String>,
    pub node_alias: Option<String>,
    /// Signer transport: "nostr" (remote VLS), "none" (plain LDK keys,
    /// no VLS), or "embedded" (in-process VLS signer, the default).
    pub signer_transport: String,
    /// Nostr relay URL (required when signer_transport = "nostr").
    pub signer_relay: Option<String>,
    /// Node-proxy's Nostr secret key in hex (required when signer_transport = "nostr").
    pub signer_nsec: Option<String>,
    /// Remote signer's Nostr public key in hex (required when signer_transport = "nostr").
    pub signer_pubkey: Option<String>,
}

impl LdkServiceConfig {
    fn parse_network(&self) -> Result<Network, LdkServiceInitError> {
        match self.network.to_lowercase().as_str() {
            "regtest" => Ok(Network::Regtest),
            "testnet" => Ok(Network::Testnet),
            "bitcoin" | "mainnet" => Ok(Network::Bitcoin),
            "signet" => Ok(Network::Signet),
            other => Err(LdkServiceInitError::InvalidNetwork {
                network: other.to_string(),
            }),
        }
    }

    fn validate(&self) -> Result<(), LdkServiceInitError> {
        if self.bitcoind_rpc_host.trim().is_empty() {
            return Err(LdkServiceInitError::InvalidConfig(
                "bitcoind_rpc_host must not be empty".to_string(),
            ));
        }
        if self.bitcoind_rpc_user.trim().is_empty() {
            return Err(LdkServiceInitError::InvalidConfig(
                "bitcoind_rpc_user must not be empty".to_string(),
            ));
        }
        if self.bitcoind_rpc_password.trim().is_empty() {
            return Err(LdkServiceInitError::InvalidConfig(
                "bitcoind_rpc_password must not be empty".to_string(),
            ));
        }
        if self.ldk_storage_dir.trim().is_empty() {
            return Err(LdkServiceInitError::InvalidConfig(
                "ldk_storage_dir must not be empty".to_string(),
            ));
        }
        Ok(())
    }
}

#[derive(Debug)]
pub enum LdkServiceInitError {
    InvalidNetwork { network: String },
    InvalidListeningAddress { address: String },
    InvalidConfig(String),
    BuildFailed(String),
    StartFailed(String),
}

impl fmt::Display for LdkServiceInitError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidNetwork { network } => {
                write!(f, "unsupported network for LdkService: {network}")
            }
            Self::InvalidListeningAddress { address } => {
                write!(f, "invalid ldk_listen_addr: {address}")
            }
            Self::InvalidConfig(msg) => write!(f, "invalid LdkService config: {msg}"),
            Self::BuildFailed(msg) => write!(f, "failed to build LdkService node: {msg}"),
            Self::StartFailed(msg) => write!(f, "failed to start LdkService node: {msg}"),
        }
    }
}

impl std::error::Error for LdkServiceInitError {}

#[derive(Debug)]
pub enum LdkServiceError {
    SyncFailed(String),
    AddressGenerationFailed(String),
    BalanceOverflow { sats: u64 },
    InvalidInvoice(String),
    InvalidInvoiceRequest(String),
    InvalidPubkey(String),
    InvalidAmount(u64),
    ChannelFailed(String),
    PeerFailed(String),
    PaymentFailed(String),
    PaymentNotFound(String),
    InvalidAddress(String),
    OfferFailed(String),
    StopFailed(String),
}

impl fmt::Display for LdkServiceError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::SyncFailed(msg) => write!(f, "ldk wallet sync failed: {msg}"),
            Self::AddressGenerationFailed(msg) => {
                write!(f, "ldk address generation failed: {msg}")
            }
            Self::BalanceOverflow { sats } => {
                write!(f, "balance conversion overflow for sats={sats}")
            }
            Self::InvalidInvoice(msg) => write!(f, "invalid invoice: {msg}"),
            Self::InvalidInvoiceRequest(msg) => write!(f, "invalid invoice request: {msg}"),
            Self::InvalidPubkey(msg) => write!(f, "invalid pubkey: {msg}"),
            Self::InvalidAmount(amount) => write!(f, "invalid amount: {amount}"),
            Self::ChannelFailed(msg) => write!(f, "channel operation failed: {msg}"),
            Self::PeerFailed(msg) => write!(f, "peer operation failed: {msg}"),
            Self::PaymentFailed(msg) => write!(f, "payment failed: {msg}"),
            Self::PaymentNotFound(msg) => write!(f, "payment not found: {msg}"),
            Self::InvalidAddress(msg) => write!(f, "invalid address: {msg}"),
            Self::OfferFailed(msg) => write!(f, "offer operation failed: {msg}"),
            Self::StopFailed(msg) => write!(f, "ldk node stop failed: {msg}"),
        }
    }
}

impl std::error::Error for LdkServiceError {}

#[derive(Debug, Clone)]
pub struct LdkBalance {
    pub total_msat: u64,
    pub lightning_msat: u64,
    pub onchain_msat: u64,
}

pub struct LdkNodeStatus {
    pub latest_best_block_height: u32,
}

pub struct LdkService {
    node: Arc<Node>,
    network: Network,
    /// VLS keys manager, if using VLS signer.
    keys_manager: Option<Arc<ldk_vls2_client::KeysManagerClient>>,
}

pub struct LdkPaymentResult {
    pub preimage: String,
    pub fees_paid_msat: Option<u64>,
}

pub struct LdkInvoiceResult {
    pub invoice: String,
    pub payment_hash: Option<String>,
    pub amount_msat: Option<u64>,
    pub expires_at: Option<u64>,
}

#[derive(Debug, Clone, Serialize)]
pub struct LdkChannelInfo {
    pub id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub short_channel_id: Option<String>,
    pub peer_pubkey: String,
    pub state: String,
    pub is_private: bool,
    pub local_balance: u64,
    pub remote_balance: u64,
    pub capacity: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub funding_txid: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub funding_output_index: Option<u32>,
}

#[derive(Debug, Clone, Serialize)]
pub struct LdkPeerInfo {
    pub pubkey: String,
    pub address: String,
    pub connected: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub alias: Option<String>,
    pub num_channels: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct LdkChannelFees {
    pub id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub short_channel_id: Option<String>,
    pub peer_pubkey: String,
    pub base_fee: u32,
    pub fee_rate: u32,
    pub min_htlc: u64,
    pub max_htlc: Option<u64>,
}

#[derive(Debug, Clone, Serialize)]
pub struct NetworkNodeInfo {
    pub pubkey: String,
    pub alias: String,
    pub color: String,
    pub num_channels: usize,
    pub total_capacity: u64,
    pub addresses: Vec<String>,
    pub last_update: u32,
}

#[derive(Debug, Clone, Serialize)]
pub struct NetworkChannelPolicy {
    pub base_fee: u32,
    pub fee_rate: u32,
    pub min_htlc: u64,
    pub max_htlc: u64,
    pub time_lock_delta: u16,
    pub disabled: bool,
    pub last_update: u32,
}

#[derive(Debug, Clone, Serialize)]
pub struct NetworkChannelInfo {
    pub short_channel_id: String,
    pub capacity: Option<u64>,
    pub node1_pubkey: String,
    pub node2_pubkey: String,
    pub node1_policy: Option<NetworkChannelPolicy>,
    pub node2_policy: Option<NetworkChannelPolicy>,
}

#[derive(Debug, Clone, Serialize)]
pub struct NetworkStats {
    pub num_nodes: usize,
    pub num_channels: usize,
    pub total_capacity: u64,
    pub avg_channel_size: u64,
    pub max_channel_size: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct RouteHop {
    pub pubkey: String,
    pub short_channel_id: String,
    pub fee: u64,
    pub expiry: u16,
}

#[derive(Debug, Clone, Serialize)]
pub struct FoundRoute {
    pub total_fee: u64,
    pub total_time_lock: u32,
    pub hops: Vec<RouteHop>,
}

impl LdkService {
    /// Create an LdkService from an already-started Node.
    pub fn from_node(node: Node, network: Network) -> Arc<Self> {
        Arc::new(Self {
            node: Arc::new(node),
            network,
            keys_manager: None,
        })
    }

    pub fn start_from_config(cfg: &LdkServiceConfig) -> Result<Arc<Self>, LdkServiceInitError> {
        cfg.validate()?;
        let network = cfg.parse_network()?;

        // Convert ldk-node Network to VLS Network for the embedded signer
        let vls_network = match network {
            Network::Regtest => lightning_signer::bitcoin::Network::Regtest,
            Network::Testnet => lightning_signer::bitcoin::Network::Testnet,
            Network::Bitcoin => lightning_signer::bitcoin::Network::Bitcoin,
            Network::Signet => lightning_signer::bitcoin::Network::Signet,
            _ => return Err(LdkServiceInitError::InvalidNetwork { network: cfg.network.clone() }),
        };

        // "none" bypasses VLS entirely and lets ldk-node use its own
        // KeysManager and BDK wallet, seeded from its storage dir. Intended for
        // tests and bring-up: there is no policy validation on this path.
        let use_vls = cfg.signer_transport.as_str() != "none";

        // Create transport: either Nostr (remote signer) or embedded (in-process)
        let transport: Arc<dyn ldk_vls2_client::Transport> = match cfg.signer_transport.as_str() {
            "nostr" => {
                let relay = cfg.signer_relay.as_deref().ok_or_else(|| {
                    LdkServiceInitError::InvalidConfig("signer.relay required for nostr transport".into())
                })?;
                let nsec = cfg.signer_nsec.as_deref().ok_or_else(|| {
                    LdkServiceInitError::InvalidConfig("signer.nsec required for nostr transport".into())
                })?;
                let signer_pubkey = cfg.signer_pubkey.as_deref().ok_or_else(|| {
                    LdkServiceInitError::InvalidConfig("signer.signer_pubkey required for nostr transport".into())
                })?;
                eprintln!("Using Nostr signer transport: relay={}", relay);
                Arc::new(
                    ldk_vls2_client::NostrTransport::new(relay, nsec, signer_pubkey)
                        .map_err(|e| LdkServiceInitError::BuildFailed(format!("NostrTransport init: {}", e)))?,
                )
            }
            _ => {
                // Default: embedded signer with NullTransport
                let policy_filters = vec![
                    "policy-commitment-htlc-routing-balance:warn".to_string(),
                    "policy-routing-balanced:warn".to_string(),
                ];
                let signer = ldk_vls2_signer::EmbeddedSigner::new_with_protocol_version(
                    vls_network,
                    None,  // DummyPersister for now
                    false,
                    &policy_filters,
                    6,  // PROTOCOL_VERSION_NO_SECRET: enables explicit RevokeCommitmentTx
                ).map_err(|e| LdkServiceInitError::BuildFailed(format!("VLS signer init: {}", e)))?;
                eprintln!("Using embedded signer transport");
                Arc::new(ldk_vls2_client::NullTransport::new(signer))
            }
        };

        let network_name = cfg.network.to_lowercase();
        let keys_manager = use_vls.then(|| {
            Arc::new(ldk_vls2_client::KeysManagerClient::new(
                transport,
                &network_name,
            ))
        });

        let mut builder = Builder::new();
        builder.set_network(network);
        builder.set_chain_source_bitcoind_rpc(
            cfg.bitcoind_rpc_host.clone(),
            cfg.bitcoind_rpc_port,
            cfg.bitcoind_rpc_user.clone(),
            cfg.bitcoind_rpc_password.clone(),
        );
        builder.set_storage_dir_path(cfg.ldk_storage_dir.clone());

        if let Some(km) = &keys_manager {
            // Inject VLS signer via bridge wrapper
            let bridge = Arc::new(vls_keys_bridge::VlsKeysInterface {
                inner: km.clone(),
            });
            builder.set_custom_keys_interface(bridge);

            // Configure watch-only BDK wallet with VLS signing
            let xpub = km.xpub();
            let bdk_signer = Arc::new(km.bdk_signer());
            builder.set_custom_wallet(xpub, bdk_signer);
        } else {
            eprintln!("Using plain LDK keys (no VLS signer)");
        }

        if let Some(listen_addr) = &cfg.ldk_listen_addr {
            let socket = SocketAddress::from_str(listen_addr).map_err(|_| {
                LdkServiceInitError::InvalidListeningAddress {
                    address: listen_addr.clone(),
                }
            })?;
            builder
                .set_listening_addresses(vec![socket])
                .map_err(|e| LdkServiceInitError::BuildFailed(e.to_string()))?;
        }

        if let Some(alias) = &cfg.node_alias {
            builder
                .set_node_alias(alias.clone())
                .map_err(|e| LdkServiceInitError::BuildFailed(format!("invalid node alias: {e}")))?;
        }

        let node = builder
            .build()
            .map_err(|e| LdkServiceInitError::BuildFailed(e.to_string()))?;
        node.start()
            .map_err(|e| LdkServiceInitError::StartFailed(e.to_string()))?;

        Ok(Arc::new(Self {
            node: Arc::new(node),
            network,
            keys_manager,
        }))
    }

    /// Notify the VLS signer that a channel is ready (CheckOutpoint + LockOutpoint).
    ///
    /// Call this from the Event::ChannelReady handler.
    pub fn notify_channel_ready(&self, user_channel_id: u128) {
        if let Some(km) = &self.keys_manager {
            if let Err(e) = km.channel_ready_by_user_channel_id(user_channel_id) {
                eprintln!("WARN: VLS channel_ready failed: {}", e);
            }
        }
    }

    pub fn node(&self) -> &Arc<Node> {
        &self.node
    }

    pub fn node_id(&self) -> String {
        self.node.node_id().to_string()
    }

    pub fn status(&self) -> LdkNodeStatus {
        let status = self.node.status();
        LdkNodeStatus {
            latest_best_block_height: status.current_best_block.height,
        }
    }

    pub fn network(&self) -> &'static str {
        match self.network {
            Network::Regtest => "regtest",
            Network::Testnet => "testnet",
            Network::Bitcoin => "bitcoin",
            Network::Signet => "signet",
            _ => "unknown",
        }
    }

    pub fn sync_wallets(&self) -> Result<(), LdkServiceError> {
        self.node
            .sync_wallets()
            .map_err(|e| LdkServiceError::SyncFailed(e.to_string()))
    }

    pub fn get_balance(&self) -> Result<LdkBalance, LdkServiceError> {
        let balances = self.node.list_balances();
        let onchain_msat = balances
            .spendable_onchain_balance_sats
            .checked_mul(1000)
            .ok_or(LdkServiceError::BalanceOverflow {
                sats: balances.spendable_onchain_balance_sats,
            })?;
        let lightning_msat = balances
            .total_lightning_balance_sats
            .checked_mul(1000)
            .ok_or(LdkServiceError::BalanceOverflow {
                sats: balances.total_lightning_balance_sats,
            })?;
        let total_msat = onchain_msat
            .checked_add(lightning_msat)
            .ok_or(LdkServiceError::BalanceOverflow {
                sats: balances.spendable_onchain_balance_sats + balances.total_lightning_balance_sats,
            })?;
        Ok(LdkBalance { total_msat, lightning_msat, onchain_msat })
    }

    pub fn get_balance_msat(&self) -> Result<u64, LdkServiceError> {
        Ok(self.get_balance()?.total_msat)
    }

    pub fn new_onchain_address(&self) -> Result<String, LdkServiceError> {
        self.node
            .onchain_payment()
            .new_address()
            .map(|a| a.to_string())
            .map_err(|e| LdkServiceError::AddressGenerationFailed(e.to_string()))
    }

    pub fn make_invoice(
        &self,
        amount_msat: u64,
        description: Option<&str>,
        description_hash: Option<&str>,
        expiry_secs: Option<u64>,
    ) -> Result<LdkInvoiceResult, LdkServiceError> {
        if amount_msat == 0 {
            return Err(LdkServiceError::InvalidAmount(amount_msat));
        }
        if description_hash.is_some() {
            return Err(LdkServiceError::InvalidInvoiceRequest(
                "description_hash is not supported yet".to_string(),
            ));
        }

        let description_value = description.unwrap_or("nwc invoice").to_string();
        let desc = Description::new(description_value)
            .map_err(|e| LdkServiceError::InvalidInvoiceRequest(e.to_string()))?;
        let invoice_desc = Bolt11InvoiceDescription::Direct(desc);
        let expiry_u32 = expiry_secs
            .map(u32::try_from)
            .transpose()
            .map_err(|_| {
                LdkServiceError::InvalidInvoiceRequest("expiry exceeds u32::MAX".to_string())
            })?
            .unwrap_or(3600);

        let invoice = self
            .node
            .bolt11_payment()
            .receive(amount_msat, &invoice_desc, expiry_u32)
            .map_err(|e| LdkServiceError::InvalidInvoiceRequest(e.to_string()))?;

        let payment_hash = Some(invoice.payment_hash().to_string());
        let expires_at = invoice.expires_at().map(|ts| ts.as_secs());

        Ok(LdkInvoiceResult {
            invoice: invoice.to_string(),
            payment_hash,
            amount_msat: invoice.amount_milli_satoshis(),
            expires_at,
        })
    }

    pub fn pay_invoice(
        &self,
        invoice_str: &str,
        amount_msat: Option<u64>,
    ) -> Result<LdkPaymentResult, LdkServiceError> {
        let invoice = Bolt11Invoice::from_str(invoice_str)
            .map_err(|e| LdkServiceError::InvalidInvoice(e.to_string()))?;
        let payment_id = if let Some(amount) = amount_msat {
            self.node
                .bolt11_payment()
                .send_using_amount(&invoice, amount, None)
                .map_err(|e| LdkServiceError::PaymentFailed(e.to_string()))?
        } else {
            self.node
                .bolt11_payment()
                .send(&invoice, None)
                .map_err(|e| LdkServiceError::PaymentFailed(e.to_string()))?
        };

        self.wait_for_outbound_payment(payment_id)
    }

    pub fn pay_keysend(
        &self,
        dest_pubkey: &str,
        amount_msat: u64,
    ) -> Result<LdkPaymentResult, LdkServiceError> {
        if amount_msat == 0 {
            return Err(LdkServiceError::InvalidAmount(amount_msat));
        }
        let node_id = PublicKey::from_str(dest_pubkey)
            .map_err(|e| LdkServiceError::InvalidPubkey(e.to_string()))?;
        let payment_id = self
            .node
            .spontaneous_payment()
            .send(amount_msat, node_id, None)
            .map_err(|e| LdkServiceError::PaymentFailed(e.to_string()))?;

        self.wait_for_outbound_payment(payment_id)
    }

    pub fn open_channel(
        &self,
        counterparty_pubkey: &str,
        counterparty_addr: &str,
        channel_amount_sats: u64,
        push_to_counterparty_msat: Option<u64>,
    ) -> Result<(), LdkServiceError> {
        let node_id = PublicKey::from_str(counterparty_pubkey)
            .map_err(|e| LdkServiceError::InvalidPubkey(e.to_string()))?;
        let addr = SocketAddress::from_str(counterparty_addr)
            .map_err(|e| LdkServiceError::ChannelFailed(e.to_string()))?;
        self.node
            .open_channel(
                node_id,
                addr,
                channel_amount_sats,
                push_to_counterparty_msat,
                None,
            )
            .map_err(|e| LdkServiceError::ChannelFailed(e.to_string()))?;
        Ok(())
    }

    pub fn connect_peer(
        &self,
        counterparty_pubkey: &str,
        counterparty_addr: &str,
    ) -> Result<(), LdkServiceError> {
        let node_id = PublicKey::from_str(counterparty_pubkey)
            .map_err(|e| LdkServiceError::InvalidPubkey(e.to_string()))?;
        let addr = SocketAddress::from_str(counterparty_addr)
            .map_err(|e| LdkServiceError::PeerFailed(e.to_string()))?;
        self.node
            .connect(node_id, addr, true)
            .map_err(|e| LdkServiceError::PeerFailed(e.to_string()))?;
        Ok(())
    }

    pub fn disconnect_peer(&self, counterparty_pubkey: &str) -> Result<(), LdkServiceError> {
        let node_id = PublicKey::from_str(counterparty_pubkey)
            .map_err(|e| LdkServiceError::InvalidPubkey(e.to_string()))?;
        self.node
            .disconnect(node_id)
            .map_err(|e| LdkServiceError::PeerFailed(e.to_string()))?;
        Ok(())
    }

    pub fn stop(&self) -> Result<(), LdkServiceError> {
        self.node
            .stop()
            .map_err(|e| LdkServiceError::StopFailed(e.to_string()))
    }

    pub fn has_ready_channel_with(&self, counterparty_pubkey: &str) -> bool {
        let Ok(counterparty) = PublicKey::from_str(counterparty_pubkey) else {
            return false;
        };
        self.node
            .list_channels()
            .iter()
            .any(|c| c.counterparty_node_id == counterparty && c.is_channel_ready)
    }

    pub fn has_channel_with(&self, counterparty_pubkey: &str) -> bool {
        let Ok(counterparty) = PublicKey::from_str(counterparty_pubkey) else {
            return false;
        };
        self.node
            .list_channels()
            .iter()
            .any(|c| c.counterparty_node_id == counterparty)
    }

    pub fn list_channels(&self) -> Vec<LdkChannelInfo> {
        self.node
            .list_channels()
            .iter()
            .map(|channel| {
                let state = if channel.is_channel_ready && channel.is_usable {
                    "active"
                } else if channel.is_channel_ready {
                    "inactive"
                } else {
                    "pending_open"
                };

                let short_channel_id = channel.short_channel_id.map(format_scid);

                let (funding_txid, funding_output_index) = channel
                    .funding_txo
                    .map(|txo| (Some(txo.txid.to_string()), Some(txo.vout)))
                    .unwrap_or((None, None));

                LdkChannelInfo {
                    id: channel.channel_id.to_string(),
                    short_channel_id,
                    peer_pubkey: channel.counterparty_node_id.to_string(),
                    state: state.to_string(),
                    is_private: !channel.is_announced,
                    local_balance: channel.outbound_capacity_msat,
                    remote_balance: channel.inbound_capacity_msat,
                    capacity: channel.channel_value_sats * 1000,
                    funding_txid,
                    funding_output_index,
                }
            })
            .collect()
    }

    pub fn close_channel(&self, channel_id: &str, force: bool) -> Result<(), LdkServiceError> {
        let details = self
            .node
            .list_channels()
            .into_iter()
            .find(|channel| channel.channel_id.to_string() == channel_id)
            .ok_or_else(|| {
                LdkServiceError::ChannelFailed(format!("channel not found: {channel_id}"))
            })?;

        if force {
            self.node
                .force_close_channel(
                    &details.user_channel_id,
                    details.counterparty_node_id,
                    Some("closed via control API".to_string()),
                )
                .map_err(|e| LdkServiceError::ChannelFailed(e.to_string()))?;
        } else {
            self.node
                .close_channel(&details.user_channel_id, details.counterparty_node_id)
                .map_err(|e| LdkServiceError::ChannelFailed(e.to_string()))?;
        }
        Ok(())
    }

    pub fn list_peers(&self) -> Vec<LdkPeerInfo> {
        let channels = self.node.list_channels();
        self.node
            .list_peers()
            .iter()
            .map(|peer| {
                let num_channels = channels
                    .iter()
                    .filter(|c| c.counterparty_node_id == peer.node_id)
                    .count();
                LdkPeerInfo {
                    pubkey: peer.node_id.to_string(),
                    address: peer.address.to_string(),
                    connected: peer.is_connected,
                    alias: None,
                    num_channels,
                }
            })
            .collect()
    }

    pub fn get_channel_fees(&self, channel_id: Option<&str>) -> Vec<LdkChannelFees> {
        self.node
            .list_channels()
            .iter()
            .filter(|c| match channel_id {
                Some(id) => c.channel_id.to_string() == id,
                None => true,
            })
            .map(|channel| {
                let short_channel_id = channel.short_channel_id.map(|scid| format_scid(scid));
                LdkChannelFees {
                    id: channel.channel_id.to_string(),
                    short_channel_id,
                    peer_pubkey: channel.counterparty_node_id.to_string(),
                    base_fee: channel.config.forwarding_fee_base_msat,
                    fee_rate: channel.config.forwarding_fee_proportional_millionths,
                    min_htlc: channel.inbound_htlc_minimum_msat,
                    max_htlc: channel.inbound_htlc_maximum_msat,
                }
            })
            .collect()
    }

    pub fn set_channel_fees(
        &self,
        channel_id: &str,
        base_fee_msat: Option<u32>,
        fee_rate: Option<u32>,
    ) -> Result<(), LdkServiceError> {
        let details = self
            .node
            .list_channels()
            .into_iter()
            .find(|c| c.channel_id.to_string() == channel_id)
            .ok_or_else(|| {
                LdkServiceError::ChannelFailed(format!("channel not found: {channel_id}"))
            })?;

        let mut config = details.config;
        if let Some(base) = base_fee_msat {
            config.forwarding_fee_base_msat = base;
        }
        if let Some(rate) = fee_rate {
            config.forwarding_fee_proportional_millionths = rate;
        }

        self.node
            .update_channel_config(
                &details.user_channel_id,
                details.counterparty_node_id,
                config,
            )
            .map_err(|e| LdkServiceError::ChannelFailed(e.to_string()))
    }

    pub fn list_network_nodes(&self, limit: usize, offset: usize) -> Vec<NetworkNodeInfo> {
        let graph = self.node.network_graph();
        let node_ids = graph.list_nodes();
        node_ids
            .iter()
            .skip(offset)
            .take(limit)
            .filter_map(|node_id| {
                let info = graph.node(node_id)?;
                Some(map_node_info(node_id, &info, &graph))
            })
            .collect()
    }

    pub fn get_network_node(&self, pubkey: &str) -> Result<Option<NetworkNodeInfo>, LdkServiceError> {
        let pk = PublicKey::from_str(pubkey)
            .map_err(|e| LdkServiceError::InvalidPubkey(e.to_string()))?;
        let node_id = NodeId::from_pubkey(&pk);
        let graph = self.node.network_graph();
        let info = graph.node(&node_id);
        Ok(info.map(|i| map_node_info(&node_id, &i, &graph)))
    }

    pub fn get_network_stats(&self) -> NetworkStats {
        let graph = self.node.network_graph();
        let num_nodes = graph.list_nodes().len();
        let channel_ids = graph.list_channels();
        let num_channels = channel_ids.len();
        let mut total_capacity: u64 = 0;
        let mut max_channel_size: u64 = 0;
        for scid in &channel_ids {
            if let Some(ch) = graph.channel(*scid) {
                let cap = ch.capacity_sats.unwrap_or(0);
                total_capacity = total_capacity.saturating_add(cap);
                if cap > max_channel_size {
                    max_channel_size = cap;
                }
            }
        }
        let avg_channel_size = if num_channels > 0 {
            total_capacity / num_channels as u64
        } else {
            0
        };
        NetworkStats {
            num_nodes,
            num_channels,
            total_capacity,
            avg_channel_size,
            max_channel_size,
        }
    }

    pub fn get_network_channel(&self, scid_str: &str) -> Result<Option<NetworkChannelInfo>, LdkServiceError> {
        let scid = parse_scid(scid_str)
            .ok_or_else(|| LdkServiceError::ChannelFailed(format!("invalid scid format: {scid_str}")))?;
        let graph = self.node.network_graph();
        Ok(graph.channel(scid).map(|ch| {
            let node1_policy = ch.one_to_two.as_ref().map(|p| NetworkChannelPolicy {
                base_fee: p.fees.base_msat,
                fee_rate: p.fees.proportional_millionths,
                min_htlc: p.htlc_minimum_msat,
                max_htlc: p.htlc_maximum_msat,
                time_lock_delta: p.cltv_expiry_delta,
                disabled: !p.enabled,
                last_update: p.last_update,
            });
            let node2_policy = ch.two_to_one.as_ref().map(|p| NetworkChannelPolicy {
                base_fee: p.fees.base_msat,
                fee_rate: p.fees.proportional_millionths,
                min_htlc: p.htlc_minimum_msat,
                max_htlc: p.htlc_maximum_msat,
                time_lock_delta: p.cltv_expiry_delta,
                disabled: !p.enabled,
                last_update: p.last_update,
            });
            NetworkChannelInfo {
                short_channel_id: format_scid(scid),
                capacity: ch.capacity_sats,
                node1_pubkey: ch.node_one.to_string(),
                node2_pubkey: ch.node_two.to_string(),
                node1_policy,
                node2_policy,
            }
        }))
    }

    pub fn lookup_payment_by_hash(&self, payment_hash: &str) -> Result<PaymentDetails, LdkServiceError> {
        let hash_bytes = decode_hex(payment_hash)
            .map_err(|e| LdkServiceError::PaymentNotFound(format!("invalid payment hash hex: {e}")))?;
        let arr: [u8; 32] = hash_bytes
            .try_into()
            .map_err(|_| LdkServiceError::PaymentNotFound("payment hash must be 32 bytes".to_string()))?;
        let payment_id = PaymentId(arr);
        self.node
            .payment(&payment_id)
            .ok_or_else(|| LdkServiceError::PaymentNotFound(payment_hash.to_string()))
    }

    pub fn lookup_payment_by_bolt11(&self, invoice_str: &str) -> Result<PaymentDetails, LdkServiceError> {
        let invoice = Bolt11Invoice::from_str(invoice_str)
            .map_err(|e| LdkServiceError::InvalidInvoice(e.to_string()))?;
        let hash = invoice.payment_hash().to_string();
        self.lookup_payment_by_hash(&hash)
    }

    pub fn list_payments_filtered(
        &self,
        from: Option<u64>,
        until: Option<u64>,
        limit: Option<u64>,
        offset: Option<u64>,
        unpaid: Option<bool>,
        direction: Option<PaymentDirection>,
        payment_method: Option<PaymentMethod>,
    ) -> Vec<PaymentDetails> {
        let mut payments: Vec<PaymentDetails> = self
            .node
            .list_payments()
            .into_iter()
            .filter(|p| {
                if let Some(from_ts) = from {
                    if p.latest_update_timestamp < from_ts {
                        return false;
                    }
                }
                if let Some(until_ts) = until {
                    if p.latest_update_timestamp > until_ts {
                        return false;
                    }
                }
                if let Some(dir) = &direction {
                    if p.direction != *dir {
                        return false;
                    }
                }
                if let Some(pm) = &payment_method {
                    let matches = match (pm, &p.kind) {
                        (PaymentMethod::Bolt11, PaymentKind::Bolt11 { .. })
                        | (PaymentMethod::Bolt11, PaymentKind::Bolt11Jit { .. }) => true,
                        (PaymentMethod::Bolt12, PaymentKind::Bolt12Offer { .. })
                        | (PaymentMethod::Bolt12, PaymentKind::Bolt12Refund { .. }) => true,
                        (PaymentMethod::Keysend, PaymentKind::Spontaneous { .. }) => true,
                        _ => false,
                    };
                    if !matches {
                        return false;
                    }
                }
                let include_unpaid = unpaid.unwrap_or(false);
                if !include_unpaid && p.status == PaymentStatus::Pending {
                    return false;
                }
                true
            })
            .collect();

        payments.sort_by(|a, b| b.latest_update_timestamp.cmp(&a.latest_update_timestamp));

        let offset_val = offset.unwrap_or(0) as usize;
        let payments = if offset_val > 0 {
            payments.into_iter().skip(offset_val).collect()
        } else {
            payments
        };

        if let Some(lim) = limit {
            payments.into_iter().take(lim as usize).collect()
        } else {
            payments
        }
    }

    pub fn pay_onchain(
        &self,
        address: &str,
        amount_sats: u64,
        _feerate: Option<u64>,
    ) -> Result<String, LdkServiceError> {
        let addr = Address::from_str(address)
            .map_err(|e| LdkServiceError::InvalidAddress(e.to_string()))?
            .require_network(self.network)
            .map_err(|e| LdkServiceError::InvalidAddress(e.to_string()))?;

        let txid = self
            .node
            .onchain_payment()
            .send_to_address(&addr, amount_sats, None)
            .map_err(|e| LdkServiceError::PaymentFailed(e.to_string()))?;

        Ok(txid.to_string())
    }

    pub fn make_offer(
        &self,
        amount_msat: u64,
        description: &str,
        expiry_secs: Option<u32>,
    ) -> Result<String, LdkServiceError> {
        let offer = self
            .node
            .bolt12_payment()
            .receive(amount_msat, description, expiry_secs, None)
            .map_err(|e| LdkServiceError::OfferFailed(e.to_string()))?;

        Ok(offer.to_string())
    }

    pub fn pay_offer(
        &self,
        offer_str: &str,
        _amount: Option<u64>,
        payer_note: Option<String>,
    ) -> Result<LdkPaymentResult, LdkServiceError> {
        let offer = Offer::from_str(offer_str)
            .map_err(|e| LdkServiceError::OfferFailed(format!("invalid offer: {e:?}")))?;

        let payment_id = self
            .node
            .bolt12_payment()
            .send(&offer, None, payer_note, None)
            .map_err(|e| LdkServiceError::PaymentFailed(e.to_string()))?;

        self.wait_for_outbound_payment(payment_id)
    }

    fn wait_for_outbound_payment(
        &self,
        payment_id: PaymentId,
    ) -> Result<LdkPaymentResult, LdkServiceError> {
        let timeout = Duration::from_secs(60);
        let start = std::time::Instant::now();
        loop {
            if let Some(payment) = self
                .node
                .list_payments()
                .into_iter()
                .find(|p| p.id == payment_id && p.direction == PaymentDirection::Outbound)
            {
                match payment.status {
                    PaymentStatus::Succeeded => {
                        let preimage = match payment.kind {
                            PaymentKind::Bolt11 { preimage, .. } => preimage,
                            PaymentKind::Bolt11Jit { preimage, .. } => preimage,
                            PaymentKind::Spontaneous { preimage, .. } => preimage,
                            PaymentKind::Bolt12Offer { preimage, .. } => preimage,
                            PaymentKind::Bolt12Refund { preimage, .. } => preimage,
                            _ => None,
                        }
                        .ok_or_else(|| {
                            LdkServiceError::PaymentFailed(
                                "payment succeeded but preimage missing".to_string(),
                            )
                        })?;

                        return Ok(LdkPaymentResult {
                            preimage: hex_string(&preimage.0),
                            fees_paid_msat: payment.fee_paid_msat,
                        });
                    }
                    PaymentStatus::Failed => {
                        return Err(LdkServiceError::PaymentFailed(
                            "payment marked failed".to_string(),
                        ));
                    }
                    PaymentStatus::Pending => {}
                }
            }

            if start.elapsed() > timeout {
                return Err(LdkServiceError::PaymentFailed(
                    "timeout waiting for payment outcome".to_string(),
                ));
            }
            std::thread::sleep(Duration::from_millis(100));
        }
    }

    pub fn find_routes(
        &self,
        dest_pubkey: &str,
        amount_msat: u64,
        max_routes: usize,
    ) -> Vec<FoundRoute> {
        use std::cmp::Reverse;
        use std::collections::{BinaryHeap, HashMap, HashSet};

        let Ok(dest_pk) = PublicKey::from_str(dest_pubkey) else {
            return Vec::new();
        };
        let dest_node = NodeId::from_pubkey(&dest_pk);
        let our_pk = self.node.node_id();
        let our_node = NodeId::from_pubkey(&our_pk);

        if our_node == dest_node {
            return Vec::new();
        }

        let graph = self.node.network_graph();
        let all_channels = graph.list_channels();

        // Build adjacency: node_id → [(neighbor, scid, base_fee, fee_rate_ppm, cltv)]
        type Edge = (NodeId, u64, u32, u32, u16);
        let mut adj: HashMap<NodeId, Vec<Edge>> = HashMap::new();

        for scid in &all_channels {
            let Some(ch) = graph.channel(*scid) else { continue };
            if let Some(upd) = &ch.one_to_two {
                if amount_msat >= upd.htlc_minimum_msat
                    && amount_msat <= upd.htlc_maximum_msat
                {
                    adj.entry(ch.node_one).or_default().push((
                        ch.node_two,
                        *scid,
                        upd.fees.base_msat,
                        upd.fees.proportional_millionths,
                        upd.cltv_expiry_delta,
                    ));
                }
            }
            if let Some(upd) = &ch.two_to_one {
                if amount_msat >= upd.htlc_minimum_msat
                    && amount_msat <= upd.htlc_maximum_msat
                {
                    adj.entry(ch.node_two).or_default().push((
                        ch.node_one,
                        *scid,
                        upd.fees.base_msat,
                        upd.fees.proportional_millionths,
                        upd.cltv_expiry_delta,
                    ));
                }
            }
        }

        // Dijkstra: find lowest-fee paths
        // State: (cost_msat, node, path as [(node, scid, hop_fee, cltv)])
        let mut heap: BinaryHeap<Reverse<(u64, NodeId, Vec<(NodeId, u64, u64, u16)>)>> =
            BinaryHeap::new();
        let mut visited: HashSet<NodeId> = HashSet::new();
        let mut found: Vec<FoundRoute> = Vec::new();

        heap.push(Reverse((0, our_node, Vec::new())));

        while let Some(Reverse((cost, node, path))) = heap.pop() {
            if found.len() >= max_routes {
                break;
            }
            if path.len() > 6 {
                continue;
            }
            if node == dest_node {
                let hops: Vec<RouteHop> = path
                    .iter()
                    .map(|(n, scid, fee, cltv)| RouteHop {
                        pubkey: hex_string(n.as_slice()),
                        short_channel_id: format_scid(*scid),
                        fee: *fee,
                        expiry: *cltv,
                    })
                    .collect();
                let total_time_lock: u32 = hops.iter().map(|h| h.expiry as u32).sum();
                found.push(FoundRoute {
                    total_fee: cost,
                    total_time_lock,
                    hops,
                });
                continue;
            }
            if !visited.insert(node) {
                continue;
            }
            if let Some(edges) = adj.get(&node) {
                for (next, scid, base, prop, cltv) in edges {
                    if visited.contains(next) {
                        continue;
                    }
                    let hop_fee =
                        *base as u64 + (amount_msat * *prop as u64) / 1_000_000;
                    let mut new_path = path.clone();
                    new_path.push((*next, *scid, hop_fee, *cltv));
                    heap.push(Reverse((cost + hop_fee, *next, new_path)));
                }
            }
        }

        found
    }
}

fn format_scid(scid: u64) -> String {
    let block = scid >> 40;
    let tx = (scid >> 16) & 0xFFFFFF;
    let vout = scid & 0xFFFF;
    format!("{block}x{tx}x{vout}")
}

fn parse_scid(s: &str) -> Option<u64> {
    let parts: Vec<&str> = s.split('x').collect();
    if parts.len() != 3 {
        return None;
    }
    let block: u64 = parts[0].parse().ok()?;
    let tx: u64 = parts[1].parse().ok()?;
    let vout: u64 = parts[2].parse().ok()?;
    Some((block << 40) | (tx << 16) | vout)
}

fn map_node_info(
    node_id: &NodeId,
    info: &ldk_node::lightning::routing::gossip::NodeInfo,
    graph: &ldk_node::graph::NetworkGraph,
) -> NetworkNodeInfo {
    let (alias, color, addresses, last_update) = match &info.announcement_info {
        Some(ann) => {
            let alias = ann.alias().to_string();
            let rgb = ann.rgb();
            let color = format!("#{:02x}{:02x}{:02x}", rgb[0], rgb[1], rgb[2]);
            let addresses: Vec<String> = ann.addresses().iter().map(|a| a.to_string()).collect();
            let last_update = ann.last_update();
            (alias, color, addresses, last_update)
        }
        None => (String::new(), "#000000".to_string(), Vec::new(), 0),
    };

    let total_capacity: u64 = info
        .channels
        .iter()
        .filter_map(|scid| graph.channel(*scid))
        .map(|ch| ch.capacity_sats.unwrap_or(0))
        .sum();

    NetworkNodeInfo {
        pubkey: node_id.to_string(),
        alias,
        color,
        num_channels: info.channels.len(),
        total_capacity,
        addresses,
        last_update,
    }
}

fn hex_string(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        use std::fmt::Write as _;
        let _ = write!(&mut out, "{:02x}", b);
    }
    out
}

fn decode_hex(s: &str) -> Result<Vec<u8>, String> {
    if s.len() % 2 != 0 {
        return Err("odd-length hex string".to_string());
    }
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).map_err(|e| e.to_string()))
        .collect()
}

/// Wrapper around KeysManagerClient that implements ldk_node::KeysInterface.
///
/// This is needed because ldk_vls2_client defines its own KeysInterface mirror
/// to avoid a circular dependency on ldk-node, but ldk-node's Builder requires
/// the ldk_node::KeysInterface trait specifically.
mod vls_keys_bridge {
    use std::future::Future;
    use std::pin::Pin;
    use std::sync::Arc;

    use ldk_node::bitcoin::secp256k1::ecdsa::{RecoverableSignature, Signature};
    use ldk_node::bitcoin::secp256k1::ecdh::SharedSecret;
    use ldk_node::bitcoin::secp256k1::{All, PublicKey, Scalar, Secp256k1};
    use ldk_node::bitcoin::{ScriptBuf, Transaction, TxOut};
    use ldk_node::lightning::ln::inbound_payment::ExpandedKey;
    use ldk_node::lightning::ln::msgs::UnsignedGossipMessage;
    use ldk_node::lightning::ln::script::ShutdownScript;
    use ldk_node::lightning::sign::{
        ChangeDestinationSource, EntropySource, NodeSigner, OutputSpender,
        PeerStorageKey, ReceiveAuthKey, Recipient, SignerProvider, SpendableOutputDescriptor,
    };
    use ldk_node::lightning::util::dyn_signer::DynSigner;
    use ldk_node::lightning_invoice::RawBolt11Invoice;

    use ldk_vls2_client::KeysManagerClient;

    /// Newtype that bridges ldk_vls2_client::KeysInterface → ldk_node::KeysInterface.
    pub struct VlsKeysInterface {
        pub inner: Arc<KeysManagerClient>,
    }

    impl EntropySource for VlsKeysInterface {
        fn get_secure_random_bytes(&self) -> [u8; 32] {
            self.inner.get_secure_random_bytes()
        }
    }

    impl NodeSigner for VlsKeysInterface {
        fn get_expanded_key(&self) -> ExpandedKey {
            self.inner.get_expanded_key()
        }

        fn get_peer_storage_key(&self) -> PeerStorageKey {
            self.inner.get_peer_storage_key()
        }

        fn get_receive_auth_key(&self) -> ReceiveAuthKey {
            self.inner.get_receive_auth_key()
        }

        fn get_node_id(&self, recipient: Recipient) -> Result<PublicKey, ()> {
            self.inner.get_node_id(recipient)
        }

        fn ecdh(
            &self,
            recipient: Recipient,
            other_key: &PublicKey,
            tweak: Option<&Scalar>,
        ) -> Result<SharedSecret, ()> {
            self.inner.ecdh(recipient, other_key, tweak)
        }

        fn sign_invoice(
            &self,
            invoice: &RawBolt11Invoice,
            recipient: Recipient,
        ) -> Result<RecoverableSignature, ()> {
            self.inner.sign_invoice(invoice, recipient)
        }

        fn sign_bolt12_invoice(
            &self,
            invoice: &ldk_node::lightning::offers::invoice::UnsignedBolt12Invoice,
        ) -> Result<ldk_node::bitcoin::secp256k1::schnorr::Signature, ()> {
            self.inner.sign_bolt12_invoice(invoice)
        }

        fn sign_gossip_message(&self, msg: UnsignedGossipMessage) -> Result<Signature, ()> {
            self.inner.sign_gossip_message(msg)
        }

        fn sign_message(&self, msg: &[u8]) -> Result<String, ()> {
            self.inner.sign_message(msg)
        }
    }

    impl SignerProvider for VlsKeysInterface {
        type EcdsaSigner = DynSigner;

        fn generate_channel_keys_id(
            &self,
            inbound: bool,
            user_channel_id: u128,
        ) -> [u8; 32] {
            self.inner.generate_channel_keys_id(inbound, user_channel_id)
        }

        fn derive_channel_signer(
            &self,
            channel_keys_id: [u8; 32],
        ) -> Self::EcdsaSigner {
            self.inner.derive_channel_signer(channel_keys_id)
        }

        fn get_destination_script(&self, channel_keys_id: [u8; 32]) -> Result<ScriptBuf, ()> {
            self.inner.get_destination_script(channel_keys_id)
        }

        fn get_shutdown_scriptpubkey(&self) -> Result<ShutdownScript, ()> {
            self.inner.get_shutdown_scriptpubkey()
        }
    }

    impl OutputSpender for VlsKeysInterface {
        fn spend_spendable_outputs(
            &self,
            descriptors: &[&SpendableOutputDescriptor],
            outputs: Vec<TxOut>,
            change_destination_script: ScriptBuf,
            feerate_sat_per_1000_weight: u32,
            locktime: Option<ldk_node::bitcoin::absolute::LockTime>,
            secp_ctx: &Secp256k1<All>,
        ) -> Result<Transaction, ()> {
            self.inner.spend_spendable_outputs(
                descriptors,
                outputs,
                change_destination_script,
                feerate_sat_per_1000_weight,
                locktime,
                secp_ctx,
            )
        }
    }

    impl ChangeDestinationSource for VlsKeysInterface {
        fn get_change_destination_script<'a>(
            &'a self,
        ) -> Pin<Box<dyn Future<Output = Result<ScriptBuf, ()>> + Send + 'a>> {
            self.inner.get_change_destination_script()
        }
    }

    impl ldk_node::KeysInterface for VlsKeysInterface {
        fn sign_invoice_hash(
            &self,
            hash: &ldk_node::bitcoin::secp256k1::Message,
        ) -> Result<RecoverableSignature, ()> {
            // Delegate to the ldk_vls2_client KeysInterface impl
            ldk_vls2_client::KeysInterface::sign_invoice_hash(&*self.inner, hash)
        }
    }
}
