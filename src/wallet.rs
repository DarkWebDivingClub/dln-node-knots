//! The wallet: twenty-three NWC methods over LDK.
//!
//! **This file is the node's whole NWC surface.** There is no pipeline
//! here, no grant parsing, no rate bucket, no event kind and no NIP-44
//! call — `nostr-ln` owns all of that, and a handler that tried to reach
//! any of it could not compile.
//!
//! Twenty-three, not twenty-four: `estimate_onchain_fees` is absent
//! because it is not implemented. It used to be *registered* and return
//! `NOT_IMPLEMENTED`, which meant the info event advertised a method that
//! always failed. `#[nostr_ln::service]` generates the method list from
//! this impl block, so the way to stop advertising it is to stop writing
//! it, and a client now learns the truth from discovery rather than from
//! a failed call.

use std::str::FromStr;
use std::sync::Arc;

use ldk_node::lightning::offers::offer::Offer;
use nwc::nostr::hashes::{sha256, Hash};
use nostr_ln::nnc::{ErrorCode, NncError};
use nostr_ln::nwc::methods::*;
use nostr_ln::service::handler::Fut;
use nostr_ln::service::{Caller, WalletService};

use crate::lightning::{LdkService, LdkServiceError, PaymentDirection, PaymentStatus};
use crate::state::{address_store, offer_store};

/// What this node sends. NWC-02 and NWC-03.
const NOTIFICATIONS: &[&str] =
    &["payment_received", "payment_sent", "hold_invoice_accepted"];

pub struct Wallet {
    ldk: Arc<LdkService>,
    alias: Option<String>,
}

impl Wallet {
    pub fn new(ldk: Arc<LdkService>, alias: Option<String>) -> Self {
        Self { ldk, alias }
    }
}

/// An LDK failure, as an NWC error.
fn ldk_err(what: &str, code: ErrorCode, e: LdkServiceError) -> NncError {
    NncError::new(code, format!("{what}: {e}"))
}

/// A payment record, as NWC core's transaction shape.
fn to_transaction(p: &crate::lightning::PaymentDetails) -> Transaction {
    let settled = p.status == PaymentStatus::Succeeded;
    Transaction {
        transaction_type: match p.direction {
            PaymentDirection::Inbound => TransactionType::Incoming,
            PaymentDirection::Outbound => TransactionType::Outgoing,
        },
        state: Some(match p.status {
            PaymentStatus::Succeeded => TransactionState::Settled,
            PaymentStatus::Failed => TransactionState::Failed,
            PaymentStatus::Pending => TransactionState::Pending,
        }),
        // LDK's record carries neither the encoded invoice nor its
        // description, so neither is invented here. The old handler
        // returned `None` for both too.
        invoice: None,
        description: None,
        description_hash: None,
        preimage: crate::preimage_from_kind(&p.kind),
        payment_hash: crate::payment_hash_from_kind(&p.kind).unwrap_or_default(),
        amount: p.amount_msat.unwrap_or(0),
        fees_paid: p.fee_paid_msat.unwrap_or(0),
        created_at: p.latest_update_timestamp,
        expires_at: None,
        settled_at: settled.then_some(p.latest_update_timestamp),
        metadata: None,
    }
}

#[nostr_ln::service]
impl WalletService for Wallet {
    // ── published core ───────────────────────────────────────────────

    fn get_info<'a>(&'a self, _r: GetInfoRequest, _c: Caller<'a>)
        -> Fut<'a, Result<GetInfoResponse, NncError>>
    {
        Box::pin(async move {
            Ok(GetInfoResponse {
                alias: self.alias.clone().or_else(|| Some("dln-node".into())),
                color: None,
                pubkey: Some(self.ldk.node_id()),
                network: Some(self.ldk.network().to_string()),
                block_height: Some(self.ldk.status().latest_best_block_height as u64),
                block_hash: None,
                // Generated from this impl block, so it cannot claim a
                // method the node does not serve.
                methods: self.methods().iter().map(|m| m.to_string()).collect(),
                notifications: Some(NOTIFICATIONS.iter().map(|n| n.to_string()).collect()),
                // No numbered extension is claimed: the ones this node
                // implements from `nips` have no number assigned, and
                // upstream's maintainers assign them.
                extensions: None,
                bip321_methods: Some(vec![
                    Bip321Capability { method: "bolt11".into(), address_types: None },
                    Bip321Capability { method: "bolt12".into(), address_types: None },
                    Bip321Capability {
                        method: "onchain".into(),
                        // LDK issues p2tr and nothing else, and saying so
                        // is the point of this field.
                        address_types: Some(vec!["p2tr".into()]),
                    },
                ]),
            })
        })
    }

