//! The wallet handler against a real LDK node and a real `bitcoind`.
//!
//! **Ported by mission 25.3, and smaller for it.** These nine tests used
//! to spin up a relay, publish a grant, encrypt a NIP-47 request, wait on
//! a notification stream and decrypt a response — about a hundred lines
//! each — in order to find out whether LDK produces an invoice.
//!
//! None of that was what they were testing. The relay, the grant and the
//! encryption are `nostr-ln`'s now and `nostr-ln-e2e-test` exercises them
//! over a real relay for every method. What is left here is the part only
//! this repository can answer: does the handler do the right thing with
//! the node underneath it.
//!
//! Three of them found something while being ported. `pay_invoice` with a
//! zero amount, `pay_keysend` with a zero amount, and `make_invoice` with
//! a `description_hash` all used to be refused by a `validate` hook that
//! the crate's pipeline does not replace — it validates that a request
//! *parses*, which is a different question. The guards are back in the
//! handler, where they were always a business rule rather than a parsing
//! one.

mod common;

use std::sync::Arc;

use dln_node::lightning::{LdkService, LdkServiceConfig};
use dln_node::wallet::Wallet;
use nostr_ln::nnc::ErrorCode;
use nostr_ln::nwc::methods::*;
use nostr_ln::service::{Caller, WalletService};
use nostr_sdk::prelude::Keys;

use common::bitcoind::BitcoindHarness;

fn unique_storage_dir(prefix: &str) -> String {
    format!(
        "/tmp/{prefix}-{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock should be monotonic")
            .as_nanos()
    )
}

/// A wallet handler over a node backed by a fresh `bitcoind`.
async fn wallet(prefix: &str) -> (BitcoindHarness, Arc<LdkService>, Wallet) {
    let bitcoind = BitcoindHarness::start().await;
    let cfg = LdkServiceConfig {
        network: "regtest".to_string(),
        bitcoind_rpc_host: bitcoind.rpc_host().to_string(),
        bitcoind_rpc_port: bitcoind.rpc_port(),
        bitcoind_rpc_user: bitcoind.rpc_user().to_string(),
        bitcoind_rpc_password: bitcoind.rpc_password().to_string(),
        ldk_storage_dir: unique_storage_dir(prefix),
        ldk_listen_addr: None,
        node_alias: None,
        signer_transport: "embedded".to_string(),
        signer_relay: None,
        signer_nsec: None,
        signer_pubkey: None,
    };
    let ldk = LdkService::start_from_config(&cfg).expect("ldk service should start");
    let w = Wallet::new(ldk.clone(), None);
    (bitcoind, ldk, w)
}

