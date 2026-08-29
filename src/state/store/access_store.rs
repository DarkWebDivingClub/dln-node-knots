use crate::state::rate_state::RateState;
use nwc::nostr::nips::nip47::Method;
use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock, RwLock};

#[derive(Debug, Eq, PartialEq, Hash, Clone)]
pub(crate) enum AccessKey {
    Method { pubkey: String, method: Method },
    Quota { pubkey: String },
}

pub(crate) struct AccessState {
    pub(crate) access_rate: RwLock<HashMap<AccessKey, Arc<Mutex<RateState>>>>, // per-method access rate
    pub(crate) quota: RwLock<HashMap<AccessKey, Arc<Mutex<RateState>>>>, // per-user quota rate
}

pub(crate) type SharedRateState = Arc<Mutex<RateState>>;

static ACCESS_STATE: OnceLock<AccessState> = OnceLock::new();

/// Returns the shared access-state store, initializing empty maps on first use.
///
/// The returned store contains independent maps for:
/// - per-method access-rate counters (`access_rate`)
/// - per-user quota counters (`quota`)
///
/// # Returns
/// A process-global `AccessState` containing the two guarded maps.
pub(crate) fn access_state() -> &'static AccessState {
    ACCESS_STATE.get_or_init(|| AccessState {
        access_rate: RwLock::new(HashMap::new()),
        quota: RwLock::new(HashMap::new()),
    })
}

pub(crate) fn access_key_pubkey(key: &AccessKey) -> &str {
    match key {
        AccessKey::Method { pubkey, .. } => pubkey,
        AccessKey::Quota { pubkey } => pubkey,
    }
}

pub(crate) fn get_access_rate_state(key: &AccessKey) -> Option<SharedRateState> {
    access_state()
        .access_rate
        .read()
        .expect("access_rate map lock poisoned")
        .get(key)
        .cloned()
}

pub(crate) fn get_quota_state(key: &AccessKey) -> Option<SharedRateState> {
    access_state()
        .quota
        .read()
        .expect("quota map lock poisoned")
        .get(key)
        .cloned()
}

pub(crate) fn insert_access_rate_state(key: AccessKey, state: RateState) -> SharedRateState {
    let handle = Arc::new(Mutex::new(state));
    access_state()
        .access_rate
        .write()
        .expect("access_rate map lock poisoned")
        .insert(key, handle.clone());
    handle
}

pub(crate) fn insert_quota_state(key: AccessKey, state: RateState) -> SharedRateState {
    let handle = Arc::new(Mutex::new(state));
    access_state()
        .quota
        .write()
        .expect("quota map lock poisoned")
        .insert(key, handle.clone());
    handle
}

pub(crate) fn retain_access_rate_states<F>(mut keep: F)
where
    F: FnMut(&AccessKey, &SharedRateState) -> bool,
{
    access_state()
        .access_rate
        .write()
        .expect("access_rate map lock poisoned")
        .retain(|k, v| keep(k, v));
}

pub(crate) fn retain_quota_states<F>(mut keep: F)
where
    F: FnMut(&AccessKey, &SharedRateState) -> bool,
{
    access_state()
        .quota
        .write()
        .expect("quota map lock poisoned")
        .retain(|k, v| keep(k, v));
}

pub(crate) fn clear_access_rate_states() {
    access_state()
        .access_rate
        .write()
        .expect("access_rate map lock poisoned")
        .clear();
}

pub(crate) fn clear_quota_states() {
    access_state()
        .quota
        .write()
        .expect("quota map lock poisoned")
        .clear();
}

pub(crate) fn clear_all_states() {
    clear_access_rate_states();
    clear_quota_states();
}