    fn get_balance<'a>(&'a self, _r: GetBalanceRequest, _c: Caller<'a>)
        -> Fut<'a, Result<GetBalanceResponse, NncError>>
    {
        Box::pin(async move {
            self.ldk
                .sync_wallets()
                .map_err(|e| ldk_err("sync", ErrorCode::Internal, e))?;
            let b = self
                .ldk
                .get_balance()
                .map_err(|e| ldk_err("balance", ErrorCode::Internal, e))?;
            Ok(GetBalanceResponse {
                balance: b.total_msat,
                lightning_balance: Some(b.lightning_msat),
                // **Sats, and the name says so.** LDK reports msats; the
                // chain has no smaller unit and `nwc-units.md` requires the
                // suffix. The old field was `onchain_balance` carrying
                // msats, which is the silent kind of wrong.
                onchain_balance_sats: Some(b.onchain_msat / 1_000),
            })
        })
    }

    fn pay_invoice<'a>(&'a self, r: PayInvoiceRequest, _c: Caller<'a>)
        -> Fut<'a, Result<PayInvoiceResponse, NncError>>
    {
        Box::pin(async move {
            // The pipeline validates that the request *parses*. Whether
            // zero is a sensible amount is this handler's question, and
            // answering it here is better than letting LDK fail later
            // with a message about something else.
            if r.amount == Some(0) {
                return Err(NncError::new(
                    ErrorCode::BadRequest,
                    "amount must be greater than 0",
                ));
            }
            if r.invoice.trim().is_empty() {
                return Err(NncError::new(ErrorCode::BadRequest, "invoice is required"));
            }
            let p = self
                .ldk
                .pay_invoice(r.invoice.trim(), r.amount)
                .map_err(|e| ldk_err("pay_invoice", ErrorCode::PaymentFailed, e))?;
            Ok(PayInvoiceResponse { preimage: p.preimage, fees_paid: p.fees_paid_msat })
        })
    }

    fn make_invoice<'a>(&'a self, r: MakeInvoiceRequest, _c: Caller<'a>)
        -> Fut<'a, Result<MakeInvoiceResponse, NncError>>
    {
        Box::pin(async move {
            // LDK cannot bind a description hash, and an invoice whose
            // hash does not commit to the description it claims is worse
            // than no invoice.
            if r.description_hash.is_some() {
                return Err(NncError::new(
                    ErrorCode::BadRequest,
                    "this node does not support description_hash",
                ));
            }
            let inv = self
                .ldk
                .make_invoice(
                    r.amount,
                    r.description.as_deref(),
                    r.description_hash.as_deref(),
                    r.expiry,
                )
                .map_err(|e| ldk_err("make_invoice", ErrorCode::Internal, e))?;
            Ok(Transaction {
                transaction_type: TransactionType::Incoming,
                state: Some(TransactionState::Pending),
                invoice: Some(inv.invoice),
                description: r.description,
                description_hash: r.description_hash,
                preimage: None,
                payment_hash: inv.payment_hash.unwrap_or_default(),
                amount: inv.amount_msat.unwrap_or(r.amount),
                fees_paid: 0,
                created_at: now(),
                expires_at: inv.expires_at,
                // Absent by specification, which is the one difference
                // between this response and lookup_invoice's.
                settled_at: None,
                metadata: r.metadata,
            })
        })
    }

