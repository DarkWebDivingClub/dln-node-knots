//! LDK events, as notifications.
//!
//! The node's outbound half. Everything here follows from work the node
//! did rather than from a request, which is what makes it a notification.
//!
//! **It goes out through `Notifier::announce`, and that is the point.**
//! The code this replaces published to
//! `subscription_store::get_subscribers(type)` directly, which had three
//! defects the crate removes by construction:
//!
//! - it consulted the subscription and **not the grant**, so a controller
//!   whose grant did not permit a notification type received it anyway
//! - it published NIP-04 alongside NIP-44, which is
//!   [dln-node#3](https://github.com/DarkWebDivingClub/dln-node/issues/3)'s
//!   class of defect on the outbound path
//! - it tagged nothing, so every notification was the untagged kind
//!   mission 19 exists to make unconstructable — byte-identical to a
//!   legitimate delivery and impossible for a client to tell apart
//!
//! `announce` is the subscription route: the intersection of who
//! subscribed and who was granted, with an `a` tag naming each recipient's
//! own subscription. None of these events follows from a command, so none
//! carries an `e` tag.

use std::sync::Arc;

use ldk_node::Event;
use nostr_ln::nnc::{ChannelClosed, ChannelOpened, CloseType, Notification, NotificationType};
use nostr_ln::nwc::methods::{HoldInvoiceAccepted, Transaction, TransactionState, TransactionType};
use nostr_ln::nwc::{WalletNotification, WalletNotificationType};
use nostr_ln::service::Notifier;

use crate::lightning::LdkService;
use crate::state::forwarding_store;

/// Hex for a payment hash.
fn hex_hash(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or_default()
}

/// A payment, as NWC core's transaction record.
fn payment(direction: TransactionType, hash: String, amount: u64) -> Transaction {
    Transaction {
        transaction_type: direction,
        state: Some(TransactionState::Settled),
        invoice: None,
        description: None,
        description_hash: None,
        preimage: None,
        payment_hash: hash,
        amount,
        fees_paid: 0,
        created_at: now(),
        expires_at: None,
        settled_at: Some(now()),
        metadata: None,
    }
}