/// A caller. The handler does not read it — authorization happened before
/// the request reached here, which is the whole point of the pipeline
/// being somewhere else.
fn caller(keys: &Keys) -> Caller<'_> {
    Caller { controller: &keys.public_key, request_id: None }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn get_info_returns_the_ldk_identity() {
    let (_b, ldk, w) = wallet("nwc-ldk-get-info").await;
    let k = Keys::generate();
    let info = w.get_info(GetInfoRequest {}, caller(&k)).await.expect("get_info");

    assert_eq!(info.network.as_deref(), Some("regtest"));
    assert_eq!(
        info.pubkey.as_deref(),
        Some(ldk.node_id().as_str()),
        "the pubkey is the node's, not the nostr key's"
    );
    assert!(
        info.methods.contains(&"pay_invoice".to_string()),
        "the method list is generated from the impl block"
    );
    assert!(
        !info.methods.contains(&"estimate_onchain_fees".to_string()),
        "and it must not contain a method this node does not implement — \
         which it did, returning NOT_IMPLEMENTED, until 25.3"
    );
    assert_eq!(
        info.notifications.as_ref().map(|n| n.len()),
        Some(3),
        "NWC-02's two and NWC-03's one"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn get_balance_reports_what_the_chain_paid_in() {
    let (b, ldk, w) = wallet("nwc-ldk-balance").await;
    let k = Keys::generate();

    let before = w.get_balance(GetBalanceRequest {}, caller(&k)).await.expect("balance");
    assert_eq!(before.balance, 0);

    let address = ldk.new_onchain_address().expect("ldk address generation should work");
    b.create_wallet("test").await;
    // Coinbase needs a hundred confirmations before it is spendable, which
    // is why this mines far more blocks than the payment needs.
    let miner = b.get_new_address().await;
    b.mine_blocks(101, &miner).await;
    b.send_to_address(&address, 1.0).await;
    b.mine_blocks(6, &miner).await;
    ldk.sync_wallets().expect("sync");

    let after = w.get_balance(GetBalanceRequest {}, caller(&k)).await.expect("balance");
    assert!(after.balance > 0, "the funding arrived");
    assert_eq!(
        after.onchain_balance_sats,
        Some(after.balance / 1_000),
        "on-chain funds only, and reported in **sats** — the field used to \
         be onchain_balance carrying msats"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn make_invoice_produces_one() {
    let (_b, _ldk, w) = wallet("nwc-ldk-make-invoice").await;
    let k = Keys::generate();
    let inv = w
        .make_invoice(
            MakeInvoiceRequest {
                amount: 123_000,
                description: Some("invoice from a handler".into()),
                description_hash: None,
                expiry: Some(3600),
                metadata: None,
            },
            caller(&k),
        )
        .await
        .expect("make_invoice");

    assert!(inv.invoice.is_some_and(|i| !i.is_empty()));
    assert_eq!(inv.amount, 123_000);
    assert!(inv.settled_at.is_none(), "nothing has paid it");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn make_invoice_refuses_a_description_hash() {
    let (_b, _ldk, w) = wallet("nwc-ldk-desc-hash").await;
    let k = Keys::generate();
    let e = w
        .make_invoice(
            MakeInvoiceRequest {
                amount: 123_000,
                description: None,
                description_hash: Some("ab".repeat(32)),
                expiry: None,
                metadata: None,
            },
            caller(&k),
        )
        .await
        .expect_err("LDK cannot bind a description hash");
    assert_eq!(e.code, ErrorCode::BadRequest);
    assert!(e.message.contains("description_hash"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn paying_zero_is_refused_before_the_node_is_asked() {
    let (_b, _ldk, w) = wallet("nwc-ldk-zero").await;
    let k = Keys::generate();

    let e = w
        .pay_invoice(
            PayInvoiceRequest { invoice: "lnbcrt1...".into(), amount: Some(0), metadata: None },
            caller(&k),
        )
        .await
        .expect_err("zero is not an amount");
    assert_eq!(e.code, ErrorCode::BadRequest);
    assert_eq!(e.message, "amount must be greater than 0");

    let e = w
        .pay_keysend(
            PayKeysendRequest {
                amount: 0,
                pubkey: "03".to_string() + &"ab".repeat(32),
                preimage: None,
                tlv_records: None,
            },
            caller(&k),
        )
        .await
        .expect_err("nor for keysend");
    assert_eq!(e.code, ErrorCode::BadRequest);
    assert_eq!(e.message, "amount must be greater than 0");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_invalid_invoice_fails_as_a_payment_failure() {
    let (_b, _ldk, w) = wallet("nwc-ldk-bad-invoice").await;
    let k = Keys::generate();
    let e = w
        .pay_invoice(
            PayInvoiceRequest { invoice: "not-an-invoice".into(), amount: None, metadata: None },
            caller(&k),
        )
        .await
        .expect_err("it is not an invoice");
    assert_eq!(e.code, ErrorCode::PaymentFailed);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_invalid_pubkey_fails_as_a_payment_failure() {
    let (_b, _ldk, w) = wallet("nwc-ldk-bad-pubkey").await;
    let k = Keys::generate();
    let e = w
        .pay_keysend(
            PayKeysendRequest {
                amount: 1_000,
                pubkey: "not-a-pubkey".into(),
                preimage: None,
                tlv_records: None,
            },
            caller(&k),
        )
        .await
        .expect_err("it is not a pubkey");
    assert_eq!(e.code, ErrorCode::PaymentFailed);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn list_invoices_shows_one_nobody_paid() {
    // The distinction `nwc-invoices.md` exists for: an unpaid invoice is
    // absent from `list_transactions`, because nothing happened, and
    // present here, because it was asked for.
    let (_b, _ldk, w) = wallet("nwc-ldk-list-invoices").await;
    let k = Keys::generate();
    w.make_invoice(
        MakeInvoiceRequest {
            amount: 55_000,
            description: Some("unpaid".into()),
            description_hash: None,
            expiry: Some(3600),
            metadata: None,
        },
        caller(&k),
    )
    .await
    .expect("make_invoice");

    let listed = w
        .list_invoices(ListInvoicesRequest::default(), caller(&k))
        .await
        .expect("list_invoices");
    assert_eq!(listed.invoices.len(), 1);
    assert_eq!(listed.invoices[0].state, InvoiceState::Pending);
    assert!(listed.invoices[0].preimage.is_none(), "unpaid, so no preimage");
}