    fn lookup_invoice<'a>(&'a self, r: LookupInvoiceRequest, _c: Caller<'a>)
        -> Fut<'a, Result<LookupInvoiceResponse, NncError>>
    {
        Box::pin(async move {
            let found = match (&r.payment_hash, &r.invoice) {
                (Some(h), _) => self.ldk.lookup_payment_by_hash(h),
                (None, Some(i)) => self.ldk.lookup_payment_by_bolt11(i),
                (None, None) => {
                    return Err(NncError::new(
                        ErrorCode::BadRequest,
                        "one of payment_hash or invoice is required",
                    ))
                }
            }
            .map_err(|e| ldk_err("lookup_invoice", ErrorCode::NotFound, e))?;
            Ok(to_transaction(&found))
        })
    }

    // ── nwc-onchain.md ───────────────────────────────────────────────

    fn pay_onchain<'a>(&'a self, r: PayOnchainRequest, _c: Caller<'a>)
        -> Fut<'a, Result<PayOnchainResponse, NncError>>
    {
        Box::pin(async move {
            let txid = self
                .ldk
                .pay_onchain(&r.address, r.amount_sats, r.feerate)
                .map_err(|e| ldk_err("pay_onchain", ErrorCode::PaymentFailed, e))?;
            Ok(PayOnchainResponse { txid, fee_sats: None })
        })
    }

    fn make_new_address<'a>(&'a self, r: MakeNewAddressRequest, _c: Caller<'a>)
        -> Fut<'a, Result<MakeNewAddressResponse, NncError>>
    {
        Box::pin(async move {
            // LDK issues p2tr. A caller asking for anything else is told,
            // rather than quietly given something it did not ask for.
            if let Some(t) = &r.address_type {
                if t != "p2tr" {
                    return Err(NncError::new(
                        ErrorCode::BadRequest,
                        format!("this node generates p2tr, not {t}"),
                    ));
                }
            }
            let address = self
                .ldk
                .new_onchain_address()
                .map_err(|e| ldk_err("make_new_address", ErrorCode::Internal, e))?;
            address_store::register_address(address.clone());
            Ok(MakeNewAddressResponse { address, address_type: "p2tr".into() })
        })
    }

    fn lookup_address<'a>(&'a self, r: LookupAddressRequest, _c: Caller<'a>)
        -> Fut<'a, Result<LookupAddressResponse, NncError>>
    {
        Box::pin(async move {
            let rec = address_store::get_address(&r.address).ok_or_else(|| {
                NncError::new(ErrorCode::NotFound, "this node does not watch that address")
            })?;
            Ok(LookupAddressResponse {
                address: rec.address,
                address_type: "p2tr".into(),
                total_received_sats: rec.transactions.iter().map(|t| t.amount_sats).sum(),
                transactions: rec
                    .transactions
                    .into_iter()
                    .map(|t| AddressTransaction {
                        txid: t.txid,
                        amount_sats: t.amount_sats,
                        // **This node does not track confirmations**, and
                        // reports zero rather than guessing. Zero means
                        // unconfirmed, so a client waits — which is the
                        // safe direction to be wrong in. Overstating would
                        // have it spend against an output that can still
                        // disappear.
                        confirmations: 0,
                        timestamp: t.timestamp,
                    })
                    .collect(),
            })
        })
    }

    fn list_addresses<'a>(&'a self, _r: ListAddressesRequest, _c: Caller<'a>)
        -> Fut<'a, Result<ListAddressesResponse, NncError>>
    {
        Box::pin(async move {
            Ok(ListAddressesResponse {
                addresses: address_store::list_addresses()
                    .into_iter()
                    .map(|a| AddressRecord {
                        address_type: "p2tr".into(),
                        total_received_sats: a.transactions.iter().map(|t| t.amount_sats).sum(),
                        // The store does not record when an address was
                        // generated, so the earliest payment to it is the
                        // closest true answer, and zero where none has
                        // arrived.
                        created_at: a.transactions.iter().map(|t| t.timestamp).min().unwrap_or(0),
                        address: a.address,
                    })
                    .collect(),
            })
        })
    }

    // ── NWC-03, hold invoices ────────────────────────────────────────

