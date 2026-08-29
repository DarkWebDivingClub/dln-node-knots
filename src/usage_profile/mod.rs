use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use crate::RateLimitRule;
use nwc::nostr::nips::nip47::Method;

pub(crate) mod service;
pub(crate) mod store;
pub(crate) use service::{
    clear_all_usage_profiles_and_states, upsert_usage_profile_and_reset_states,
};
pub use store::{clear_usage_profiles, get_usage_profile};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MethodAccessRule {
    pub access_rate: Option<RateLimitRule>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UsageProfile {
    pub quota: Option<RateLimitRule>,
    pub methods: Option<HashMap<Method, MethodAccessRule>>,
    pub control: Option<HashMap<String, MethodAccessRule>>,
}
