//! `dln-node`: a Lightning node that speaks NWC and NIP-XX.
//!
//! **This crate implements handlers.** It has no relay code, no event
//! kind, no NIP-44 call, no grant parsing and no rate bucket, because
//! [`nostr_ln`] owns every one of those — once, for this node and for
//! every other consumer at the same time.
//!
//! | | |
//! |---|---|
//! | [`wallet`] | twenty-three NWC methods |
//! | [`control`] | fourteen NIP-XX methods |
//! | [`events`] | LDK events, as notifications with a cause |
//! | [`service`] | starting both, and the event pump |
//! | [`lightning`] | the node itself |
//!
//! ## What was deleted, and what went with it
//!
//! About fifteen hundred lines of protocol: a request pipeline, grant
//! parsing, usage profiles, rate and quota buckets, encryption helpers,
//! event-kind constants, notification publishers and a relay loop. Five
//! open issues lived in that code and none of them was migrated:
//!
//! | | |
//! |---|---|
//! | [#1](https://github.com/DarkWebDivingClub/dln-node/issues/1) | grants applied without checking who signed them |
//! | [#2](https://github.com/DarkWebDivingClub/dln-node/issues/2) | `access_rate` where the specification says `rate` |
//! | [#3](https://github.com/DarkWebDivingClub/dln-node/issues/3) | NIP-04 where the specification says NIP-44 |
//! | [#4](https://github.com/DarkWebDivingClub/dln-node/issues/4) | buckets refilling from grant application |
//! | [#5](https://github.com/DarkWebDivingClub/dln-node/issues/5) | `rate_per_micro`, so refill never happened |
//!
//! That is what a shared implementation is for: these questions are
//! answered somewhere else, and a handler cannot get them wrong because it
//! cannot reach them.

pub mod control;
pub mod events;
pub mod lightning;
pub mod service;
mod state;
pub mod wallet;

use crate::lightning::PaymentKind;


pub(crate) fn payment_hash_from_kind(kind: &PaymentKind) -> Option<String> {
    match kind {
        PaymentKind::Bolt11 { hash, .. } => Some(hex_payment_hash(&hash.0)),
        PaymentKind::Bolt11Jit { hash, .. } => Some(hex_payment_hash(&hash.0)),
        PaymentKind::Spontaneous { hash, .. } => Some(hex_payment_hash(&hash.0)),
        PaymentKind::Bolt12Offer { hash, .. } => hash.as_ref().map(|h| hex_payment_hash(&h.0)),
        PaymentKind::Bolt12Refund { hash, .. } => hash.as_ref().map(|h| hex_payment_hash(&h.0)),
        _ => None,
    }
}


pub(crate) fn preimage_from_kind(kind: &PaymentKind) -> Option<String> {
    match kind {
        PaymentKind::Bolt11 { preimage, .. } => preimage.as_ref().map(|p| hex_payment_hash(&p.0)),
        PaymentKind::Bolt11Jit { preimage, .. } => preimage.as_ref().map(|p| hex_payment_hash(&p.0)),
        PaymentKind::Spontaneous { preimage, .. } => preimage.as_ref().map(|p| hex_payment_hash(&p.0)),
        PaymentKind::Bolt12Offer { preimage, .. } => preimage.as_ref().map(|p| hex_payment_hash(&p.0)),
        PaymentKind::Bolt12Refund { preimage, .. } => preimage.as_ref().map(|p| hex_payment_hash(&p.0)),
        _ => None,
    }
}


pub(crate) fn hex_payment_hash(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        use std::fmt::Write as _;
        let _ = write!(&mut out, "{:02x}", b);
    }
    out
}
