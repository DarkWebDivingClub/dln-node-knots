use std::collections::{HashMap, HashSet};
use std::sync::{OnceLock, RwLock};

/// Global subscription state: maps client_pubkey → set of notification types.
static SUBSCRIPTIONS: OnceLock<RwLock<HashMap<String, HashSet<String>>>> = OnceLock::new();

fn store() -> &'static RwLock<HashMap<String, HashSet<String>>> {
    SUBSCRIPTIONS.get_or_init(|| RwLock::new(HashMap::new()))
}

/// Subscribe a client to the given notification types.
/// Replaces any existing subscription for that client.
pub fn subscribe(client_pubkey: &str, types: &[String]) {
    let mut map = store().write().expect("subscription store lock poisoned");
    let set: HashSet<String> = types.iter().cloned().collect();
    map.insert(client_pubkey.to_string(), set);
}

/// Remove all subscriptions for a client.
pub fn unsubscribe(client_pubkey: &str) {
    let mut map = store().write().expect("subscription store lock poisoned");
    map.remove(client_pubkey);
}

/// Get all client pubkeys subscribed to a given notification type.
pub fn get_subscribers(notification_type: &str) -> Vec<String> {
    let map = store().read().expect("subscription store lock poisoned");
    map.iter()
        .filter(|(_, types)| types.contains(notification_type))
        .map(|(pubkey, _)| pubkey.clone())
        .collect()
}

/// Check if a client is subscribed to a given notification type.
pub fn is_subscribed(client_pubkey: &str, notification_type: &str) -> bool {
    let map = store().read().expect("subscription store lock poisoned");
    map.get(client_pubkey)
        .map(|types| types.contains(notification_type))
        .unwrap_or(false)
}

/// Clear all subscriptions (for testing).
pub fn clear() {
    let mut map = store().write().expect("subscription store lock poisoned");
    map.clear();
}
