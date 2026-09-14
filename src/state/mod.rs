//! What the node remembers that LDK does not.
//!
//! Offers, addresses and forwarding history. **Not** grants, rate buckets
//! or subscriptions — those were here until mission 25.3 and are the
//! crate's now.

pub mod address_store;
pub mod forwarding_store;
pub mod offer_store;