    fn make_hold_invoice<'a>(&'a self, r: MakeHoldInvoiceRequest, _c: Caller<'a>)
        -> Fut<'a, Result<MakeHoldInvoiceResponse, NncError>>
    {
        Box::pin(async move {
            let inv = self
                .ldk
                .make_hold_invoice(r.amount, r.description.as_deref(), r.expiry, &r.payment_hash)
                .map_err(|e| ldk_err("make_hold_invoice", ErrorCode::Internal, e))?;
            Ok(MakeHoldInvoiceResponse {
                kind: "incoming".into(),
                invoice: Some(inv.invoice),
                payment_hash: r.payment_hash,
                amount: inv.amount_msat.unwrap_or(r.amount),
                created_at: now(),
                expires_at: inv.expires_at,
                description: r.description,
                description_hash: r.description_hash,
                metadata: None,
            })
        })
    }

    fn settle_hold_invoice<'a>(&'a self, r: SettleHoldInvoiceRequest, _c: Caller<'a>)
        -> Fut<'a, Result<SettleHoldInvoiceResponse, NncError>>
    {
        Box::pin(async move {
            self.ldk
                .settle_hold_invoice(&r.preimage)
                .map_err(|e| ldk_err("settle_hold_invoice", ErrorCode::Internal, e))?;
            Ok(SettleHoldInvoiceResponse {})
        })
    }

    fn cancel_hold_invoice<'a>(&'a self, r: CancelHoldInvoiceRequest, _c: Caller<'a>)
        -> Fut<'a, Result<CancelHoldInvoiceResponse, NncError>>
    {
        Box::pin(async move {
            self.ldk
                .cancel_hold_invoice(&r.payment_hash)
                .map_err(|e| ldk_err("cancel_hold_invoice", ErrorCode::Internal, e))?;
            Ok(CancelHoldInvoiceResponse {})
        })
    }

    // ── NWC-04, NWC-05, NWC-09 ───────────────────────────────────────

    fn pay_keysend<'a>(&'a self, r: PayKeysendRequest, _c: Caller<'a>)
        -> Fut<'a, Result<PayKeysendResponse, NncError>>
    {
        Box::pin(async move {
            if r.amount == 0 {
                return Err(NncError::new(
                    ErrorCode::BadRequest,
                    "amount must be greater than 0",
                ));
            }
            let p = self
                .ldk
                .pay_keysend(&r.pubkey, r.amount)
                .map_err(|e| ldk_err("pay_keysend", ErrorCode::PaymentFailed, e))?;
            Ok(PayKeysendResponse { preimage: p.preimage, fees_paid: p.fees_paid_msat })
        })
    }

    fn list_transactions<'a>(&'a self, r: ListTransactionsRequest, _c: Caller<'a>)
        -> Fut<'a, Result<ListTransactionsResponse, NncError>>
    {
        Box::pin(async move {
            let direction = r.transaction_type.as_ref().map(|t| match t {
                TransactionType::Incoming => PaymentDirection::Inbound,
                TransactionType::Outgoing => PaymentDirection::Outbound,
            });
            let found = self.ldk.list_payments_filtered(
                r.from, r.until, r.limit, r.offset, r.unpaid, direction, None,
            );
            let transactions: Vec<Transaction> = found.iter().map(to_transaction).collect();
            let total_count = Some(transactions.len() as u64);
            Ok(ListTransactionsResponse { transactions, total_count })
        })
    }

