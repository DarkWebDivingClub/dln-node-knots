#[path = "../common/mod.rs"]
pub mod common;

mod get_balance_after_onchain_funding;
mod get_info_returns_ldk_identity;
mod make_invoice_happy_path;
mod make_invoice_invalid_description_hash_returns_error;
mod pay_invoice_invalid_invoice_returns_error;
mod pay_invoice_zero_amount_returns_error;
mod pay_keysend_invalid_pubkey_returns_error;
mod pay_keysend_zero_amount_returns_error;
mod list_invoices_after_payment;

use nostr_sdk::prelude::{Keys, PublicKey};
use std::sync::OnceLock;

pub fn shared_relay_pubkey() -> PublicKey {
    static RELAY_PUBKEY: OnceLock<PublicKey> = OnceLock::new();
    RELAY_PUBKEY
        .get_or_init(|| Keys::generate().public_key())
        .clone()
}
