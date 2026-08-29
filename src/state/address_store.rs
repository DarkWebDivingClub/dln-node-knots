use std::collections::HashMap;
use std::sync::{OnceLock, RwLock};

#[derive(Debug, Clone)]
pub struct AddressTransaction {
    pub txid: String,
    pub amount_sats: u64,
    pub timestamp: u64,
}

#[derive(Debug, Clone)]
pub struct AddressRecord {
    pub address: String,
    pub transactions: Vec<AddressTransaction>,
}

static ADDRESS_STORE: OnceLock<RwLock<HashMap<String, AddressRecord>>> = OnceLock::new();

fn store() -> &'static RwLock<HashMap<String, AddressRecord>> {
    ADDRESS_STORE.get_or_init(|| RwLock::new(HashMap::new()))
}

pub fn register_address(address: String) {
    let mut map = store().write().expect("address store lock poisoned");
    map.entry(address.clone()).or_insert_with(|| AddressRecord {
        address,
        transactions: Vec::new(),
    });
}

pub fn get_address(address: &str) -> Option<AddressRecord> {
    let map = store().read().expect("address store lock poisoned");
    map.get(address).cloned()
}

pub fn list_addresses() -> Vec<AddressRecord> {
    let map = store().read().expect("address store lock poisoned");
    map.values().cloned().collect()
}

#[allow(dead_code)]
pub fn record_transaction(address: &str, txid: String, amount_sats: u64, timestamp: u64) {
    let mut map = store().write().expect("address store lock poisoned");
    if let Some(record) = map.get_mut(address) {
        record.transactions.push(AddressTransaction {
            txid,
            amount_sats,
            timestamp,
        });
    }
}

#[allow(dead_code)]
pub fn clear() {
    let mut map = store().write().expect("address store lock poisoned");
    map.clear();
}
