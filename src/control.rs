//! Node control: fourteen NIP-XX methods over LDK.
//!
//! The counterpart to [`crate::wallet`], and the same rule — a handler and
//! nothing else. `nostr-ln` owns the pipeline, the grants, the buckets and
//! the kinds.
//!
//! **Fourteen, not sixteen.** `get_pending_htlcs` and `sign_message` are
//! absent because this node does not implement them; the method list comes
//! from this impl block, so not writing them is how they stop being
//! advertised. `subscribe_notifications` is gone for a different reason —
//! the crate makes a subscription an addressable event a controller
//! publishes, not a method it calls.

use std::sync::Arc;

use nostr_ln::nnc::methods::*;
use nostr_ln::nnc::{
    Channel, ChannelFees, ChannelPolicy, ChannelState, ForwardingEvent, Hop, NetworkNode, Peer,
    Route,
};
use nostr_ln::nnc::{ErrorCode, NncError};
use nostr_ln::service::handler::Fut;
use nostr_ln::service::{Caller, ControlService, Notifier};

use crate::lightning::{LdkService, LdkServiceError};
use crate::state::forwarding_store;

pub struct Control {
    ldk: Arc<LdkService>,
    notifier: std::sync::Mutex<Option<Notifier>>,
}

impl Control {
    pub fn new(ldk: Arc<LdkService>) -> Self {
        Self { ldk, notifier: std::sync::Mutex::new(None) }
    }

    /// Taken after construction, because the notifier needs the service and
    /// the service needs the handler.
    pub fn with_notifier(self, notifier: Notifier) -> Self {
        *self.notifier.lock().unwrap() = Some(notifier);
        self
    }

    pub fn notifier(&self) -> Option<Notifier> {
        self.notifier.lock().unwrap().clone()
    }
}

fn ldk_err(what: &str, code: ErrorCode, e: LdkServiceError) -> NncError {
    NncError::new(code, format!("{what}: {e}"))
}

fn to_policy(p: &crate::lightning::NetworkChannelPolicy) -> ChannelPolicy {
    ChannelPolicy {
        base_fee: Some(p.base_fee as u64),
        fee_rate: Some(p.fee_rate as u64),
        min_htlc: Some(p.min_htlc),
        max_htlc: Some(p.max_htlc),
        time_lock_delta: Some(p.time_lock_delta as u64),
        disabled: p.disabled,
        last_update: Some(p.last_update as u64),
    }
}

#[nostr_ln::service]
impl ControlService for Control {
    // ── channels ─────────────────────────────────────────────────────

    fn list_channels<'a>(&'a self, _r: ListChannelsRequest, _c: Caller<'a>)
        -> Fut<'a, Result<ListChannelsResponse, NncError>>
    {
        Box::pin(async move {
            Ok(ListChannelsResponse {
                channels: self
                    .ldk
                    .list_channels()
                    .into_iter()
                    .map(|c| Channel {
                        id: c.id,
                        short_channel_id: c.short_channel_id,
                        peer_pubkey: c.peer_pubkey,
                        state: Some(match c.state.as_str() {
                            "active" => ChannelState::Active,
                            "inactive" => ChannelState::Inactive,
                            "pending_open" => ChannelState::PendingOpen,
                            "pending_close" => ChannelState::PendingClose,
                            "force_closing" => ChannelState::ForceClosing,
                            other => ChannelState::Unknown(other.to_string()),
                        }),
                        is_private: c.is_private,
                        capacity: Some(c.capacity),
                        local_balance: Some(c.local_balance),
                        remote_balance: Some(c.remote_balance),
                        funding_txid: c.funding_txid,
                        extra: Default::default(),
                    })
                    .collect(),
            })
        })
    }

    fn open_channel<'a>(&'a self, r: OpenChannelRequest, c: Caller<'a>)
        -> Fut<'a, Result<OpenChannelResponse, NncError>>
    {
        Box::pin(async move {
            let host = r.host.clone().ok_or_else(|| {
                NncError::new(ErrorCode::BadRequest, "host is required to open a channel")
            })?;
            self.ldk
                .open_channel(&r.pubkey, &host, r.amount_sats, r.push_amount)
                .map_err(|e| ldk_err("open_channel", ErrorCode::ChannelFailed, e))?;
            // **Asynchronous by specification.** This acknowledges; the
            // outcome arrives as `channel_opened` when the funding
            // confirms, and a caller reading this as "done" has made the
            // mistake NIP-XX spends a paragraph on.
            let _ = (c, &self.notifier);
            Ok(OpenChannelResponse {})
        })
    }

