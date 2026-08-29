#[path = "../common/mod.rs"]
pub mod common;

pub mod usage_profile_service;

use nostr_sdk::prelude::{Keys, PublicKey};
use std::sync::OnceLock;

pub fn shared_relay_pubkey() -> PublicKey {
    static RELAY_PUBKEY: OnceLock<PublicKey> = OnceLock::new();
    RELAY_PUBKEY
        .get_or_init(|| Keys::generate().public_key())
        .clone()
}