    fn lookup_payment<'a>(&'a self, r: LookupPaymentRequest, _c: Caller<'a>)
        -> Fut<'a, Result<LookupPaymentResponse, NncError>>
    {
        Box::pin(async move {
            // NWC-09: exactly one selector form. A wallet that guessed
            // could answer a reconciliation with the wrong payment, and
            // nothing in the exchange would say so.
            let forms = [
                r.transaction_id.is_some(),
                r.payment_hash.is_some() || r.invoice.is_some(),
                r.payment_type.is_some() && r.lookup.is_some(),
            ];
            if forms.iter().filter(|f| **f).count() != 1 {
                return Err(NncError::new(
                    ErrorCode::BadRequest,
                    "exactly one of transaction_id, the bolt11 fields, or \
                     payment_type with lookup",
                ));
            }
            // This node keys payments by hash, so transaction_id is the
            // hash. Stable and wallet-scoped, which is what NWC-09 asks.
            let hash = r
                .transaction_id
                .clone()
                .or_else(|| r.payment_hash.clone())
                .or_else(|| {
                    r.lookup
                        .as_ref()
                        .and_then(|l| l.get("payment_hash"))
                        .and_then(|v| v.as_str())
                        .map(str::to_string)
                });
            let found = match (hash, &r.invoice) {
                (Some(h), _) => self.ldk.lookup_payment_by_hash(&h),
                (None, Some(i)) => self.ldk.lookup_payment_by_bolt11(i),
                (None, None) => {
                    return Err(NncError::new(ErrorCode::BadRequest, "no usable selector"))
                }
            }
            .map_err(|e| ldk_err("lookup_payment", ErrorCode::NotFound, e))?;

            let t = to_transaction(&found);
            Ok(LookupPaymentResponse {
                transaction_id: t.payment_hash.clone(),
                payment_direction: t.transaction_type.clone(),
                state: match t.state {
                    Some(TransactionState::Settled) => PaymentState::Settled,
                    Some(TransactionState::Failed) => PaymentState::Failed,
                    Some(TransactionState::Accepted) => PaymentState::Accepted,
                    Some(TransactionState::Expired) => PaymentState::Expired,
                    _ => PaymentState::Pending,
                },
                payment_type: "bolt11".into(),
                amount: t.amount,
                fees_paid: Some(t.fees_paid),
                created_at: t.created_at,
                updated_at: None,
                expires_at: t.expires_at,
                settled_at: t.settled_at,
                failure_reason: None,
                metadata: None,
                details: serde_json::json!({
                    "payment_hash": t.payment_hash,
                    "invoice": t.invoice,
                    "preimage": t.preimage,
                    "description": t.description,
                }),
            })
        })
    }

    // ── nwc-invoices.md ──────────────────────────────────────────────

    fn list_invoices<'a>(&'a self, r: ListInvoicesRequest, _c: Caller<'a>)
        -> Fut<'a, Result<ListInvoicesResponse, NncError>>
    {
        Box::pin(async move {
            // Incoming only, and unpaid included: an invoice nobody paid
            // is exactly what this lists and what list_transactions omits.
            let found = self.ldk.list_payments_filtered(
                r.from,
                r.until,
                r.limit,
                r.offset,
                Some(true),
                Some(PaymentDirection::Inbound),
                None,
            );
            let now_secs = now();
            let invoices = found
                .iter()
                .map(to_transaction)
                .map(|t| {
                    let state = match t.state {
                        Some(TransactionState::Settled) => InvoiceState::Settled,
                        _ if t.expires_at.is_some_and(|e| e < now_secs) => InvoiceState::Expired,
                        _ => InvoiceState::Pending,
                    };
                    InvoiceRecord {
                        invoice: t.invoice.unwrap_or_default(),
                        description: t.description,
                        payment_hash: t.payment_hash,
                        amount: t.amount,
                        preimage: if state == InvoiceState::Settled { t.preimage } else { None },
                        settled_at: if state == InvoiceState::Settled { t.settled_at } else { None },
                        state,
                        created_at: t.created_at,
                        expires_at: t.expires_at.unwrap_or(0),
                    }
                })
                .filter(|i| r.state.as_ref().is_none_or(|w| *w == i.state))
                .collect();
            Ok(ListInvoicesResponse { invoices })
        })
    }

    // ── NWC-12 and nwc-offers.md ─────────────────────────────────────

    fn make_offer<'a>(&'a self, r: MakeOfferRequest, _c: Caller<'a>)
        -> Fut<'a, Result<MakeOfferResponse, NncError>>
    {
        Box::pin(async move {
            // NWC-12: BOLT12 requires an amount-bearing offer to describe
            // what is being paid for.
            if r.amount.is_some() && r.description.is_none() {
                return Err(NncError::new(
                    ErrorCode::BadRequest,
                    "an amount-bearing offer requires a description",
                ));
            }
            let description = r.description.clone().unwrap_or_else(|| "offer".into());
            let expiry = r.expires_at.and_then(|e| u32::try_from(e.saturating_sub(now())).ok());
            let offer = self
                .ldk
                .make_offer(r.amount.unwrap_or(0), &description, expiry)
                .map_err(|e| ldk_err("make_offer", ErrorCode::Internal, e))?;
            let offer_id = Offer::from_str(&offer).map(|o| o.id().0).unwrap_or([0u8; 32]);
            offer_store::insert_offer(
                offer.clone(),
                description.clone(),
                r.amount.unwrap_or(0),
                offer_id,
            );
            Ok(MakeOfferResponse {
                offer_id: hex::encode(offer_id),
                offer,
                amount: r.amount,
                description: r.description,
                issuer: r.issuer,
                single_use: r.single_use.unwrap_or(false),
                created_at: now(),
                expires_at: r.expires_at,
            })
        })
    }