    fn close_channel<'a>(&'a self, r: CloseChannelRequest, _c: Caller<'a>)
        -> Fut<'a, Result<CloseChannelResponse, NncError>>
    {
        Box::pin(async move {
            self.ldk
                .close_channel(&r.id, r.force.unwrap_or(false))
                .map_err(|e| ldk_err("close_channel", ErrorCode::ChannelFailed, e))?;
            Ok(CloseChannelResponse {})
        })
    }

    // ── peers ────────────────────────────────────────────────────────

    fn list_peers<'a>(&'a self, _r: ListPeersRequest, _c: Caller<'a>)
        -> Fut<'a, Result<ListPeersResponse, NncError>>
    {
        Box::pin(async move {
            Ok(ListPeersResponse {
                peers: self
                    .ldk
                    .list_peers()
                    .into_iter()
                    .map(|p| Peer {
                        pubkey: p.pubkey,
                        address: Some(p.address),
                        connected: p.connected,
                        alias: p.alias,
                        num_channels: Some(p.num_channels as u64),
                    })
                    .collect(),
            })
        })
    }

    fn connect_peer<'a>(&'a self, r: ConnectPeerRequest, _c: Caller<'a>)
        -> Fut<'a, Result<ConnectPeerResponse, NncError>>
    {
        Box::pin(async move {
            let host = r.host.clone().ok_or_else(|| {
                NncError::new(ErrorCode::BadRequest, "host is required to connect")
            })?;
            self.ldk
                .connect_peer(&r.pubkey, &host)
                .map_err(|e| ldk_err("connect_peer", ErrorCode::ConnectionFailed, e))?;
            Ok(ConnectPeerResponse {})
        })
    }

    fn disconnect_peer<'a>(&'a self, r: DisconnectPeerRequest, _c: Caller<'a>)
        -> Fut<'a, Result<DisconnectPeerResponse, NncError>>
    {
        Box::pin(async move {
            self.ldk
                .disconnect_peer(&r.pubkey)
                .map_err(|e| ldk_err("disconnect_peer", ErrorCode::ConnectionFailed, e))?;
            Ok(DisconnectPeerResponse {})
        })
    }

    // ── fees ─────────────────────────────────────────────────────────

    fn get_channel_fees<'a>(&'a self, r: GetChannelFeesRequest, _c: Caller<'a>)
        -> Fut<'a, Result<GetChannelFeesResponse, NncError>>
    {
        Box::pin(async move {
            Ok(GetChannelFeesResponse {
                fees: self
                    .ldk
                    .get_channel_fees(r.id.as_deref())
                    .into_iter()
                    .map(|f| ChannelFees {
                        id: f.id,
                        short_channel_id: f.short_channel_id,
                        peer_pubkey: Some(f.peer_pubkey),
                        base_fee: f.base_fee as u64,
                        fee_rate: f.fee_rate as u64,
                        min_htlc: Some(f.min_htlc),
                        max_htlc: f.max_htlc,
                        extra: Default::default(),
                    })
                    .collect(),
            })
        })
    }

    fn set_channel_fees<'a>(&'a self, r: SetChannelFeesRequest, _c: Caller<'a>)
        -> Fut<'a, Result<SetChannelFeesResponse, NncError>>
    {
        Box::pin(async move {
            // LDK sets fees per channel and cannot set HTLC bounds, so a
            // request omitting `id` or naming `min_htlc`/`max_htlc` is
            // refused rather than partly honoured. Silently ignoring half
            // a fee policy is how a node ends up charging what nobody
            // asked it to.
            let id = r.id.as_deref().ok_or_else(|| {
                NncError::new(ErrorCode::BadRequest, "this node sets fees per channel, so id is required")
            })?;
            if r.min_htlc.is_some() || r.max_htlc.is_some() {
                return Err(NncError::new(
                    ErrorCode::BadRequest,
                    "this node cannot set min_htlc or max_htlc",
                ));
            }
            self.ldk
                .set_channel_fees(
                    id,
                    r.base_fee.and_then(|v| u32::try_from(v).ok()),
                    r.fee_rate.and_then(|v| u32::try_from(v).ok()),
                )
                .map_err(|e| ldk_err("set_channel_fees", ErrorCode::ChannelFailed, e))?;
            Ok(SetChannelFeesResponse {})
        })
    }

    // ── routing and history ──────────────────────────────────────────

