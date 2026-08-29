use serde::Serialize;
use std::sync::{OnceLock, RwLock};

#[derive(Debug, Clone, Serialize)]
pub struct ForwardingEntry {
    pub incoming_channel_id: String,
    pub outgoing_channel_id: String,
    pub incoming_amount: u64,
    pub outgoing_amount: u64,
    pub fee_earned: u64,
    pub settled_at: u64,
}

static FORWARDING_HISTORY: OnceLock<RwLock<Vec<ForwardingEntry>>> = OnceLock::new();

fn store() -> &'static RwLock<Vec<ForwardingEntry>> {
    FORWARDING_HISTORY.get_or_init(|| RwLock::new(Vec::new()))
}

pub fn record_forward(entry: ForwardingEntry) {
    let mut vec = store().write().expect("forwarding store lock poisoned");
    vec.push(entry);
}

pub fn get_history(
    from: Option<u64>,
    until: Option<u64>,
    limit: usize,
    offset: usize,
) -> Vec<ForwardingEntry> {
    let vec = store().read().expect("forwarding store lock poisoned");
    vec.iter()
        .filter(|e| {
            if let Some(from_ts) = from {
                if e.settled_at < from_ts {
                    return false;
                }
            }
            if let Some(until_ts) = until {
                if e.settled_at > until_ts {
                    return false;
                }
            }
            true
        })
        .skip(offset)
        .take(limit)
        .cloned()
        .collect()
}

#[allow(dead_code)]
pub fn clear() {
    let mut vec = store().write().expect("forwarding store lock poisoned");
    vec.clear();
}