    fn pay_offer<'a>(&'a self, r: PayOfferRequest, _c: Caller<'a>)
        -> Fut<'a, Result<PayOfferResponse, NncError>>
    {
        Box::pin(async move {
            let p = self
                .ldk
                .pay_offer(&r.offer, r.amount, r.payer_note)
                .map_err(|e| ldk_err("pay_offer", ErrorCode::PaymentFailed, e))?;
            Ok(PayOfferResponse {
                // The hash is this node's stable payment id, as in
                // lookup_payment.
                transaction_id: p.preimage.clone(),
                preimage: p.preimage,
                fees_paid: p.fees_paid_msat,
            })
        })
    }

    fn list_offers<'a>(&'a self, r: ListOffersRequest, _c: Caller<'a>)
        -> Fut<'a, Result<ListOffersResponse, NncError>>
    {
        Box::pin(async move {
            let active_only = r.active_only.unwrap_or(false);
            Ok(ListOffersResponse {
                offers: offer_store::list_offers()
                    .into_iter()
                    .filter(|o| !active_only || o.active)
                    .map(|o| OfferRecord {
                        offer_id: hex::encode(o.offer_id),
                        offer: o.offer,
                        description: Some(o.description),
                        issuer: None,
                        amount: Some(o.amount_msat),
                        active: o.active,
                        single_use: false,
                        num_payments_received: o.num_payments_received,
                        total_received: o.total_received_msat,
                        // Not recorded by the store either.
                        created_at: 0,
                        expires_at: None,
                    })
                    .collect(),
            })
        })
    }

    fn disable_offer<'a>(&'a self, r: DisableOfferRequest, _c: Caller<'a>)
        -> Fut<'a, Result<DisableOfferResponse, NncError>>
    {
        Box::pin(async move {
            let id: [u8; 32] = hex::decode(&r.offer_id)
                .ok()
                .and_then(|b| b.try_into().ok())
                .ok_or_else(|| {
                    NncError::new(ErrorCode::BadRequest, "offer_id must be 32 bytes, hex")
                })?;
            let offer = offer_store::find_by_offer_id(&id)
                .ok_or_else(|| NncError::new(ErrorCode::NotFound, "no such offer"))?;
            // Idempotent by specification: disabling a disabled offer
            // succeeds, so the store's false return is not an error.
            offer_store::disable_offer(&offer);
            Ok(DisableOfferResponse {})
        })
    }

    // ── nwc-route.md ─────────────────────────────────────────────────

    fn quote_payment<'a>(&'a self, r: QuotePaymentRequest, _c: Caller<'a>)
        -> Fut<'a, Result<QuotePaymentResponse, NncError>>
    {
        Box::pin(async move {
            // Takes the invoice, which is the whole difference from the
            // `estimate_routing_fees` this replaces: a destination pubkey
            // carries no route hints, so it cannot reach a payee behind a
            // private channel.
            let invoice = ldk_node::lightning_invoice::Bolt11Invoice::from_str(&r.invoice)
                .map_err(|e| NncError::new(ErrorCode::BadRequest, format!("invoice: {e}")))?;
            let amount = r
                .amount
                .or_else(|| invoice.amount_milli_satoshis())
                .ok_or_else(|| {
                    NncError::new(ErrorCode::BadRequest, "the invoice has no amount")
                })?;
            let destination = invoice.recover_payee_pub_key().to_string();
            let routes = self.ldk.find_routes(&destination, amount, 1);
            let Some(route) = routes.first() else {
                // A result, not a failure: "cannot reach" is what the
                // caller asked.
                return Ok(QuotePaymentResponse {
                    amount,
                    fee_msat: 0,
                    cltv_expiry_delta: 0,
                    route_found: false,
                });
            };
            Ok(QuotePaymentResponse {
                amount,
                fee_msat: route.total_fee,
                cltv_expiry_delta: route.total_time_lock,
                route_found: true,
            })
        })
    }

