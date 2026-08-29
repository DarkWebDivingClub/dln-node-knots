use crate::usage_profile::UsageProfile;
use std::collections::HashMap;
use std::sync::{OnceLock, RwLock};

static USAGE_PROFILES: OnceLock<RwLock<HashMap<String, UsageProfile>>> = OnceLock::new();

/// Returns the shared in-memory usage-profile store.
///
/// The map is initialized on first use and guarded by an `RwLock`.
///
/// # Returns
/// A process-global `RwLock<HashMap<String, UsageProfile>>`.
fn usage_profiles() -> &'static RwLock<HashMap<String, UsageProfile>> {
    USAGE_PROFILES.get_or_init(|| RwLock::new(HashMap::new()))
}

/// Looks up and clones the usage profile for `pubkey`.
///
/// # Arguments
/// * `pubkey` - Caller pubkey string used as profile key.
///
/// # Returns
/// `Some(UsageProfile)` when a profile exists, otherwise `None`.
///
/// # Panics
/// Panics if the usage profile lock is poisoned.
pub fn get_usage_profile(pubkey: &str) -> Option<UsageProfile> {
    let map = usage_profiles()
        .read()
        .expect("usage profile map lock poisoned");
    map.get(pubkey).cloned()
}

/// Clears all stored usage profiles.
///
/// # Panics
/// Panics if the usage profile lock is poisoned.
pub fn clear_usage_profiles() {
    let mut map = usage_profiles()
        .write()
        .expect("usage profile map lock poisoned");
    map.clear();
}

/// Inserts or replaces the usage profile for `target_pubkey`.
///
/// # Arguments
/// * `target_pubkey` - Pubkey that owns the profile.
/// * `profile` - New profile value to upsert.
///
/// # Panics
/// Panics if the usage profile lock is poisoned.
pub(crate) fn upsert_usage_profile(target_pubkey: &str, profile: UsageProfile) {
    let mut map = usage_profiles()
        .write()
        .expect("usage profile map lock poisoned");
    map.insert(target_pubkey.to_string(), profile);
}