    fn query_routes<'a>(&'a self, r: QueryRoutesRequest, _c: Caller<'a>)
        -> Fut<'a, Result<QueryRoutesResponse, NncError>>
    {
        Box::pin(async move {
            let max = r.max_routes.unwrap_or(1).max(1) as usize;
            Ok(QueryRoutesResponse {
                routes: self
                    .ldk
                    .find_routes(&r.destination, r.amount, max)
                    .into_iter()
                    .map(|route| Route {
                        total_fee: route.total_fee,
                        total_time_lock: route.total_time_lock as u64,
                        hops: route
                            .hops
                            .into_iter()
                            .map(|h| Hop {
                                pubkey: h.pubkey,
                                short_channel_id: Some(h.short_channel_id),
                                fee: h.fee,
                                extra: Default::default(),
                            })
                            .collect(),
                    })
                    .collect(),
            })
        })
    }

    fn get_forwarding_history<'a>(&'a self, r: GetForwardingHistoryRequest, _c: Caller<'a>)
        -> Fut<'a, Result<GetForwardingHistoryResponse, NncError>>
    {
        Box::pin(async move {
            Ok(GetForwardingHistoryResponse {
                forwards: forwarding_store::get_history(
                    r.from,
                    r.until,
                    r.limit.unwrap_or(100) as usize,
                    r.offset.unwrap_or(0) as usize,
                )
                    .into_iter()
                    .map(|f| ForwardingEvent {
                        incoming_channel_id: f.incoming_channel_id,
                        outgoing_channel_id: f.outgoing_channel_id,
                        incoming_amount: f.incoming_amount,
                        outgoing_amount: f.outgoing_amount,
                        fee_earned: f.fee_earned,
                        settled_at: f.settled_at,
                    })
                    .collect(),
            })
        })
    }

    // ── the graph ────────────────────────────────────────────────────

    fn list_network_nodes<'a>(&'a self, r: ListNetworkNodesRequest, _c: Caller<'a>)
        -> Fut<'a, Result<ListNetworkNodesResponse, NncError>>
    {
        Box::pin(async move {
            Ok(ListNetworkNodesResponse {
                nodes: self
                    .ldk
                    .list_network_nodes(
                        r.limit.unwrap_or(100) as usize,
                        r.offset.unwrap_or(0) as usize,
                    )
                    .into_iter()
                    .map(to_network_node)
                    .collect(),
            })
        })
    }

    fn get_network_node<'a>(&'a self, r: GetNetworkNodeRequest, _c: Caller<'a>)
        -> Fut<'a, Result<GetNetworkNodeResponse, NncError>>
    {
        Box::pin(async move {
            let found = self
                .ldk
                .get_network_node(&r.pubkey)
                .map_err(|e| ldk_err("get_network_node", ErrorCode::Internal, e))?
                .ok_or_else(|| NncError::new(ErrorCode::NotFound, "no such node in the graph"))?;
            Ok(to_network_node(found))
        })
    }

    fn get_network_stats<'a>(&'a self, _r: GetNetworkStatsRequest, _c: Caller<'a>)
        -> Fut<'a, Result<GetNetworkStatsResponse, NncError>>
    {
        Box::pin(async move {
            let s = self.ldk.get_network_stats();
            Ok(GetNetworkStatsResponse {
                num_nodes: s.num_nodes as u64,
                num_channels: s.num_channels as u64,
                total_capacity: s.total_capacity,
                avg_channel_size: Some(s.avg_channel_size),
                max_channel_size: Some(s.max_channel_size),
            })
        })
    }

    fn get_network_channel<'a>(&'a self, r: GetNetworkChannelRequest, _c: Caller<'a>)
        -> Fut<'a, Result<GetNetworkChannelResponse, NncError>>
    {
        Box::pin(async move {
            let ch = self
                .ldk
                .get_network_channel(&r.short_channel_id)
                .map_err(|e| ldk_err("get_network_channel", ErrorCode::Internal, e))?
                .ok_or_else(|| {
                    NncError::new(ErrorCode::NotFound, "no such channel in the graph")
                })?;
            Ok(GetNetworkChannelResponse {
                short_channel_id: ch.short_channel_id,
                capacity: ch.capacity,
                node1_pubkey: ch.node1_pubkey,
                node2_pubkey: ch.node2_pubkey,
                node1_policy: ch.node1_policy.as_ref().map(to_policy),
                node2_policy: ch.node2_policy.as_ref().map(to_policy),
            })
        })
    }
}

fn to_network_node(n: crate::lightning::NetworkNodeInfo) -> NetworkNode {
    NetworkNode {
        pubkey: n.pubkey,
        alias: Some(n.alias),
        color: Some(n.color),
        num_channels: Some(n.num_channels as u64),
        total_capacity: Some(n.total_capacity),
        addresses: Some(n.addresses),
        last_update: Some(n.last_update as u64),
        extra: Default::default(),
    }
}