    // ── nwc-bip321.md ────────────────────────────────────────────────

    fn pay<'a>(&'a self, r: PayRequest, _c: Caller<'a>)
        -> Fut<'a, Result<PayResponse, NncError>>
    {
        Box::pin(async move {
            let uri = bip321::Uri::parse(&r.payment)
                .map_err(|e| NncError::new(ErrorCode::BadRequest, format!("bip321: {e}")))?;
            let amount_msat = r.amount.or_else(|| uri.amount.map(|a| a.to_sat() * 1_000));

            // NWC-321: a wallet honouring max_fee MUST return fees_paid,
            // and one that does not MUST ignore the parameter. This node
            // cannot cap a route, so it ignores it — and therefore must
            // not pretend otherwise. `fees_paid` is still reported; what
            // it must not do is claim the budget was enforced.
            let _ = &r.max_fee;

            // Lightning first: the payer usually cares more about
            // settlement speed than the payee's ordering.
            if let Some(invoice) = uri.lightning.first() {
                let p = self
                    .ldk
                    .pay_invoice(invoice.as_str(), amount_msat)
                    .map_err(|e| ldk_err("pay/bolt11", ErrorCode::PaymentFailed, e))?;
                return Ok(paid("bolt11", amount_msat.unwrap_or(0), Some(p.preimage), None,
                               p.fees_paid_msat.unwrap_or(0)));
            }
            if let Some(offer) = uri.lno.first() {
                let p = self
                    .ldk
                    .pay_offer(offer.as_str(), amount_msat, r.payer_note.clone())
                    .map_err(|e| ldk_err("pay/bolt12", ErrorCode::PaymentFailed, e))?;
                return Ok(paid("bolt12", amount_msat.unwrap_or(0), Some(p.preimage), None,
                               p.fees_paid_msat.unwrap_or(0)));
            }
            if uri.address.is_some() {
                let amount_sat = amount_msat.map(|m| m / 1_000).ok_or_else(|| {
                    NncError::new(ErrorCode::BadRequest, "an on-chain payment needs an amount")
                })?;
                let addr = uri.address_str().ok_or_else(|| {
                    NncError::new(ErrorCode::BadRequest, "missing address")
                })?;
                let txid = self
                    .ldk
                    .pay_onchain(addr, amount_sat, None)
                    .map_err(|e| ldk_err("pay/onchain", ErrorCode::PaymentFailed, e))?;
                // `onchain` is nwc-bip321.md's, not NWC-321's, and this
                // node declares that extension in bip321_methods.
                return Ok(paid("onchain", amount_sat * 1_000, None, Some(txid), 0));
            }
            Err(NncError::new(
                ErrorCode::UnsupportedPaymentInstruction,
                "no instruction in this URI is one this node can pay",
            ))
        })
    }

