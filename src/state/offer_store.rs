use std::collections::HashMap;
use std::sync::{OnceLock, RwLock};

#[derive(Debug, Clone)]
pub struct OfferRecord {
    pub offer: String,
    pub description: String,
    pub amount_msat: u64,
    pub active: bool,
    pub num_payments_received: u64,
    pub total_received_msat: u64,
    pub offer_id: [u8; 32],
}

static OFFER_STORE: OnceLock<RwLock<HashMap<String, OfferRecord>>> = OnceLock::new();

fn store() -> &'static RwLock<HashMap<String, OfferRecord>> {
    OFFER_STORE.get_or_init(|| RwLock::new(HashMap::new()))
}

pub fn insert_offer(offer: String, description: String, amount_msat: u64, offer_id: [u8; 32]) {
    let mut map = store().write().expect("offer store lock poisoned");
    map.insert(
        offer.clone(),
        OfferRecord {
            offer,
            description,
            amount_msat,
            active: true,
            num_payments_received: 0,
            total_received_msat: 0,
            offer_id,
        },
    );
}

pub fn find_by_offer_id(offer_id: &[u8; 32]) -> Option<String> {
    let map = store().read().expect("offer store lock poisoned");
    map.iter()
        .find(|(_, record)| &record.offer_id == offer_id)
        .map(|(key, _)| key.clone())
}

pub fn get_offer(offer: &str) -> Option<OfferRecord> {
    let map = store().read().expect("offer store lock poisoned");
    map.get(offer).cloned()
}

pub fn list_offers() -> Vec<OfferRecord> {
    let map = store().read().expect("offer store lock poisoned");
    map.values().cloned().collect()
}

pub fn disable_offer(offer: &str) -> bool {
    let mut map = store().write().expect("offer store lock poisoned");
    if let Some(record) = map.get_mut(offer) {
        record.active = false;
        true
    } else {
        false
    }
}

pub fn record_payment(offer: &str, amount_msat: u64) {
    let mut map = store().write().expect("offer store lock poisoned");
    if let Some(record) = map.get_mut(offer) {
        record.num_payments_received += 1;
        record.total_received_msat += amount_msat;
    }
}

#[allow(dead_code)]
pub fn clear() {
    let mut map = store().write().expect("offer store lock poisoned");
    map.clear();
}