/// Handle one LDK event.
///
/// Delivery failures are logged rather than returned: an event has already
/// happened, and a node that stopped processing them because one
/// controller was unreachable would fall behind the chain.
pub async fn handle(notifier: &Notifier, ldk: &Arc<LdkService>, event: &Event) {
    match event {
        Event::PaymentReceived { payment_hash, amount_msat, .. } => {
            let n = WalletNotification::new(
                WalletNotificationType::PaymentReceived,
                payment(TransactionType::Incoming, hex_hash(&payment_hash.0), *amount_msat),
            );
            announce_wallet(notifier, n).await;
        }

        Event::PaymentSuccessful { payment_hash, fee_paid_msat, .. } => {
            let mut t =
                payment(TransactionType::Outgoing, hex_hash(&payment_hash.0), 0);
            t.fees_paid = fee_paid_msat.unwrap_or(0);
            let n = WalletNotification::new(WalletNotificationType::PaymentSent, t);
            announce_wallet(notifier, n).await;
        }

        Event::PaymentClaimable { payment_hash, claimable_amount_msat, claim_deadline, .. } => {
            // NWC-03's settle_deadline. Past it neither settling nor
            // cancelling is safe, which is why it is carried rather than
            // left for the recipient to work out.
            let n = WalletNotification::new(
                WalletNotificationType::HoldInvoiceAccepted,
                HoldInvoiceAccepted {
                    kind: "incoming".into(),
                    state: Some("accepted".into()),
                    invoice: String::new(),
                    payment_hash: hex_hash(&payment_hash.0),
                    amount: *claimable_amount_msat,
                    created_at: now(),
                    expires_at: 0,
                    settle_deadline: claim_deadline.map(|d| d as u64),
                    description: None,
                    description_hash: None,
                    metadata: None,
                },
            );
            announce_wallet(notifier, n).await;
        }

        Event::ChannelReady { channel_id, user_channel_id, counterparty_node_id, .. } => {
            // The VLS signer needs this before anything else: CheckOutpoint
            // and LockOutpoint, and it is not a notification.
            ldk.notify_channel_ready(user_channel_id.0);

            let id = channel_id.to_string();
            let found = ldk.list_channels().into_iter().find(|c| c.id == id);
            let n = Notification::new(
                NotificationType::ChannelOpened,
                ChannelOpened {
                    channel: nostr_ln::nnc::Channel {
                        id: id.clone(),
                        short_channel_id: found.as_ref().and_then(|c| c.short_channel_id.clone()),
                        peer_pubkey: counterparty_node_id
                            .map(|p| p.to_string())
                            .unwrap_or_default(),
                        state: Some(nostr_ln::nnc::ChannelState::Active),
                        is_private: found.as_ref().map(|c| c.is_private).unwrap_or(false),
                        capacity: found.as_ref().map(|c| c.capacity),
                        local_balance: found.as_ref().map(|c| c.local_balance),
                        remote_balance: found.as_ref().map(|c| c.remote_balance),
                        funding_txid: found.as_ref().and_then(|c| c.funding_txid.clone()),
                        extra: Default::default(),
                    },
                },
            );
            announce_control(notifier, n).await;
        }

        Event::ChannelClosed { channel_id, counterparty_node_id, reason, .. } => {
            let n = Notification::new(
                NotificationType::ChannelClosed,
                ChannelClosed {
                    id: channel_id.to_string(),
                    short_channel_id: None,
                    peer_pubkey: counterparty_node_id.map(|p| p.to_string()),
                    capacity: None,
                    closing_txid: None,
                    close_type: close_type(reason.as_ref()),
                },
            );
            announce_control(notifier, n).await;
        }

        Event::PaymentForwarded {
            prev_channel_id,
            next_channel_id,
            total_fee_earned_msat,
            outbound_amount_forwarded_msat,
            ..
        } => {
            let fee = total_fee_earned_msat.unwrap_or(0);
            let outgoing = outbound_amount_forwarded_msat.unwrap_or(0);
            forwarding_store::record_forward(forwarding_store::ForwardingEntry {
                incoming_channel_id: prev_channel_id.to_string(),
                outgoing_channel_id: next_channel_id.to_string(),
                incoming_amount: outgoing.saturating_add(fee),
                outgoing_amount: outgoing,
                fee_earned: fee,
                settled_at: now(),
            });
        }

        _ => {}
    }
}

/// Why a channel closed.
///
/// `ForceRemote` and `Breach` are the ones that matter: both follow from
/// no command at all, which is why `channel_closed` has a subscription
/// route in the first place.
fn close_type(reason: Option<&ldk_node::lightning::events::ClosureReason>) -> CloseType {
    use ldk_node::lightning::events::ClosureReason as R;
    match reason {
        Some(R::CounterpartyForceClosed { .. }) => CloseType::ForceRemote,
        Some(R::HolderForceClosed { .. }) => CloseType::ForceLocal,
        Some(R::LegacyCooperativeClosure)
        | Some(R::CounterpartyInitiatedCooperativeClosure)
        | Some(R::LocallyInitiatedCooperativeClosure) => CloseType::Cooperative,
        _ => CloseType::Unknown("unknown".into()),
    }
}

async fn announce_wallet(
    notifier: &Notifier,
    n: Result<WalletNotification, serde_json::Error>,
) {
    match n {
        Ok(n) => {
            if let Err(e) = notifier.announce(&n).await {
                tracing::warn!("could not announce: {e}");
            }
        }
        Err(e) => tracing::warn!("could not build notification: {e}"),
    }
}

async fn announce_control(notifier: &Notifier, n: Result<Notification, serde_json::Error>) {
    match n {
        Ok(n) => {
            if let Err(e) = notifier.announce(&n).await {
                tracing::warn!("could not announce: {e}");
            }
        }
        Err(e) => tracing::warn!("could not build notification: {e}"),
    }
}