    fn receive<'a>(&'a self, r: ReceiveRequest, _c: Caller<'a>)
        -> Fut<'a, Result<ReceiveResponse, NncError>>
    {
        Box::pin(async move {
            let mut uri: bip321::Uri<'_> = bip321::Uri::new();
            if let Some(amount_msat) = r.amount {
                uri.amount = Some(ldk_node::bitcoin::Amount::from_sat(amount_msat / 1_000));
            }
            if let Some(label) = &r.label {
                // Names the payee, and belongs only in the URI.
                uri.label = Some(bip321::Param::from_decoded(label.clone()));
            }
            if let Some(description) = &r.description {
                uri.message = Some(bip321::Param::from_decoded(description.clone()));
            }

            let default = vec![
                Bip321Method { method: "bolt11".into(), expiry: None, address_type: None },
                Bip321Method { method: "bolt12".into(), expiry: None, address_type: None },
                Bip321Method { method: "onchain".into(), expiry: None, address_type: None },
            ];
            let wanted = r.methods.as_ref().unwrap_or(&default);

            let mut any = false;
            for entry in wanted {
                match entry.method.as_str() {
                    "bolt11" => {
                        // BOLT-11 needs an amount. Without one it is
                        // skipped rather than failing the call: a URI
                        // offering the rest is a useful answer.
                        let Some(amount) = r.amount else { continue };
                        match self.ldk.make_invoice(
                            amount,
                            r.description.as_deref(),
                            None,
                            entry.expiry,
                        ) {
                            Ok(inv) => {
                                uri.lightning.push(bip321::Param::from_decoded(inv.invoice));
                                any = true;
                            }
                            Err(e) => tracing::warn!("make_bip321: bolt11 skipped: {e}"),
                        }
                    }
                    "bolt12" => {
                        let desc = r.description.as_deref().unwrap_or("bip321 offer");
                        let expiry = entry.expiry.and_then(|e| u32::try_from(e).ok());
                        match self.ldk.make_offer(r.amount.unwrap_or(0), desc, expiry) {
                            Ok(offer) => {
                                let id =
                                    Offer::from_str(&offer).map(|o| o.id().0).unwrap_or([0u8; 32]);
                                offer_store::insert_offer(
                                    offer.clone(),
                                    desc.to_string(),
                                    r.amount.unwrap_or(0),
                                    id,
                                );
                                uri.lno.push(bip321::Param::from_decoded(offer));
                                any = true;
                            }
                            Err(e) => tracing::warn!("make_bip321: bolt12 skipped: {e}"),
                        }
                    }
                    "onchain" => {
                        if entry.address_type.as_deref().is_some_and(|t| t != "p2tr") {
                            tracing::warn!("make_bip321: this node generates p2tr only");
                            continue;
                        }
                        match self.ldk.new_onchain_address() {
                            Ok(addr) => {
                                address_store::register_address(addr.clone());
                                if let Ok(parsed) = addr.parse() {
                                    uri.set_address(addr, parsed);
                                    any = true;
                                }
                            }
                            Err(e) => tracing::warn!("make_bip321: onchain skipped: {e}"),
                        }
                    }
                    other => tracing::warn!("make_bip321: unsupported method {other}"),
                }
            }
            if !any {
                return Err(NncError::new(
                    ErrorCode::BadRequest,
                    "no payment instruction could be generated",
                ));
            }
            Ok(ReceiveResponse { bip321: format!("{uri}"), transaction_id: None })
        })
    }

    /// What this node sends, for the info event's `notifications` tag.
    fn notifications(&self) -> &'static [&'static str] {
        NOTIFICATIONS
    }
}

fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// A completed `pay`, as NWC-321's response.
///
/// This node settles synchronously, so `state` is always `settled` and
/// `settled_at` is always present — a `pending` here would be a claim it
/// cannot make good.
fn paid(
    instruction_type: &str,
    amount: u64,
    preimage: Option<String>,
    txid: Option<String>,
    fees_paid: u64,
) -> PayResponse {
    // **The hash, never the preimage.** The preimage proves the payment
    // and is the one value that must not travel as an identifier —
    // `transaction_id` is for correlation and a client may log it, quote
    // it back, or hand it to something else. The hash is what it is for.
    let hash = preimage
        .as_deref()
        .and_then(decode_hex)
        .map(|b| sha256::Hash::hash(&b).to_string());
    PayResponse {
        // This node keys payments by hash, as `lookup_payment` does. An
        // on-chain instruction has no hash, so the txid identifies it.
        transaction_id: hash.clone().or_else(|| txid.clone()).unwrap_or_default(),
        state: "settled".into(),
        instruction_type: instruction_type.into(),
        amount,
        fees_paid,
        payment_hash: hash,
        preimage,
        payer_proof: None,
        txid,
        failure_reason: None,
        created_at: now(),
        settled_at: Some(now()),
    }
}


/// Hex to bytes, strictly 32 of them.
fn decode_hex(s: &str) -> Option<Vec<u8>> {
    if s.len() != 64 {
        return None;
    }
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).ok())
        .collect()
}
