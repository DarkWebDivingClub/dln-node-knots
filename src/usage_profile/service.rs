use crate::state::rate_state::RateState;
use crate::state::store::{
    access_key_pubkey, clear_all_states, insert_access_rate_state, insert_quota_state,
    retain_access_rate_states, retain_quota_states, AccessKey,
};
use crate::usage_profile::store::{
    clear_usage_profiles as clear_usage_profiles_store, upsert_usage_profile,
};
use crate::UsageProfile;
use std::time::{SystemTime, UNIX_EPOCH};

fn key_belongs_to_pubkey(key: &AccessKey, pubkey: &str) -> bool {
    access_key_pubkey(key) == pubkey
}

fn clear_states_for_pubkey(pubkey: &str) {
    retain_access_rate_states(|key, _| !key_belongs_to_pubkey(key, pubkey));
    retain_quota_states(|key, _| !key_belongs_to_pubkey(key, pubkey));
}

pub(crate) fn clear_all_access_states() {
    clear_all_states();
}

fn initialize_states_for_profile(target_pubkey: &str, profile: &UsageProfile, now: u64) {
    if let Some(methods) = profile.methods.as_ref() {
        for (method, method_rule) in methods {
            if let Some(rule) = method_rule.access_rate.as_ref() {
                if let Ok(state) = RateState::from_rule(now, rule) {
                    insert_access_rate_state(
                        AccessKey::Method {
                            pubkey: target_pubkey.to_string(),
                            method: method.clone(),
                        },
                        state,
                    );
                }
            }
        }
    }

    if let Some(rule) = profile.quota.as_ref() {
        if let Ok(state) = RateState::from_rule(now, rule) {
            insert_quota_state(
                AccessKey::Quota {
                    pubkey: target_pubkey.to_string(),
                },
                state,
            );
        }
    }
}

fn now_micros() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_micros()
        .min(u128::from(u64::MAX)) as u64
}

/// Upserts a profile and resets in-memory counters for that pubkey.
///
/// Existing access/quota states for `target_pubkey` are removed first, then re-created from the
/// incoming profile's configured rules. Counter values are reset as part of this process.
pub(crate) fn upsert_usage_profile_and_reset_states(target_pubkey: &str, profile: UsageProfile) {
    clear_states_for_pubkey(target_pubkey);
    initialize_states_for_profile(target_pubkey, &profile, now_micros());
    upsert_usage_profile(target_pubkey, profile);
}

/// Clears all usage profiles and all in-memory access/quota states.
pub(crate) fn clear_all_usage_profiles_and_states() {
    clear_usage_profiles_store();
    clear_all_access_states();
}
