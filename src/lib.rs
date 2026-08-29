use crate::state::rate_state::RateState;
use crate::state::store::{get_access_rate_state, get_quota_state, AccessKey};
use nostr_sdk::prelude::*;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use nwc::nostr::nips::nip04;
use nwc::nostr::nips::nip44;
use nwc::nostr::nips::nip47::{
    CancelHoldInvoiceResponse, ErrorCode,
    EstimateRoutingFeesResponse, GetBalanceResponse, GetInfoResponse,
    LookupInvoiceResponse, MakeHoldInvoiceResponse, MakeInvoiceResponse, MakeNewAddressResponse,
    MakeOfferResponse, Method, NIP47Error, Notification, NotificationResult, NotificationType,
    MakeBip321Response, Bip321MethodInfo, Bip321MethodEntry,
    PayBip321Response, PayInvoiceResponse, PayKeysendResponse, PaymentNotification,
    PayOfferResponse, PayOnchainResponse, Request, RequestParams, Response, ResponseResult,
    SettleHoldInvoiceResponse, SubscribeNotificationsResponse, TransactionState, TransactionType,
    LookupOfferResponse, LookupAddressResponse, AddressTransaction,
    ListInvoicesResponse, InvoiceEntry,
    ListOffersResponse, OfferEntry,
    DisableOfferResponse,
    ListAddressesResponse, AddressEntry,
    NncNotification, NncNotificationType, NncNotificationResult,
    ChannelOpenedNotification, ChannelClosedNotification, PaymentMethod,
};
use std::cmp::Ordering;
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, MutexGuard, OnceLock, RwLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use crate::lightning::{LdkBalance, LdkService, LdkServiceError, PaymentDetails, PaymentDirection, PaymentKind, PaymentStatus};
use ldk_node::lightning::offers::offer::Offer;
use std::str::FromStr;
pub mod rate_limit_rule;
pub mod lightning;
mod state;
pub mod usage_profile;

use crate::state::store::access_store::SharedRateState;
use crate::state::{offer_store, address_store, subscription_store, forwarding_store};
pub use rate_limit_rule::RateLimitRule;
pub use state::rate_state::RateStateError;
pub use usage_profile::get_usage_profile;
pub use usage_profile::{MethodAccessRule, UsageProfile};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AccessErrorContext {
    AccessRate,
    Quota,
}

static GLOBAL_KEYS: OnceLock<Keys> = OnceLock::new();
static RELAY_PUBKEY: OnceLock<PublicKey> = OnceLock::new();
static OWNERS: OnceLock<RwLock<Vec<String>>> = OnceLock::new();
static APPLIED_GRANT_EVENTS: OnceLock<RwLock<HashMap<String, AppliedGrantEvent>>> = OnceLock::new();
static FORCED_EXECUTE_FAILURES: OnceLock<RwLock<HashSet<Method>>> = OnceLock::new();
static LDK_SERVICE: OnceLock<RwLock<Option<Arc<LdkService>>>> = OnceLock::new();
static NODE_ALIAS: OnceLock<Option<String>> = OnceLock::new();

pub const CONTROL_REQUEST_KIND: u16 = 23198;
pub const CONTROL_RESPONSE_KIND: u16 = 23199;

#[derive(Clone)]
struct AppliedGrantEvent {
    created_at: u64,
    event_id: String,
}

fn set_global_keys(keys: &Keys) {
    let _ = GLOBAL_KEYS.set(keys.clone());
}

fn set_ldk_service(ldk_service: Arc<LdkService>) {
    let lock = LDK_SERVICE.get_or_init(|| RwLock::new(None));
    let mut guard = lock.write().expect("ldk service lock poisoned");
    *guard = Some(ldk_service);
}

fn get_ldk_service() -> Option<Arc<LdkService>> {
    let lock = LDK_SERVICE.get_or_init(|| RwLock::new(None));
    let guard = lock.read().expect("ldk service lock poisoned");
    guard.clone()
}

pub fn set_relay_pubkey(pubkey: PublicKey) {
    let _ = RELAY_PUBKEY.set(pubkey);
}

pub fn set_owners(owners: Vec<String>) {
    let lock = OWNERS.get_or_init(|| RwLock::new(Vec::new()));
    let mut guard = lock.write().expect("owners lock poisoned");
    *guard = owners;
}

fn parse_grant_target(event: &Event) -> Option<String> {
    let d_tag = event.tags.iter().find_map(|tag| {
        let parts = tag.as_slice();
        if parts.get(0).map(|v| v.as_str()) == Some("d") {
            parts.get(1).cloned()
        } else {
            None
        }
    })?;
    let mut parts = d_tag.splitn(2, ':');
    let node_pubkey = parts.next()?;
    if let Some(configured_node_pubkey) = RELAY_PUBKEY.get() {
        if node_pubkey != configured_node_pubkey.to_string() {
            return None;
        }
    }
    let user_pubkey = parts.next()?;
    if user_pubkey.is_empty() {
        None
    } else {
        Some(user_pubkey.to_string())
    }
}

fn should_apply_grant_event(target_pubkey: &str, event: &Event) -> bool {
    let created_at = event.created_at.as_secs();
    let event_id = event.id.to_string();

    let mut map = APPLIED_GRANT_EVENTS
        .get_or_init(|| RwLock::new(HashMap::new()))
        .write()
        .expect("applied grant event map lock poisoned");

    match map.get(target_pubkey) {
        None => {
            map.insert(
                target_pubkey.to_string(),
                AppliedGrantEvent {
                    created_at,
                    event_id,
                },
            );
            true
        }
        Some(existing) => {
            let should_apply = created_at > existing.created_at
                || (created_at == existing.created_at && event_id > existing.event_id);
            if should_apply {
                map.insert(
                    target_pubkey.to_string(),
                    AppliedGrantEvent {
                        created_at,
                        event_id,
                    },
                );
            }
            should_apply
        }
    }
}

pub fn clear_usage_profiles() {
    usage_profile::clear_all_usage_profiles_and_states();
}

pub fn clear_subscriptions() {
    subscription_store::clear();
}

#[doc(hidden)]
pub fn seed_offer_for_testing(offer: String, description: String, amount_msat: u64) {
    let offer_id = Offer::from_str(&offer)
        .map(|o| o.id().0)
        .unwrap_or([0u8; 32]);
    offer_store::insert_offer(offer, description, amount_msat, offer_id);
}

#[doc(hidden)]
pub fn seed_address_for_testing(address: String) {
    address_store::register_address(address);
}

#[doc(hidden)]
pub fn seed_address_transaction_for_testing(address: &str, txid: String, amount_sats: u64, timestamp: u64) {
    address_store::record_transaction(address, txid, amount_sats, timestamp);
}

#[doc(hidden)]
pub fn clear_offers_for_testing() {
    offer_store::clear();
}

#[doc(hidden)]
pub fn clear_addresses_for_testing() {
    address_store::clear();
}

#[doc(hidden)]
pub fn clear_access_states_for_testing() {
    usage_profile::service::clear_all_access_states();
}

#[doc(hidden)]
pub fn set_execute_failure_for_testing(method: Method, enabled: bool) {
    let lock = FORCED_EXECUTE_FAILURES.get_or_init(|| RwLock::new(HashSet::new()));
    let mut guard = lock
        .write()
        .expect("forced execute failures lock poisoned");
    if enabled {
        guard.insert(method);
    } else {
        guard.remove(&method);
    }
}

#[doc(hidden)]
pub fn clear_execute_failures_for_testing() {
    let lock = FORCED_EXECUTE_FAILURES.get_or_init(|| RwLock::new(HashSet::new()));
    let mut guard = lock
        .write()
        .expect("forced execute failures lock poisoned");
    guard.clear();
}

fn should_force_execute_failure(method: &Method) -> bool {
    let lock = FORCED_EXECUTE_FAILURES.get_or_init(|| RwLock::new(HashSet::new()));
    let guard = lock.read().expect("forced execute failures lock poisoned");
    guard.contains(method)
}

struct StateUpdateRequest {
    key: AccessKey,
    rule: RateLimitRule,
    amount: u64,
    context: AccessErrorContext,
    state: SharedRateState,
}

struct GuardedStateUpdate<'a> {
    update: &'a StateUpdateRequest,
    guard: MutexGuard<'a, RateState>,
}

fn compare_access_keys(a: &AccessKey, b: &AccessKey) -> Ordering {
    match (a, b) {
        (
            AccessKey::Method {
                pubkey: ap,
                method: am,
            },
            AccessKey::Method {
                pubkey: bp,
                method: bm,
            },
        ) => ap.cmp(bp).then_with(|| am.as_str().cmp(bm.as_str())),
        (AccessKey::Method { .. }, AccessKey::Quota { .. }) => Ordering::Less,
        (AccessKey::Quota { .. }, AccessKey::Method { .. }) => Ordering::Greater,
        (AccessKey::Quota { pubkey: ap }, AccessKey::Quota { pubkey: bp }) => ap.cmp(bp),
    }
}

fn verify_access(request: &Request, event: &Event) -> Result<Vec<StateUpdateRequest>, Response> {
    let caller_pubkey = event.pubkey.to_string();

    // Require a UsageProfile grant for the caller.
    // TODO: Cant these to be combined
    let profile = get_usage_profile(&caller_pubkey);
    let profile = profile.ok_or_else(|| unauthorized_response(&request.method))?;

    // Method authorization: missing methods means no restriction.
    let method_rule = match &profile.methods {
        None => None,
        Some(methods) => {
            if methods.is_empty() {
                return Err(access_denied_response(&request.method));
            }
            match methods.get(&request.method) {
                Some(rule) => Some(rule.clone()),
                None => {
                    // Check for "ALL" special key
                    match methods.get(&Method::Unknown("ALL".to_string())) {
                        Some(rule) => Some(rule.clone()),
                        None => return Err(access_denied_response(&request.method)),
                    }
                }
            }
        }
    };

    let now = now_micros();

    let mut state_updates: Vec<StateUpdateRequest> = Vec::new();
    let mut guarded_updates: Vec<GuardedStateUpdate> = Vec::new();

    // Prepare rate limit state updates, but only apply if all checks pass.
    if let Some(rule) = method_rule.as_ref().and_then(|r| r.access_rate.as_ref()) {
        let amount = 1_000_000;

        let key = AccessKey::Method {
            pubkey: caller_pubkey.clone(),
            method: request.method.clone(),
        };

        let state = get_access_rate_state(&key).ok_or_else(|| {
            missing_state_response(&request.method, AccessErrorContext::AccessRate)
        })?;

        let state_update = StateUpdateRequest {
            key,
            rule: rule.clone(),
            amount,
            context: AccessErrorContext::AccessRate,
            state,
        };

        state_updates.push(state_update);
    }

    // Prepare quota updates. Only apply if the request spends msats.
    let amount_msat = request_spend_msat(request).unwrap_or(0);
    if amount_msat > 0 {
        if let Some(rule) = profile.quota.as_ref() {
            let key = AccessKey::Quota {
                pubkey: caller_pubkey.clone(),
            };

            let state = get_quota_state(&key).ok_or_else(|| {
                missing_state_response(&request.method, AccessErrorContext::Quota)
            })?;

            let state_update = StateUpdateRequest {
                key,
                rule: rule.clone(),
                amount: amount_msat,
                context: AccessErrorContext::Quota,
                state,
            };

            state_updates.push(state_update);
        }
    }

    state_updates.sort_by(|a, b| compare_access_keys(&a.key, &b.key));

    // Lock
    for state_update in &state_updates {
        let guard = state_update
            .state
            .lock()
            .map_err(|_| poisoned_state_response(&request.method, state_update.context))?;
        guarded_updates.push(GuardedStateUpdate {
            update: state_update,
            guard,
        });
    }

    // Verify
    for guarded_update in &guarded_updates {
        let state_update = guarded_update.update;
        guarded_update
            .guard
            .check_withdraw_after_refill(state_update.amount, now, &state_update.rule)
            .map_err(|e| rate_state_error_response(&request.method, &e, state_update.context))?;
    }

    // Debit
    for guarded_update in &mut guarded_updates {
        let state_update = guarded_update.update;
        guarded_update
            .guard
            .withdraw_after_refill(state_update.amount, now, &state_update.rule)
            .map_err(|e| rate_state_error_response(&request.method, &e, state_update.context))?;
    }

    drop(guarded_updates);

    Ok(state_updates)
}

fn refund_applied_state_updates(method: &Method, updates: &[StateUpdateRequest]) {
    for update in updates {
        let mut guard = match update.state.lock() {
            Ok(guard) => guard,
            Err(_) => {
                eprintln!(
                    "Failed to refund {:?}: state lock poisoned ({:?})",
                    method, update.context
                );
                continue;
            }
        };

        if let Err(error) = guard.refund(update.amount, &update.rule) {
            let mapped = map_rate_state_error(&error, update.context);
            eprintln!(
                "Failed to refund {:?}: {} ({:?})",
                method, mapped.message, mapped.code
            );
        }
    }
}

fn access_denied_response(method: &Method) -> Response {
    Response {
        result_type: method.clone(),
        error: Some(NIP47Error {
            code: ErrorCode::Restricted,
            message: "access denied, insufficient permission".to_string(),
        }),
        result: None,
    }
}

fn unauthorized_response(method: &Method) -> Response {
    Response {
        result_type: method.clone(),
        error: Some(NIP47Error {
            code: ErrorCode::Unauthorized,
            message: "unauthorized".to_string(),
        }),
        result: None,
    }
}

fn rate_limited_response(method: &Method) -> Response {
    Response {
        result_type: method.clone(),
        error: Some(NIP47Error {
            code: ErrorCode::RateLimited,
            message: "rate limit exceeded".to_string(),
        }),
        result: None,
    }
}

fn quota_exceeded_response(method: &Method) -> Response {
    Response {
        result_type: method.clone(),
        error: Some(NIP47Error {
            code: ErrorCode::QuotaExceeded,
            message: "quota exceeded".to_string(),
        }),
        result: None,
    }
}

fn map_ldk_service_error(
    operation: &'static str,
    code: ErrorCode,
    error: LdkServiceError,
) -> NIP47Error {
    NIP47Error {
        code,
        message: format!("ldk {operation} failed: {error}"),
    }
}

pub fn map_rate_state_error(error: &RateStateError, context: AccessErrorContext) -> NIP47Error {
    match error {
        RateStateError::InsufficientBalance => {
            let (code, message) = match context {
                AccessErrorContext::AccessRate => (ErrorCode::RateLimited, "rate limit exceeded"),
                AccessErrorContext::Quota => (ErrorCode::QuotaExceeded, "quota exceeded"),
            };
            NIP47Error {
                code,
                message: message.to_string(),
            }
        }
        RateStateError::AmountTooLarge { .. } => NIP47Error {
            code: ErrorCode::Other,
            message: "invalid amount: exceeds i64::MAX".to_string(),
        },
        RateStateError::InvalidRule { .. } => NIP47Error {
            code: ErrorCode::Other,
            message: "invalid rate limit rule".to_string(),
        },
        RateStateError::InternalInvariantViolation => NIP47Error {
            code: ErrorCode::Other,
            message: "internal rate state error".to_string(),
        },
    }
}

fn rate_state_error_response(
    method: &Method,
    error: &RateStateError,
    context: AccessErrorContext,
) -> Response {
    Response {
        result_type: method.clone(),
        error: Some(map_rate_state_error(error, context)),
        result: None,
    }
}

fn missing_state_response(method: &Method, context: AccessErrorContext) -> Response {
    let message = match context {
        AccessErrorContext::AccessRate => "missing access rate state",
        AccessErrorContext::Quota => "missing quota state",
    };
    Response {
        result_type: method.clone(),
        error: Some(NIP47Error {
            code: ErrorCode::Other,
            message: message.to_string(),
        }),
        result: None,
    }
}

fn poisoned_state_response(method: &Method, context: AccessErrorContext) -> Response {
    let message = match context {
        AccessErrorContext::AccessRate => "access rate state lock poisoned",
        AccessErrorContext::Quota => "quota state lock poisoned",
    };
    Response {
        result_type: method.clone(),
        error: Some(NIP47Error {
            code: ErrorCode::Other,
            message: message.to_string(),
        }),
        result: None,
    }
}

fn request_spend_msat(request: &Request) -> Option<u64> {
    match &request.params {
        RequestParams::PayInvoice(params) => params.amount,
        RequestParams::PayKeysend(params) => Some(params.amount),
        RequestParams::PayOnchain(params) => Some(params.amount.saturating_mul(1000)),
        RequestParams::PayOffer(params) => params.amount,
        RequestParams::PayBip321(params) => {
            bip321::Uri::parse(&params.uri)
                .ok()
                .and_then(|uri| uri.amount)
                .map(|a| a.to_sat() * 1000)
        }
        _ => None,
    }
}

fn now_micros() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_micros()
        .min(u128::from(u64::MAX)) as u64
}

/// Connects to a nostr relay, subscribes to text notes, and responds
/// "Hi" to any message containing "hello".
///
/// The `client` is returned by reference so the caller (main or tests)
/// retains access to it for shutdown or further interaction.
pub async fn run_client(keys: Keys, relay_url: &str) -> Result<Client> {
    set_global_keys(&keys);
    let client = Client::builder().signer(keys).build();
    client.add_relay(relay_url).await?;
    println!("Connecting to relay {}...", relay_url);
    client.connect().await;
    println!("Connected!");

    let filter = Filter::new().kind(Kind::TextNote);
    client.subscribe(filter).await?;
    println!("Subscribed to text notes. Listening for events...\n");

    // Clone the client so we can use it inside the notification handler
    // to publish responses. The original client is returned to the caller.
    let client_clone = client.clone();

    // Spawn the notification loop in a background task so this function
    // returns immediately. The caller can keep using the client while
    // events are being handled in the background.
    tokio::spawn(async move {
        let mut notifications = client_clone.notifications();
        while let Some(notification) = notifications.next().await {
            if let ClientNotification::Event { event, .. } = notification {
                let event = event.as_ref();
                println!(
                    "--- Event from {} ---",
                    event.pubkey.to_bech32().unwrap_or_default()
                );
                println!("{}", event.content);
                println!();

                // If the message contains "hello" (case-insensitive),
                // respond with "Hi"
                if event.content.to_lowercase().contains("hello") {
                    println!("Responding with Hi...");
                    let builder = EventBuilder::text_note("Hi");
                    if let Err(e) = client_clone.send_event_builder(builder).await {
                        eprintln!("Failed to publish response: {}", e);
                    }
                }
            }
        }
    });

    Ok(client)
}

/// Methods this wallet currently supports.
const SUPPORTED_METHODS: &[Method] = &[
    Method::GetInfo,
    Method::GetBalance,
    Method::PayInvoice,
    Method::PayKeysend,
    Method::MakeInvoice,
    Method::LookupInvoice,
    Method::ListTransactions,
    Method::MakeHoldInvoice,
    Method::CancelHoldInvoice,
    Method::SettleHoldInvoice,
    Method::PayOnchain,
    Method::MakeNewAddress,
    Method::PayOffer,
    Method::MakeOffer,
    Method::LookupOffer,
    Method::LookupAddress,
    Method::ListInvoices,
    Method::ListOffers,
    Method::DisableOffer,
    Method::ListAddresses,
    Method::PayBip321,
    Method::MakeBip321,
    Method::SubscribeNotifications,
    Method::EstimateOnchainFees,
    Method::EstimateRoutingFees,
];

pub const CONTROL_INFO_KIND: u16 = 13198;

const SUPPORTED_CONTROL_METHODS: &[&str] = &[
    "connect_peer",
    "open_channel",
    "close_channel",
    "list_channels",
    "list_peers",
    "disconnect_peer",
    "subscribe_notifications",
    "get_channel_fees",
    "set_channel_fees",
    "list_network_nodes",
    "get_network_node",
    "get_network_stats",
    "get_network_channel",
    "get_forwarding_history",
    "get_pending_htlcs",
    "query_routes",
];

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ControlRequest {
    method: String,
    #[serde(default)]
    params: Value,
}

#[derive(Debug, Clone, Deserialize)]
struct OpenChannelParams {
    pubkey: String,
    #[serde(default)]
    host: Option<String>,
    amount: u64,
    #[serde(default)]
    push_amount: Option<u64>,
    #[serde(default)]
    private: Option<bool>,
    #[serde(default)]
    close_address: Option<String>,
    #[serde(default)]
    notify: Option<bool>,
}

#[derive(Debug, Clone, Deserialize)]
struct ConnectPeerParams {
    pubkey: String,
    host: String,
}

#[derive(Debug, Clone, Deserialize)]
struct DisconnectPeerParams {
    pubkey: String,
}

#[derive(Debug, Clone, Deserialize)]
struct CloseChannelParams {
    id: String,
    #[serde(default)]
    force: bool,
    #[serde(default)]
    close_address: Option<String>,
    #[serde(default)]
    notify: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ControlError {
    code: String,
    message: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ControlResponse {
    result_type: String,
    result: Option<Value>,
    error: Option<ControlError>,
}

/// Starts a NWC (Nostr Wallet Connect) service that listens for NIP-47
/// requests and responds to them.
///
/// On startup, publishes a Kind 13194 (WalletConnectInfo) event advertising
/// supported methods. Then listens for Kind 23194 requests and responds.
///
/// Currently handles `get_info` requests with stub data. Other request
/// types receive a `NotImplemented` error response.
pub async fn run_nwc_service(keys: Keys, relay_url: &str) -> Result<Client> {
    set_global_keys(&keys);
    let client = Client::builder().signer(keys.clone()).build();
    client.add_relay(relay_url).await?;
    client.connect().await;

    // Publish NWC capabilities (Kind 13194) — space-separated list of supported methods
    let methods_str: String = SUPPORTED_METHODS
        .iter()
        .map(|m| m.as_str())
        .collect::<Vec<_>>()
        .join(" ");
    let info_event = EventBuilder::new(Kind::WalletConnectInfo, methods_str)
        .tag(Tag::parse(["encryption", "nip44_v2 nip04"]).unwrap())
        .tag(Tag::parse(["notifications", "payment_received payment_sent hold_invoice_accepted"]).unwrap());
    client.send_event_builder(info_event).await?;

    // Publish NNC capabilities (Kind 13198) — space-separated list of supported control methods
    let control_methods_str: String = SUPPORTED_CONTROL_METHODS.join(" ");
    let nnc_info_event = EventBuilder::new(Kind::Custom(CONTROL_INFO_KIND), control_methods_str)
        .tag(Tag::parse(["encryption", "nip44_v2 nip04"]).unwrap())
        .tag(Tag::parse(["notifications", "channel_opened channel_closed"]).unwrap());
    client.send_event_builder(nnc_info_event).await?;

    let our_pubkey = keys.public_key();

    // Subscribe to wallet requests addressed to us (p-tagged with our pubkey)
    // Use since=now to ignore stale requests from before we started
    let startup_time = Timestamp::now();
    let wallet_filter = Filter::new()
        .kind(Kind::WalletConnectRequest)
        .pubkey(our_pubkey)
        .since(startup_time);
    client.subscribe(wallet_filter).await?;

    // Subscribe to control requests addressed to us (p-tagged with our pubkey)
    let control_filter = Filter::new()
        .kind(Kind::Custom(CONTROL_REQUEST_KIND))
        .pubkey(our_pubkey)
        .since(startup_time);
    client.subscribe(control_filter).await?;

    // Subscribe to access grant updates (Kind 30078).
    let grants_filter = if let Some(relay_pubkey) = RELAY_PUBKEY.get() {
        Filter::new()
            .kind(Kind::Custom(30078))
            .pubkey(relay_pubkey.clone())
    } else {
        Filter::new().kind(Kind::Custom(30078))
    };
    client.subscribe(grants_filter.clone()).await?;

    let client_clone = client.clone();
    let grants_client = client.clone();
    let grants_filter_clone = grants_filter.clone();

    // Periodically fetch access grants in case subscription events are missed.
    tokio::spawn(async move {
        loop {
            if let Ok(events) = grants_client
                .fetch_events(grants_filter_clone.clone())
                .timeout(Duration::from_secs(2))
                .await
            {
                for event in events.iter() {
                    if let Some(target_pubkey) = parse_grant_target(event) {
                        if !should_apply_grant_event(&target_pubkey, event) {
                            continue;
                        }
                        if let Ok(profile) = serde_json::from_str::<UsageProfile>(&event.content) {
                            usage_profile::upsert_usage_profile_and_reset_states(
                                &target_pubkey,
                                profile,
                            );
                        }
                    }
                }
            }
            tokio::time::sleep(Duration::from_secs(30)).await;
        }
    });

    tokio::spawn(async move {
        let mut notifications = client_clone.notifications();
        while let Some(notification) = notifications.next().await {
            if let ClientNotification::Event { event, .. } = notification {
                let event = event.as_ref();
                if event.kind == Kind::WalletConnectRequest {
                    if let Err(e) = handle_nwc_request(&client_clone, &keys, event).await {
                        eprintln!("Failed to handle NWC request: {}", e);
                    }
                } else if event.kind == Kind::Custom(CONTROL_REQUEST_KIND) {
                    if let Err(e) = handle_control_request(&client_clone, &keys, event).await {
                        eprintln!("Failed to handle control request: {}", e);
                    }
                } else if event.kind == Kind::Custom(30078) {
                    if let Some(target_pubkey) = parse_grant_target(event) {
                        if !should_apply_grant_event(&target_pubkey, event) {
                            continue;
                        }
                        match serde_json::from_str::<UsageProfile>(&event.content) {
                            Ok(profile) => {
                                usage_profile::upsert_usage_profile_and_reset_states(
                                    &target_pubkey,
                                    profile,
                                );
                            }
                            Err(e) => {
                                eprintln!("Failed to parse UsageProfile: {}", e);
                            }
                        }
                    } else {
                        eprintln!("Access grant event missing or invalid d tag");
                    }
                }
            }
        }
    });

    Ok(client)
}

pub async fn run_nwc_service_with_ldk(
    keys: Keys,
    relay_url: &str,
    ldk_service: Arc<LdkService>,
    node_alias: Option<String>,
) -> Result<Client> {
    let _ = NODE_ALIAS.set(node_alias);
    set_ldk_service(ldk_service.clone());
    let client = run_nwc_service(keys.clone(), relay_url).await?;

    // Spawn LDK event loop for notification publishing
    let event_client = client.clone();
    let event_keys = keys.clone();
    let event_ldk = ldk_service;
    tokio::spawn(async move {
        loop {
            let event = event_ldk.node().next_event_async().await;
            if let Err(e) = handle_ldk_event(&event_client, &event_keys, &event_ldk, &event).await
            {
                eprintln!("Failed to handle LDK event: {e}");
            }
            if let Err(e) = event_ldk.node().event_handled() {
                eprintln!("Failed to mark event handled: {e}");
            }
        }
    });

    Ok(client)
}

fn payment_hash_from_kind(kind: &PaymentKind) -> Option<String> {
    match kind {
        PaymentKind::Bolt11 { hash, .. } => Some(hex_payment_hash(&hash.0)),
        PaymentKind::Bolt11Jit { hash, .. } => Some(hex_payment_hash(&hash.0)),
        PaymentKind::Spontaneous { hash, .. } => Some(hex_payment_hash(&hash.0)),
        PaymentKind::Bolt12Offer { hash, .. } => hash.as_ref().map(|h| hex_payment_hash(&h.0)),
        PaymentKind::Bolt12Refund { hash, .. } => hash.as_ref().map(|h| hex_payment_hash(&h.0)),
        _ => None,
    }
}

fn preimage_from_kind(kind: &PaymentKind) -> Option<String> {
    match kind {
        PaymentKind::Bolt11 { preimage, .. } => preimage.as_ref().map(|p| hex_payment_hash(&p.0)),
        PaymentKind::Bolt11Jit { preimage, .. } => preimage.as_ref().map(|p| hex_payment_hash(&p.0)),
        PaymentKind::Spontaneous { preimage, .. } => preimage.as_ref().map(|p| hex_payment_hash(&p.0)),
        PaymentKind::Bolt12Offer { preimage, .. } => preimage.as_ref().map(|p| hex_payment_hash(&p.0)),
        PaymentKind::Bolt12Refund { preimage, .. } => preimage.as_ref().map(|p| hex_payment_hash(&p.0)),
        _ => None,
    }
}

fn payment_method_from_kind(kind: &PaymentKind) -> Option<PaymentMethod> {
    match kind {
        PaymentKind::Bolt11 { .. } | PaymentKind::Bolt11Jit { .. } => Some(PaymentMethod::Bolt11),
        PaymentKind::Bolt12Offer { .. } | PaymentKind::Bolt12Refund { .. } => Some(PaymentMethod::Bolt12),
        PaymentKind::Spontaneous { .. } => Some(PaymentMethod::Keysend),
        _ => None,
    }
}

fn hex_payment_hash(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        use std::fmt::Write as _;
        let _ = write!(&mut out, "{:02x}", b);
    }
    out
}

fn payment_details_to_lookup_invoice_response(payment: &PaymentDetails) -> LookupInvoiceResponse {
    let transaction_type = match payment.direction {
        PaymentDirection::Inbound => Some(TransactionType::Incoming),
        PaymentDirection::Outbound => Some(TransactionType::Outgoing),
    };

    let state = match payment.status {
        PaymentStatus::Pending => Some(TransactionState::Pending),
        PaymentStatus::Succeeded => Some(TransactionState::Settled),
        PaymentStatus::Failed => Some(TransactionState::Failed),
    };

    let payment_hash = payment_hash_from_kind(&payment.kind).unwrap_or_default();
    let preimage = preimage_from_kind(&payment.kind);

    let settled_at = if payment.status == PaymentStatus::Succeeded {
        Some(Timestamp::from(payment.latest_update_timestamp))
    } else {
        None
    };

    LookupInvoiceResponse {
        transaction_type,
        state,
        invoice: None,
        description: None,
        description_hash: None,
        preimage,
        payment_hash,
        amount: payment.amount_msat.unwrap_or(0),
        fees_paid: payment.fee_paid_msat.unwrap_or(0),
        created_at: if payment.latest_update_timestamp > 0 {
            Some(Timestamp::from(payment.latest_update_timestamp))
        } else {
            None
        },
        expires_at: None,
        settled_at,
        metadata: None,
        payment_method: payment_method_from_kind(&payment.kind),
    }
}

trait Handler: Send + Sync {
    fn validate(&self, req: &Request) -> Result<(), NIP47Error>;
    fn execute(&self, req: &Request, caller_pubkey: &str) -> Result<Response, NIP47Error>;
}

struct GetInfoHandler;

impl Handler for GetInfoHandler {
    fn validate(&self, _req: &Request) -> Result<(), NIP47Error> {
        if _req.params != RequestParams::GetInfo {
            return Err(NIP47Error {
                code: ErrorCode::Other,
                message: "invalid params for get_info".to_string(),
            });
        }
        Ok(())
    }

    fn execute(&self, _req: &Request, caller_pubkey: &str) -> Result<Response, NIP47Error> {
        let methods = allowed_methods_for(caller_pubkey);
        let (pubkey, network, block_height) = if let Some(ldk_service) = get_ldk_service() {
            (
                ldk_service.node_id(),
                ldk_service.network().to_string(),
                ldk_service.status().latest_best_block_height,
            )
        } else {
            (
                GLOBAL_KEYS.get().unwrap().public_key().to_string(),
                "regtest".to_string(),
                0u32,
            )
        };
        Ok(Response {
            result_type: Method::GetInfo,
            error: None,
            result: Some(ResponseResult::GetInfo(GetInfoResponse {
                alias: Some(NODE_ALIAS.get().and_then(|a| a.clone()).unwrap_or_else(|| "dln-node".to_string())),
                color: None,
                pubkey: Some(pubkey),
                network: Some(network),
                block_height: Some(block_height),
                block_hash: None,
                methods,
                notifications: if get_ldk_service().is_some() {
                    vec![
                        "payment_received".into(),
                        "payment_sent".into(),
                        "hold_invoice_accepted".into(),
                    ]
                } else {
                    vec![]
                },
                bip321_methods: if get_ldk_service().is_some() {
                    Some(vec![
                        Bip321MethodInfo { method: "bolt11".into(), address_types: None },
                        Bip321MethodInfo { method: "bolt12".into(), address_types: None },
                        Bip321MethodInfo { method: "onchain".into(), address_types: Some(vec!["p2tr".into()]) },
                    ])
                } else {
                    None
                },
            })),
        })
    }
}

struct GetBalanceHandler;

impl Handler for GetBalanceHandler {
    fn validate(&self, req: &Request) -> Result<(), NIP47Error> {
        if req.params != RequestParams::GetBalance {
            return Err(NIP47Error {
                code: ErrorCode::Other,
                message: "invalid params for get_balance".to_string(),
            });
        }
        Ok(())
    }

    fn execute(&self, _req: &Request, _caller_pubkey: &str) -> Result<Response, NIP47Error> {
        let bal = if let Some(ldk_service) = get_ldk_service() {
            ldk_service.sync_wallets().map_err(|e| NIP47Error {
                code: ErrorCode::Other,
                message: format!("ldk sync failed: {e}"),
            })?;
            ldk_service.get_balance().map_err(|e| NIP47Error {
                code: ErrorCode::Other,
                message: format!("ldk balance failed: {e}"),
            })?
        } else {
            LdkBalance { total_msat: 0, lightning_msat: 0, onchain_msat: 0 }
        };

        Ok(Response {
            result_type: Method::GetBalance,
            error: None,
            result: Some(ResponseResult::GetBalance(GetBalanceResponse {
                balance: bal.total_msat,
                lightning_balance: Some(bal.lightning_msat),
                onchain_balance: Some(bal.onchain_msat),
            })),
        })
    }
}

struct PayInvoiceHandler;

impl Handler for PayInvoiceHandler {
    fn validate(&self, req: &Request) -> Result<(), NIP47Error> {
        if let RequestParams::PayInvoice(params) = &req.params {
            if params.invoice.trim().is_empty() {
                return Err(NIP47Error {
                    code: ErrorCode::Other,
                    message: "invoice is required".to_string(),
                });
            }
            if params.amount == Some(0) {
                return Err(NIP47Error {
                    code: ErrorCode::Other,
                    message: "amount must be greater than 0".to_string(),
                });
            }
            return Ok(());
        }

        Err(NIP47Error {
            code: ErrorCode::Other,
            message: "invalid params for pay_invoice".to_string(),
        })
    }

    fn execute(&self, _req: &Request, _caller_pubkey: &str) -> Result<Response, NIP47Error> {
        if let (Some(ldk_service), RequestParams::PayInvoice(params)) =
            (get_ldk_service(), &_req.params)
        {
            let payment = ldk_service
                .pay_invoice(&params.invoice, params.amount)
                .map_err(|e| map_ldk_service_error("pay_invoice", ErrorCode::PaymentFailed, e))?;
            return Ok(Response {
                result_type: Method::PayInvoice,
                error: None,
                result: Some(ResponseResult::PayInvoice(PayInvoiceResponse {
                    preimage: payment.preimage,
                    fees_paid: payment.fees_paid_msat,
                })),
            });
        }

        Ok(Response {
            result_type: Method::PayInvoice,
            error: None,
            result: Some(ResponseResult::PayInvoice(PayInvoiceResponse {
                preimage: "00".to_string(),
                fees_paid: Some(0),
            })),
        })
    }
}

struct PayKeysendHandler;

impl Handler for PayKeysendHandler {
    fn validate(&self, req: &Request) -> Result<(), NIP47Error> {
        if let RequestParams::PayKeysend(params) = &req.params {
            if params.pubkey.trim().is_empty() {
                return Err(NIP47Error {
                    code: ErrorCode::Other,
                    message: "pubkey is required".to_string(),
                });
            }
            if params.amount == 0 {
                return Err(NIP47Error {
                    code: ErrorCode::Other,
                    message: "amount must be greater than 0".to_string(),
                });
            }
            return Ok(());
        }

        Err(NIP47Error {
            code: ErrorCode::Other,
            message: "invalid params for pay_keysend".to_string(),
        })
    }

    fn execute(&self, _req: &Request, _caller_pubkey: &str) -> Result<Response, NIP47Error> {
        if let (Some(ldk_service), RequestParams::PayKeysend(params)) =
            (get_ldk_service(), &_req.params)
        {
            let payment = ldk_service
                .pay_keysend(&params.pubkey, params.amount)
                .map_err(|e| map_ldk_service_error("pay_keysend", ErrorCode::PaymentFailed, e))?;
            return Ok(Response {
                result_type: Method::PayKeysend,
                error: None,
                result: Some(ResponseResult::PayKeysend(PayKeysendResponse {
                    preimage: payment.preimage,
                    fees_paid: payment.fees_paid_msat,
                })),
            });
        }

        Ok(Response {
            result_type: Method::PayKeysend,
            error: None,
            result: Some(ResponseResult::PayKeysend(PayKeysendResponse {
                preimage: "00".to_string(),
                fees_paid: Some(0),
            })),
        })
    }
}

struct MakeInvoiceHandler;

impl Handler for MakeInvoiceHandler {
    fn validate(&self, req: &Request) -> Result<(), NIP47Error> {
        if let RequestParams::MakeInvoice(params) = &req.params {
            if params.amount == 0 {
                return Err(NIP47Error {
                    code: ErrorCode::Other,
                    message: "amount must be greater than 0".to_string(),
                });
            }
            return Ok(());
        }

        Err(NIP47Error {
            code: ErrorCode::Other,
            message: "invalid params for make_invoice".to_string(),
        })
    }

    fn execute(&self, _req: &Request, _caller_pubkey: &str) -> Result<Response, NIP47Error> {
        if let (Some(ldk_service), RequestParams::MakeInvoice(params)) =
            (get_ldk_service(), &_req.params)
        {
            let invoice = ldk_service
                .make_invoice(
                    params.amount,
                    params.description.as_deref(),
                    params.description_hash.as_deref(),
                    params.expiry,
                )
                .map_err(|e| map_ldk_service_error("make_invoice", ErrorCode::Other, e))?;
            return Ok(Response {
                result_type: Method::MakeInvoice,
                error: None,
                result: Some(ResponseResult::MakeInvoice(MakeInvoiceResponse {
                    invoice: invoice.invoice,
                    payment_hash: invoice.payment_hash,
                    description: params.description.clone(),
                    description_hash: params.description_hash.clone(),
                    preimage: None,
                    amount: invoice.amount_msat,
                    created_at: Some(Timestamp::now()),
                    expires_at: invoice.expires_at.map(Into::into),
                })),
            });
        }

        Ok(Response {
            result_type: Method::MakeInvoice,
            error: None,
            result: Some(ResponseResult::MakeInvoice(MakeInvoiceResponse {
                invoice: "dummy_invoice".to_string(),
                payment_hash: None,
                description: None,
                description_hash: None,
                preimage: None,
                amount: None,
                created_at: None,
                expires_at: None,
            })),
        })
    }
}

struct LookupInvoiceHandler;

impl Handler for LookupInvoiceHandler {
    fn validate(&self, req: &Request) -> Result<(), NIP47Error> {
        if let RequestParams::LookupInvoice(params) = &req.params {
            if params.payment_hash.as_deref().unwrap_or("").is_empty()
                && params.invoice.as_deref().unwrap_or("").is_empty()
            {
                return Err(NIP47Error {
                    code: ErrorCode::Other,
                    message: "payment_hash or invoice is required".to_string(),
                });
            }
            return Ok(());
        }

        Err(NIP47Error {
            code: ErrorCode::Other,
            message: "invalid params for lookup_invoice".to_string(),
        })
    }

    fn execute(&self, req: &Request, _caller_pubkey: &str) -> Result<Response, NIP47Error> {
        if let Some(ldk_service) = get_ldk_service() {
            if let RequestParams::LookupInvoice(params) = &req.params {
                let payment = if let Some(invoice) = &params.invoice {
                    ldk_service.lookup_payment_by_bolt11(invoice)
                } else if let Some(hash) = &params.payment_hash {
                    ldk_service.lookup_payment_by_hash(hash)
                } else {
                    return Err(NIP47Error {
                        code: ErrorCode::Other,
                        message: "payment_hash or invoice is required".to_string(),
                    });
                };

                match payment {
                    Ok(details) => {
                        let response = payment_details_to_lookup_invoice_response(&details);
                        return Ok(Response {
                            result_type: Method::LookupInvoice,
                            error: None,
                            result: Some(ResponseResult::LookupInvoice(response)),
                        });
                    }
                    Err(LdkServiceError::PaymentNotFound(msg)) => {
                        return Err(NIP47Error {
                            code: ErrorCode::NotFound,
                            message: format!("payment not found: {msg}"),
                        });
                    }
                    Err(e) => {
                        return Err(map_ldk_service_error("lookup_invoice", ErrorCode::Other, e));
                    }
                }
            }
        }

        // Fallback stub when no LDK service is available
        Ok(Response {
            result_type: Method::LookupInvoice,
            error: None,
            result: Some(ResponseResult::LookupInvoice(LookupInvoiceResponse {
                transaction_type: Some(TransactionType::Outgoing),
                state: Some(TransactionState::Settled),
                invoice: None,
                description: None,
                description_hash: None,
                preimage: None,
                payment_hash: "00".to_string(),
                amount: 0,
                fees_paid: 0,
                created_at: Some(Timestamp::now()),
                expires_at: None,
                settled_at: None,
                metadata: None,
                payment_method: None,
            })),
        })
    }
}

struct ListTransactionsHandler;

impl Handler for ListTransactionsHandler {
    fn validate(&self, req: &Request) -> Result<(), NIP47Error> {
        if let RequestParams::ListTransactions(_) = &req.params {
            return Ok(());
        }

        Err(NIP47Error {
            code: ErrorCode::Other,
            message: "invalid params for list_transactions".to_string(),
        })
    }

    fn execute(&self, req: &Request, _caller_pubkey: &str) -> Result<Response, NIP47Error> {
        if let Some(ldk_service) = get_ldk_service() {
            if let RequestParams::ListTransactions(params) = &req.params {
                let direction = params.transaction_type.map(|tt| match tt {
                    TransactionType::Incoming => PaymentDirection::Inbound,
                    TransactionType::Outgoing => PaymentDirection::Outbound,
                });

                let payments = ldk_service.list_payments_filtered(
                    params.from.map(|t| t.as_secs()),
                    params.until.map(|t| t.as_secs()),
                    params.limit,
                    params.offset,
                    params.unpaid,
                    direction,
                    params.payment_method,
                );

                let transactions: Vec<LookupInvoiceResponse> = payments
                    .iter()
                    .map(payment_details_to_lookup_invoice_response)
                    .collect();

                return Ok(Response {
                    result_type: Method::ListTransactions,
                    error: None,
                    result: Some(ResponseResult::ListTransactions(transactions)),
                });
            }
        }

        // Fallback stub when no LDK service is available
        Ok(Response {
            result_type: Method::ListTransactions,
            error: None,
            result: Some(ResponseResult::ListTransactions(Vec::new())),
        })
    }
}

struct MakeHoldInvoiceHandler;

impl Handler for MakeHoldInvoiceHandler {
    fn validate(&self, req: &Request) -> Result<(), NIP47Error> {
        if let RequestParams::MakeHoldInvoice(params) = &req.params {
            if params.payment_hash.trim().is_empty() {
                return Err(NIP47Error {
                    code: ErrorCode::Other,
                    message: "payment_hash is required".to_string(),
                });
            }
            if params.amount == 0 {
                return Err(NIP47Error {
                    code: ErrorCode::Other,
                    message: "amount must be greater than 0".to_string(),
                });
            }
            return Ok(());
        }

        Err(NIP47Error {
            code: ErrorCode::Other,
            message: "invalid params for make_hold_invoice".to_string(),
        })
    }

    fn execute(&self, req: &Request, _caller_pubkey: &str) -> Result<Response, NIP47Error> {
        let ldk_service = get_ldk_service().ok_or_else(|| NIP47Error {
            code: ErrorCode::Other,
            message: "ldk service unavailable".to_string(),
        })?;

        if let RequestParams::MakeHoldInvoice(params) = &req.params {
            let invoice = ldk_service
                .make_hold_invoice(
                    params.amount,
                    params.description.as_deref(),
                    params.expiry,
                    &params.payment_hash,
                )
                .map_err(|e| map_ldk_service_error("make_hold_invoice", ErrorCode::Other, e))?;

            return Ok(Response {
                result_type: Method::MakeHoldInvoice,
                error: None,
                result: Some(ResponseResult::MakeHoldInvoice(MakeHoldInvoiceResponse {
                    transaction_type: TransactionType::Incoming,
                    invoice: Some(invoice.invoice),
                    description: params.description.clone(),
                    description_hash: params.description_hash.clone(),
                    payment_hash: invoice
                        .payment_hash
                        .unwrap_or_else(|| params.payment_hash.clone()),
                    amount: invoice.amount_msat.unwrap_or(params.amount),
                    created_at: Timestamp::now(),
                    expires_at: invoice
                        .expires_at
                        .map(Timestamp::from_secs)
                        .unwrap_or_else(Timestamp::now),
                    metadata: None,
                })),
            });
        }

        Err(NIP47Error {
            code: ErrorCode::Other,
            message: "invalid params for make_hold_invoice".to_string(),
        })
    }
}

struct CancelHoldInvoiceHandler;

impl Handler for CancelHoldInvoiceHandler {
    fn validate(&self, req: &Request) -> Result<(), NIP47Error> {
        if let RequestParams::CancelHoldInvoice(params) = &req.params {
            if params.payment_hash.trim().is_empty() {
                return Err(NIP47Error {
                    code: ErrorCode::Other,
                    message: "payment_hash is required".to_string(),
                });
            }
            return Ok(());
        }

        Err(NIP47Error {
            code: ErrorCode::Other,
            message: "invalid params for cancel_hold_invoice".to_string(),
        })
    }

    fn execute(&self, req: &Request, _caller_pubkey: &str) -> Result<Response, NIP47Error> {
        let ldk_service = get_ldk_service().ok_or_else(|| NIP47Error {
            code: ErrorCode::Other,
            message: "ldk service unavailable".to_string(),
        })?;

        if let RequestParams::CancelHoldInvoice(params) = &req.params {
            ldk_service
                .cancel_hold_invoice(&params.payment_hash)
                .map_err(|e| map_ldk_service_error("cancel_hold_invoice", ErrorCode::Other, e))?;

            return Ok(Response {
                result_type: Method::CancelHoldInvoice,
                error: None,
                result: Some(ResponseResult::CancelHoldInvoice(
                    CancelHoldInvoiceResponse {},
                )),
            });
        }

        Err(NIP47Error {
            code: ErrorCode::Other,
            message: "invalid params for cancel_hold_invoice".to_string(),
        })
    }
}

struct SettleHoldInvoiceHandler;

impl Handler for SettleHoldInvoiceHandler {
    fn validate(&self, req: &Request) -> Result<(), NIP47Error> {
        if let RequestParams::SettleHoldInvoice(params) = &req.params {
            if params.preimage.trim().is_empty() {
                return Err(NIP47Error {
                    code: ErrorCode::Other,
                    message: "preimage is required".to_string(),
                });
            }
            return Ok(());
        }

        Err(NIP47Error {
            code: ErrorCode::Other,
            message: "invalid params for settle_hold_invoice".to_string(),
        })
    }

    fn execute(&self, req: &Request, _caller_pubkey: &str) -> Result<Response, NIP47Error> {
        let ldk_service = get_ldk_service().ok_or_else(|| NIP47Error {
            code: ErrorCode::Other,
            message: "ldk service unavailable".to_string(),
        })?;

        if let RequestParams::SettleHoldInvoice(params) = &req.params {
            ldk_service
                .settle_hold_invoice(&params.preimage)
                .map_err(|e| map_ldk_service_error("settle_hold_invoice", ErrorCode::Other, e))?;

            return Ok(Response {
                result_type: Method::SettleHoldInvoice,
                error: None,
                result: Some(ResponseResult::SettleHoldInvoice(
                    SettleHoldInvoiceResponse {},
                )),
            });
        }

        Err(NIP47Error {
            code: ErrorCode::Other,
            message: "invalid params for settle_hold_invoice".to_string(),
        })
    }
}

struct MakeNewAddressHandler;

impl Handler for MakeNewAddressHandler {
    fn validate(&self, req: &Request) -> Result<(), NIP47Error> {
        if req.params != RequestParams::MakeNewAddress {
            return Err(NIP47Error {
                code: ErrorCode::Other,
                message: "invalid params for make_new_address".to_string(),
            });
        }
        Ok(())
    }

    fn execute(&self, _req: &Request, _caller_pubkey: &str) -> Result<Response, NIP47Error> {
        let ldk_service = get_ldk_service().ok_or_else(|| NIP47Error {
            code: ErrorCode::Other,
            message: "ldk service unavailable".to_string(),
        })?;

        let address = ldk_service
            .new_onchain_address()
            .map_err(|e| map_ldk_service_error("make_new_address", ErrorCode::Other, e))?;

        // Track the address in the address store
        address_store::register_address(address.clone());

        Ok(Response {
            result_type: Method::MakeNewAddress,
            error: None,
            result: Some(ResponseResult::MakeNewAddress(MakeNewAddressResponse {
                address,
            })),
        })
    }
}

struct PayOnchainHandler;

impl Handler for PayOnchainHandler {
    fn validate(&self, req: &Request) -> Result<(), NIP47Error> {
        if let RequestParams::PayOnchain(params) = &req.params {
            if params.address.trim().is_empty() {
                return Err(NIP47Error {
                    code: ErrorCode::Other,
                    message: "address is required".to_string(),
                });
            }
            if params.amount == 0 {
                return Err(NIP47Error {
                    code: ErrorCode::Other,
                    message: "amount must be greater than 0".to_string(),
                });
            }
            return Ok(());
        }

        Err(NIP47Error {
            code: ErrorCode::Other,
            message: "invalid params for pay_onchain".to_string(),
        })
    }

    fn execute(&self, req: &Request, _caller_pubkey: &str) -> Result<Response, NIP47Error> {
        let ldk_service = get_ldk_service().ok_or_else(|| NIP47Error {
            code: ErrorCode::Other,
            message: "ldk service unavailable".to_string(),
        })?;

        if let RequestParams::PayOnchain(params) = &req.params {
            let txid = ldk_service
                .pay_onchain(&params.address, params.amount, params.feerate)
                .map_err(|e| map_ldk_service_error("pay_onchain", ErrorCode::PaymentFailed, e))?;

            return Ok(Response {
                result_type: Method::PayOnchain,
                error: None,
                result: Some(ResponseResult::PayOnchain(PayOnchainResponse { txid })),
            });
        }

        Err(NIP47Error {
            code: ErrorCode::Other,
            message: "invalid params for pay_onchain".to_string(),
        })
    }
}

struct MakeOfferHandler;

impl Handler for MakeOfferHandler {
    fn validate(&self, req: &Request) -> Result<(), NIP47Error> {
        if let RequestParams::MakeOffer(params) = &req.params {
            if params.amount == 0 {
                return Err(NIP47Error {
                    code: ErrorCode::Other,
                    message: "amount must be greater than 0".to_string(),
                });
            }
            if params.description.trim().is_empty() {
                return Err(NIP47Error {
                    code: ErrorCode::Other,
                    message: "description is required".to_string(),
                });
            }
            return Ok(());
        }

        Err(NIP47Error {
            code: ErrorCode::Other,
            message: "invalid params for make_offer".to_string(),
        })
    }

    fn execute(&self, req: &Request, _caller_pubkey: &str) -> Result<Response, NIP47Error> {
        let ldk_service = get_ldk_service().ok_or_else(|| NIP47Error {
            code: ErrorCode::Other,
            message: "ldk service unavailable".to_string(),
        })?;

        if let RequestParams::MakeOffer(params) = &req.params {
            let offer_str = ldk_service
                .make_offer(params.amount, &params.description, params.expiry)
                .map_err(|e| map_ldk_service_error("make_offer", ErrorCode::Other, e))?;

            // Track the offer in the offer store
            let offer_id = Offer::from_str(&offer_str)
                .map(|o| o.id().0)
                .unwrap_or([0u8; 32]);
            offer_store::insert_offer(offer_str.clone(), params.description.clone(), params.amount, offer_id);

            return Ok(Response {
                result_type: Method::MakeOffer,
                error: None,
                result: Some(ResponseResult::MakeOffer(MakeOfferResponse {
                    offer: offer_str,
                    description: Some(params.description.clone()),
                    amount: Some(params.amount),
                })),
            });
        }

        Err(NIP47Error {
            code: ErrorCode::Other,
            message: "invalid params for make_offer".to_string(),
        })
    }
}

struct PayOfferHandler;

impl Handler for PayOfferHandler {
    fn validate(&self, req: &Request) -> Result<(), NIP47Error> {
        if let RequestParams::PayOffer(params) = &req.params {
            if params.offer.trim().is_empty() {
                return Err(NIP47Error {
                    code: ErrorCode::Other,
                    message: "offer is required".to_string(),
                });
            }
            return Ok(());
        }

        Err(NIP47Error {
            code: ErrorCode::Other,
            message: "invalid params for pay_offer".to_string(),
        })
    }

    fn execute(&self, req: &Request, _caller_pubkey: &str) -> Result<Response, NIP47Error> {
        let ldk_service = get_ldk_service().ok_or_else(|| NIP47Error {
            code: ErrorCode::Other,
            message: "ldk service unavailable".to_string(),
        })?;

        if let RequestParams::PayOffer(params) = &req.params {
            let payment = ldk_service
                .pay_offer(&params.offer, params.amount, params.payer_note.clone())
                .map_err(|e| map_ldk_service_error("pay_offer", ErrorCode::PaymentFailed, e))?;

            return Ok(Response {
                result_type: Method::PayOffer,
                error: None,
                result: Some(ResponseResult::PayOffer(PayOfferResponse {
                    preimage: Some(payment.preimage),
                    fees_paid: payment.fees_paid_msat,
                })),
            });
        }

        Err(NIP47Error {
            code: ErrorCode::Other,
            message: "invalid params for pay_offer".to_string(),
        })
    }
}

struct LookupOfferHandler;

impl Handler for LookupOfferHandler {
    fn validate(&self, req: &Request) -> Result<(), NIP47Error> {
        if let RequestParams::LookupOffer(params) = &req.params {
            if params.offer.trim().is_empty() {
                return Err(NIP47Error {
                    code: ErrorCode::Other,
                    message: "offer is required".to_string(),
                });
            }
            return Ok(());
        }

        Err(NIP47Error {
            code: ErrorCode::Other,
            message: "invalid params for lookup_offer".to_string(),
        })
    }

    fn execute(&self, req: &Request, _caller_pubkey: &str) -> Result<Response, NIP47Error> {
        if let RequestParams::LookupOffer(params) = &req.params {
            let record = offer_store::get_offer(&params.offer).ok_or_else(|| NIP47Error {
                code: ErrorCode::NotFound,
                message: format!("offer not found: {}", params.offer),
            })?;

            return Ok(Response {
                result_type: Method::LookupOffer,
                error: None,
                result: Some(ResponseResult::LookupOffer(LookupOfferResponse {
                    offer: record.offer,
                    description: Some(record.description),
                    amount: Some(record.amount_msat),
                    active: record.active,
                    num_payments_received: record.num_payments_received,
                    total_received: record.total_received_msat,
                })),
            });
        }

        Err(NIP47Error {
            code: ErrorCode::Other,
            message: "invalid params for lookup_offer".to_string(),
        })
    }
}

struct LookupAddressHandler;

impl Handler for LookupAddressHandler {
    fn validate(&self, req: &Request) -> Result<(), NIP47Error> {
        if let RequestParams::LookupAddress(params) = &req.params {
            if params.address.trim().is_empty() {
                return Err(NIP47Error {
                    code: ErrorCode::Other,
                    message: "address is required".to_string(),
                });
            }
            return Ok(());
        }

        Err(NIP47Error {
            code: ErrorCode::Other,
            message: "invalid params for lookup_address".to_string(),
        })
    }

    fn execute(&self, req: &Request, _caller_pubkey: &str) -> Result<Response, NIP47Error> {
        if let RequestParams::LookupAddress(params) = &req.params {
            let record = address_store::get_address(&params.address).ok_or_else(|| NIP47Error {
                code: ErrorCode::NotFound,
                message: format!("address not found: {}", params.address),
            })?;

            let transactions: Vec<AddressTransaction> = record
                .transactions
                .iter()
                .map(|tx| AddressTransaction {
                    txid: tx.txid.clone(),
                    amount: tx.amount_sats,
                    timestamp: Timestamp::from(tx.timestamp),
                })
                .collect();

            let total_received: u64 = record.transactions.iter().map(|tx| tx.amount_sats).sum();

            return Ok(Response {
                result_type: Method::LookupAddress,
                error: None,
                result: Some(ResponseResult::LookupAddress(LookupAddressResponse {
                    address: record.address,
                    total_received,
                    transactions,
                })),
            });
        }

        Err(NIP47Error {
            code: ErrorCode::Other,
            message: "invalid params for lookup_address".to_string(),
        })
    }
}

struct ListInvoicesHandler;

impl Handler for ListInvoicesHandler {
    fn validate(&self, req: &Request) -> Result<(), NIP47Error> {
        if let RequestParams::ListInvoices(_) = &req.params {
            return Ok(());
        }

        Err(NIP47Error {
            code: ErrorCode::Other,
            message: "invalid params for list_invoices".to_string(),
        })
    }

    fn execute(&self, req: &Request, _caller_pubkey: &str) -> Result<Response, NIP47Error> {
        if let Some(ldk_service) = get_ldk_service() {
            if let RequestParams::ListInvoices(params) = &req.params {
                let direction = Some(PaymentDirection::Inbound);

                // Map state filter to PaymentStatus-based filtering
                let payments = ldk_service.list_payments_filtered(
                    params.from.map(|t| t.as_secs()),
                    params.until.map(|t| t.as_secs()),
                    None, // we'll apply limit/offset after state filtering
                    None,
                    Some(true), // include unpaid/pending
                    direction,
                    Some(PaymentMethod::Bolt11), // invoices are bolt11
                );

                let mut invoices: Vec<InvoiceEntry> = payments
                    .iter()
                    .filter_map(|payment| {
                        let state_str = match payment.status {
                            PaymentStatus::Pending => "pending",
                            PaymentStatus::Succeeded => "settled",
                            PaymentStatus::Failed => "expired",
                        };

                        // Filter by state if requested
                        if let Some(ref state_filter) = params.state {
                            if state_filter != state_str {
                                return None;
                            }
                        }

                        let payment_hash = payment_hash_from_kind(&payment.kind).unwrap_or_default();
                        let preimage = preimage_from_kind(&payment.kind);
                        let settled_at = if payment.status == PaymentStatus::Succeeded {
                            Some(Timestamp::from(payment.latest_update_timestamp))
                        } else {
                            None
                        };
                        let created_at = if payment.latest_update_timestamp > 0 {
                            Some(Timestamp::from(payment.latest_update_timestamp))
                        } else {
                            None
                        };

                        Some(InvoiceEntry {
                            invoice: None,
                            description: None,
                            payment_hash,
                            amount: payment.amount_msat.unwrap_or(0),
                            state: state_str.to_string(),
                            preimage,
                            created_at,
                            expires_at: None,
                            settled_at,
                        })
                    })
                    .collect();

                // Apply offset/limit
                let offset = params.offset.unwrap_or(0) as usize;
                if offset > 0 && offset < invoices.len() {
                    invoices = invoices.split_off(offset);
                } else if offset >= invoices.len() {
                    invoices.clear();
                }
                if let Some(limit) = params.limit {
                    invoices.truncate(limit as usize);
                }

                return Ok(Response {
                    result_type: Method::ListInvoices,
                    error: None,
                    result: Some(ResponseResult::ListInvoices(ListInvoicesResponse {
                        invoices,
                    })),
                });
            }
        }

        Ok(Response {
            result_type: Method::ListInvoices,
            error: None,
            result: Some(ResponseResult::ListInvoices(ListInvoicesResponse {
                invoices: Vec::new(),
            })),
        })
    }
}

struct ListOffersHandler;

impl Handler for ListOffersHandler {
    fn validate(&self, req: &Request) -> Result<(), NIP47Error> {
        if let RequestParams::ListOffers(_) = &req.params {
            return Ok(());
        }

        Err(NIP47Error {
            code: ErrorCode::Other,
            message: "invalid params for list_offers".to_string(),
        })
    }

    fn execute(&self, req: &Request, _caller_pubkey: &str) -> Result<Response, NIP47Error> {
        if let RequestParams::ListOffers(params) = &req.params {
            let mut offers: Vec<OfferEntry> = offer_store::list_offers()
                .iter()
                .filter(|record| {
                    if params.active_only.unwrap_or(false) {
                        record.active
                    } else {
                        true
                    }
                })
                .map(|record| OfferEntry {
                    offer: record.offer.clone(),
                    description: Some(record.description.clone()),
                    amount: Some(record.amount_msat),
                    active: record.active,
                    num_payments_received: record.num_payments_received,
                    total_received: record.total_received_msat,
                })
                .collect();

            // Apply offset/limit
            let offset = params.offset.unwrap_or(0) as usize;
            if offset > 0 && offset < offers.len() {
                offers = offers.split_off(offset);
            } else if offset >= offers.len() && offset > 0 {
                offers.clear();
            }
            if let Some(limit) = params.limit {
                offers.truncate(limit as usize);
            }

            return Ok(Response {
                result_type: Method::ListOffers,
                error: None,
                result: Some(ResponseResult::ListOffers(ListOffersResponse { offers })),
            });
        }

        Err(NIP47Error {
            code: ErrorCode::Other,
            message: "invalid params for list_offers".to_string(),
        })
    }
}

struct DisableOfferHandler;

impl Handler for DisableOfferHandler {
    fn validate(&self, req: &Request) -> Result<(), NIP47Error> {
        if let RequestParams::DisableOffer(params) = &req.params {
            if params.offer.trim().is_empty() {
                return Err(NIP47Error {
                    code: ErrorCode::Other,
                    message: "offer is required".to_string(),
                });
            }
            return Ok(());
        }

        Err(NIP47Error {
            code: ErrorCode::Other,
            message: "invalid params for disable_offer".to_string(),
        })
    }

    fn execute(&self, req: &Request, _caller_pubkey: &str) -> Result<Response, NIP47Error> {
        if let RequestParams::DisableOffer(params) = &req.params {
            if !offer_store::disable_offer(&params.offer) {
                return Err(NIP47Error {
                    code: ErrorCode::NotFound,
                    message: format!("offer not found: {}", params.offer),
                });
            }

            return Ok(Response {
                result_type: Method::DisableOffer,
                error: None,
                result: Some(ResponseResult::DisableOffer(DisableOfferResponse {})),
            });
        }

        Err(NIP47Error {
            code: ErrorCode::Other,
            message: "invalid params for disable_offer".to_string(),
        })
    }
}

struct ListAddressesHandler;

impl Handler for ListAddressesHandler {
    fn validate(&self, req: &Request) -> Result<(), NIP47Error> {
        if let RequestParams::ListAddresses(_) = &req.params {
            return Ok(());
        }

        Err(NIP47Error {
            code: ErrorCode::Other,
            message: "invalid params for list_addresses".to_string(),
        })
    }

    fn execute(&self, req: &Request, _caller_pubkey: &str) -> Result<Response, NIP47Error> {
        if let RequestParams::ListAddresses(params) = &req.params {
            let mut addresses: Vec<AddressEntry> = address_store::list_addresses()
                .iter()
                .map(|record| {
                    let total_received: u64 =
                        record.transactions.iter().map(|tx| tx.amount_sats).sum();
                    AddressEntry {
                        address: record.address.clone(),
                        total_received,
                    }
                })
                .collect();

            // Apply offset/limit
            let offset = params.offset.unwrap_or(0) as usize;
            if offset > 0 && offset < addresses.len() {
                addresses = addresses.split_off(offset);
            } else if offset >= addresses.len() && offset > 0 {
                addresses.clear();
            }
            if let Some(limit) = params.limit {
                addresses.truncate(limit as usize);
            }

            return Ok(Response {
                result_type: Method::ListAddresses,
                error: None,
                result: Some(ResponseResult::ListAddresses(ListAddressesResponse {
                    addresses,
                })),
            });
        }

        Err(NIP47Error {
            code: ErrorCode::Other,
            message: "invalid params for list_addresses".to_string(),
        })
    }
}

struct PayBip321Handler;

impl Handler for PayBip321Handler {
    fn validate(&self, req: &Request) -> Result<(), NIP47Error> {
        if let RequestParams::PayBip321(params) = &req.params {
            if params.uri.trim().is_empty() {
                return Err(NIP47Error {
                    code: ErrorCode::Other,
                    message: "uri is required".to_string(),
                });
            }
            bip321::Uri::parse(&params.uri).map_err(|e| NIP47Error {
                code: ErrorCode::Other,
                message: format!("invalid BIP-321 URI: {e}"),
            })?;
            return Ok(());
        }

        Err(NIP47Error {
            code: ErrorCode::Other,
            message: "invalid params for pay_bip321".to_string(),
        })
    }

    fn execute(&self, req: &Request, _caller_pubkey: &str) -> Result<Response, NIP47Error> {
        let ldk_service = get_ldk_service().ok_or_else(|| NIP47Error {
            code: ErrorCode::Other,
            message: "ldk service unavailable".to_string(),
        })?;

        if let RequestParams::PayBip321(params) = &req.params {
            let uri = bip321::Uri::parse(&params.uri).map_err(|e| NIP47Error {
                code: ErrorCode::Other,
                message: format!("invalid BIP-321 URI: {e}"),
            })?;

            let amount_msat = uri.amount.map(|a| a.to_sat() * 1000);

            // Priority: lightning (BOLT11) > lno (BOLT12) > on-chain address
            if let Some(invoice) = uri.lightning.first() {
                let payment = ldk_service
                    .pay_invoice(invoice.as_str(), amount_msat)
                    .map_err(|e| map_ldk_service_error("pay_bip321/bolt11", ErrorCode::PaymentFailed, e))?;
                return Ok(Response {
                    result_type: Method::PayBip321,
                    error: None,
                    result: Some(ResponseResult::PayBip321(PayBip321Response {
                        preimage: Some(payment.preimage),
                        txid: None,
                        fees_paid: payment.fees_paid_msat,
                        payment_method: "bolt11".to_string(),
                    })),
                });
            }

            if let Some(offer) = uri.lno.first() {
                let payment = ldk_service
                    .pay_offer(offer.as_str(), amount_msat, None)
                    .map_err(|e| map_ldk_service_error("pay_bip321/bolt12", ErrorCode::PaymentFailed, e))?;
                return Ok(Response {
                    result_type: Method::PayBip321,
                    error: None,
                    result: Some(ResponseResult::PayBip321(PayBip321Response {
                        preimage: Some(payment.preimage),
                        txid: None,
                        fees_paid: payment.fees_paid_msat,
                        payment_method: "bolt12".to_string(),
                    })),
                });
            }

            if uri.address.is_some() {
                let amount_sat = uri.amount
                    .map(|a| a.to_sat())
                    .ok_or_else(|| NIP47Error {
                        code: ErrorCode::Other,
                        message: "on-chain payment requires amount".to_string(),
                    })?;
                let addr_str = uri.address_str().ok_or_else(|| NIP47Error {
                    code: ErrorCode::Other,
                    message: "missing address string".to_string(),
                })?;
                let txid = ldk_service
                    .pay_onchain(addr_str, amount_sat, None)
                    .map_err(|e| map_ldk_service_error("pay_bip321/onchain", ErrorCode::PaymentFailed, e))?;
                return Ok(Response {
                    result_type: Method::PayBip321,
                    error: None,
                    result: Some(ResponseResult::PayBip321(PayBip321Response {
                        preimage: None,
                        txid: Some(txid),
                        fees_paid: None,
                        payment_method: "onchain".to_string(),
                    })),
                });
            }

            return Err(NIP47Error {
                code: ErrorCode::Other,
                message: "no supported payment method in URI".to_string(),
            });
        }

        Err(NIP47Error {
            code: ErrorCode::Other,
            message: "invalid params for pay_bip321".to_string(),
        })
    }
}

struct MakeBip321Handler;

impl Handler for MakeBip321Handler {
    fn validate(&self, req: &Request) -> Result<(), NIP47Error> {
        if let RequestParams::MakeBip321(_) = &req.params {
            return Ok(());
        }
        Err(NIP47Error {
            code: ErrorCode::Other,
            message: "invalid params for make_bip321".to_string(),
        })
    }

    fn execute(&self, req: &Request, _caller_pubkey: &str) -> Result<Response, NIP47Error> {
        let ldk_service = get_ldk_service().ok_or_else(|| NIP47Error {
            code: ErrorCode::Other,
            message: "ldk service unavailable".to_string(),
        })?;

        if let RequestParams::MakeBip321(params) = &req.params {
            let mut uri: bip321::Uri<'_> = bip321::Uri::new();

            // Set amount (msats → BTC)
            if let Some(amount_msat) = params.amount {
                uri.amount = Some(ldk_node::bitcoin::Amount::from_sat(amount_msat / 1000));
            }

            // Set label and message
            if let Some(ref label) = params.label {
                uri.label = Some(bip321::Param::from_decoded(label.to_string()));
            }
            if let Some(ref message) = params.message {
                uri.message = Some(bip321::Param::from_decoded(message.to_string()));
            }

            // Resolve method list: use params.methods or default to all three
            let default_methods = vec![
                Bip321MethodEntry { method: "bolt11".into(), expiry: None, address_type: None },
                Bip321MethodEntry { method: "bolt12".into(), expiry: None, address_type: None },
                Bip321MethodEntry { method: "onchain".into(), expiry: None, address_type: None },
            ];
            let methods = params.methods.as_deref().unwrap_or(&default_methods);

            let mut generated_any = false;

            for entry in methods {
                match entry.method.as_str() {
                    "bolt11" => {
                        // bolt11 requires an amount
                        if let Some(amount_msat) = params.amount {
                            let expiry = entry.expiry;
                            let desc = params.message.as_deref();
                            match ldk_service.make_invoice(amount_msat, desc, None, expiry) {
                                Ok(inv) => {
                                    uri.lightning.push(bip321::Param::from_decoded(inv.invoice));
                                    generated_any = true;
                                }
                                Err(e) => {
                                    eprintln!("make_bip321: bolt11 skipped: {e}");
                                }
                            }
                        }
                    }
                    "bolt12" => {
                        let amount = params.amount.unwrap_or(0);
                        let desc = params.message.as_deref().unwrap_or("bip321 offer");
                        let expiry = entry.expiry.and_then(|e| u32::try_from(e).ok());
                        match ldk_service.make_offer(amount, desc, expiry) {
                            Ok(offer_str) => {
                                let offer_id = Offer::from_str(&offer_str)
                                    .map(|o| o.id().0)
                                    .unwrap_or([0u8; 32]);
                                offer_store::insert_offer(
                                    offer_str.clone(),
                                    desc.to_string(),
                                    amount,
                                    offer_id,
                                );
                                uri.lno.push(bip321::Param::from_decoded(offer_str));
                                generated_any = true;
                            }
                            Err(e) => {
                                eprintln!("make_bip321: bolt12 skipped: {e}");
                            }
                        }
                    }
                    "onchain" => {
                        // LDK only supports p2tr addresses; reject if caller
                        // explicitly asks for a different type.
                        if let Some(ref at) = entry.address_type {
                            if at != "p2tr" {
                                eprintln!("make_bip321: onchain skipped: unsupported address_type '{at}'");
                                continue;
                            }
                        }
                        match ldk_service.new_onchain_address() {
                            Ok(addr_str) => {
                                address_store::register_address(addr_str.clone());
                                if let Ok(parsed) = addr_str.parse() {
                                    uri.set_address(addr_str, parsed);
                                    generated_any = true;
                                }
                            }
                            Err(e) => {
                                eprintln!("make_bip321: onchain skipped: {e}");
                            }
                        }
                    }
                    other => {
                        eprintln!("make_bip321: unsupported method '{other}', skipped");
                    }
                }
            }

            if !generated_any {
                return Err(NIP47Error {
                    code: ErrorCode::Other,
                    message: "failed to generate any payment instruction".to_string(),
                });
            }

            return Ok(Response {
                result_type: Method::MakeBip321,
                error: None,
                result: Some(ResponseResult::MakeBip321(MakeBip321Response {
                    uri: format!("{uri}"),
                })),
            });
        }

        Err(NIP47Error {
            code: ErrorCode::Other,
            message: "invalid params for make_bip321".to_string(),
        })
    }
}

struct EstimateOnchainFeesHandler;

impl Handler for EstimateOnchainFeesHandler {
    fn validate(&self, req: &Request) -> Result<(), NIP47Error> {
        if req.params != RequestParams::EstimateOnchainFees {
            return Err(NIP47Error {
                code: ErrorCode::Other,
                message: "invalid params for estimate_onchain_fees".to_string(),
            });
        }
        Ok(())
    }

    fn execute(&self, _req: &Request, _caller_pubkey: &str) -> Result<Response, NIP47Error> {
        // TODO: implement actual on-chain fee estimation via LDK/esplora
        Err(NIP47Error {
            code: ErrorCode::NotImplemented,
            message: "estimate_onchain_fees not yet implemented".to_string(),
        })
    }
}

struct EstimateRoutingFeesHandler;

impl Handler for EstimateRoutingFeesHandler {
    fn validate(&self, req: &Request) -> Result<(), NIP47Error> {
        if let RequestParams::EstimateRoutingFees(ref params) = req.params {
            if params.destination.is_empty() || params.amount == 0 {
                return Err(NIP47Error {
                    code: ErrorCode::Other,
                    message: "destination and amount required".to_string(),
                });
            }
            Ok(())
        } else {
            Err(NIP47Error {
                code: ErrorCode::Other,
                message: "invalid params for estimate_routing_fees".to_string(),
            })
        }
    }

    fn execute(&self, req: &Request, _caller_pubkey: &str) -> Result<Response, NIP47Error> {
        if let RequestParams::EstimateRoutingFees(ref params) = req.params {
            let routes = if let Some(ldk_service) = get_ldk_service() {
                ldk_service.find_routes(&params.destination, params.amount, 1)
            } else {
                Vec::new()
            };

            if routes.is_empty() {
                return Err(NIP47Error {
                    code: ErrorCode::Other,
                    message: "no route found to destination".to_string(),
                });
            }

            let route = &routes[0];
            Ok(Response {
                result_type: Method::EstimateRoutingFees,
                error: None,
                result: Some(ResponseResult::EstimateRoutingFees(
                    EstimateRoutingFeesResponse {
                        fee: route.total_fee,
                        time_lock_delay: route.total_time_lock,
                    },
                )),
            })
        } else {
            Err(NIP47Error {
                code: ErrorCode::Other,
                message: "invalid params for estimate_routing_fees".to_string(),
            })
        }
    }
}

// Lazily initialize a static handler map to avoid rebuilding it per request.
fn request_handlers() -> &'static HashMap<Method, Box<dyn Handler + Send + Sync>> {
    static HANDLERS: OnceLock<HashMap<Method, Box<dyn Handler + Send + Sync>>> = OnceLock::new();

    HANDLERS.get_or_init(|| {
        let mut handlers: HashMap<Method, Box<dyn Handler + Send + Sync>> = HashMap::new();
        handlers.insert(Method::GetInfo, Box::new(GetInfoHandler));
        handlers.insert(Method::GetBalance, Box::new(GetBalanceHandler));
        handlers.insert(Method::PayInvoice, Box::new(PayInvoiceHandler));
        handlers.insert(Method::PayKeysend, Box::new(PayKeysendHandler));
        handlers.insert(Method::MakeInvoice, Box::new(MakeInvoiceHandler));
        handlers.insert(Method::LookupInvoice, Box::new(LookupInvoiceHandler));
        handlers.insert(Method::ListTransactions, Box::new(ListTransactionsHandler));
        handlers.insert(Method::MakeHoldInvoice, Box::new(MakeHoldInvoiceHandler));
        handlers.insert(
            Method::CancelHoldInvoice,
            Box::new(CancelHoldInvoiceHandler),
        );
        handlers.insert(
            Method::SettleHoldInvoice,
            Box::new(SettleHoldInvoiceHandler),
        );
        handlers.insert(Method::MakeNewAddress, Box::new(MakeNewAddressHandler));
        handlers.insert(Method::PayOnchain, Box::new(PayOnchainHandler));
        handlers.insert(Method::MakeOffer, Box::new(MakeOfferHandler));
        handlers.insert(Method::PayOffer, Box::new(PayOfferHandler));
        handlers.insert(Method::LookupOffer, Box::new(LookupOfferHandler));
        handlers.insert(Method::LookupAddress, Box::new(LookupAddressHandler));
        handlers.insert(Method::ListInvoices, Box::new(ListInvoicesHandler));
        handlers.insert(Method::ListOffers, Box::new(ListOffersHandler));
        handlers.insert(Method::DisableOffer, Box::new(DisableOfferHandler));
        handlers.insert(Method::ListAddresses, Box::new(ListAddressesHandler));
        handlers.insert(Method::PayBip321, Box::new(PayBip321Handler));
        handlers.insert(Method::MakeBip321, Box::new(MakeBip321Handler));
        handlers.insert(
            Method::EstimateOnchainFees,
            Box::new(EstimateOnchainFeesHandler),
        );
        handlers.insert(
            Method::EstimateRoutingFees,
            Box::new(EstimateRoutingFeesHandler),
        );
        handlers
    })
}

/// Known NWC notification types for subscribe_notifications validation.
const KNOWN_NWC_NOTIFICATION_TYPES: &[&str] = &[
    "payment_received",
    "payment_sent",
    "hold_invoice_accepted",
];

/// Known NNC notification types for subscribe_notifications validation.
const KNOWN_NNC_NOTIFICATION_TYPES: &[&str] = &[
    "channel_opened",
    "channel_closed",
];

async fn process_nwc_request(request: Request, event: &Event) -> Response {
    // Check that the user is authorized
    let _applied_state_updates = match verify_access(&request, event) {
        Ok(state_updates) => state_updates,
        Err(response) => return response,
    };

    // Handle subscribe_notifications specially since it needs the caller pubkey
    // for subscription management and doesn't follow the normal handler pattern.
    if request.method == Method::SubscribeNotifications {
        return handle_nwc_subscribe_notifications(&request, event);
    }

    // Check that we support the requested method
    if !request_handlers().contains_key(&request.method) {
        return Response {
            result_type: request.method.clone(),
            error: Some(NIP47Error {
                code: ErrorCode::NotImplemented,
                message: format!("{} not implemented yet", request.method.as_str()),
            }),
            result: None,
        };
    }

    // Select a handler
    let handler = request_handlers().get(&request.method).unwrap();

    // Validate the request
    if let Err(e) = handler.validate(&request) {
        return Response {
            result_type: request.method.clone(),
            error: Some(e),
            result: None,
        };
    }

    let execution_result = if should_force_execute_failure(&request.method) {
        Err(NIP47Error {
            code: ErrorCode::Other,
            message: "forced execute failure for testing".to_string(),
        })
    } else {
        let request_for_exec = request.clone();
        let caller_pubkey = event.pubkey.to_string();
        let handler = &**handler;
        match tokio::task::spawn_blocking(move || {
            handler.execute(&request_for_exec, &caller_pubkey)
        })
        .await
        {
            Ok(result) => result,
            Err(e) => Err(NIP47Error {
                code: ErrorCode::Other,
                message: format!("internal execution task failed: {e}"),
            }),
        }
    };

    // Execute the request.
    match execution_result {
        Ok(response) => response,
        Err(e) => {
            refund_applied_state_updates(&request.method, &_applied_state_updates);
            Response {
                result_type: request.method.clone(),
                error: Some(e),
                result: None,
            }
        }
    }
}

fn allowed_methods_for(caller_pubkey: &str) -> Vec<Method> {
    let profile = get_usage_profile(caller_pubkey);

    let Some(profile) = profile else {
        return Vec::new();
    };

    match profile.methods {
        None => SUPPORTED_METHODS.to_vec(),
        Some(methods) => {
            if methods.is_empty() {
                Vec::new()
            } else if methods.contains_key(&Method::Unknown("ALL".to_string())) {
                SUPPORTED_METHODS.to_vec()
            } else {
                SUPPORTED_METHODS
                    .iter()
                    .filter(|m| methods.contains_key(m))
                    .cloned()
                    .collect()
            }
        }
    }
}

// ── NIP-44 encryption helpers ────────────────────────────────────────────────

/// Extract the encryption scheme from an event's `["encryption", ...]` tag.
/// Returns `"nip04"` when no encryption tag is present (backward compat).
fn get_encryption_scheme(event: &Event) -> &str {
    event.tags.iter().find_map(|tag| {
        let parts = tag.as_slice();
        if parts.first().map(|v| v.as_str()) == Some("encryption") {
            parts.get(1).map(|v| v.as_str())
        } else {
            None
        }
    }).unwrap_or("nip04")
}

fn decrypt_content(
    secret_key: &SecretKey,
    pubkey: &PublicKey,
    content: &str,
    scheme: &str,
) -> std::result::Result<String, Box<dyn std::error::Error + Send + Sync>> {
    match scheme {
        "nip44_v2" => Ok(nip44::decrypt(secret_key, pubkey, content)?),
        "nip04" => Ok(nip04::decrypt(secret_key, pubkey, content)?),
        other => Err(format!("unsupported encryption: {other}").into()),
    }
}

fn encrypt_content(
    secret_key: &SecretKey,
    pubkey: &PublicKey,
    content: &str,
    scheme: &str,
) -> std::result::Result<String, Box<dyn std::error::Error + Send + Sync>> {
    match scheme {
        "nip44_v2" => Ok(nip44::encrypt(secret_key, pubkey, content, nip44::Version::V2)?),
        "nip04" => Ok(nip04::encrypt(secret_key, pubkey, content)?),
        other => Err(format!("unsupported encryption: {other}").into()),
    }
}

async fn handle_nwc_request(
    client: &Client,
    keys: &Keys,
    event: &Event,
) -> std::result::Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let sender_pubkey = event.pubkey;
    let scheme = get_encryption_scheme(event);

    // Return UNSUPPORTED_ENCRYPTION for unknown schemes
    if scheme != "nip04" && scheme != "nip44_v2" {
        let error_response = Response {
            result_type: Method::GetInfo,
            error: Some(NIP47Error {
                code: ErrorCode::UnsupportedEncryption,
                message: format!("unsupported encryption: {scheme}"),
            }),
            result: None,
        };
        // Fall back to NIP-04 for the error response since we can't use the unknown scheme
        let encrypted = nip04::encrypt(
            keys.secret_key(),
            &sender_pubkey,
            error_response.as_json(),
        )?;
        let response_event = EventBuilder::new(Kind::WalletConnectResponse, encrypted)
            .tag(Tag::public_key(sender_pubkey))
            .tag(Tag::event(event.id));
        client.send_event_builder(response_event).await?;
        return Ok(());
    }

    let decrypted = decrypt_content(keys.secret_key(), &sender_pubkey, &event.content, scheme)?;

    let request = Request::from_json(&decrypted)?;

    let response = process_nwc_request(request, event).await;

    // Encrypt the response using the same scheme the client used
    let response_json = response.as_json();
    let encrypted = encrypt_content(keys.secret_key(), &sender_pubkey, &response_json, scheme)?;

    // Build and send the response event (Kind 23195)
    let mut response_event = EventBuilder::new(Kind::WalletConnectResponse, encrypted)
        .tag(Tag::public_key(sender_pubkey))
        .tag(Tag::event(event.id));
    if scheme != "nip04" {
        response_event = response_event.tag(Tag::parse(["encryption", scheme]).unwrap());
    }

    client.send_event_builder(response_event).await?;

    Ok(())
}

fn control_error(code: &str, message: String) -> ControlError {
    ControlError {
        code: code.to_string(),
        message,
    }
}

fn authorize_control_method(caller_pubkey: &str, method: &str) -> Result<(), ControlError> {
    let profile = get_usage_profile(caller_pubkey)
        .ok_or_else(|| control_error("UNAUTHORIZED", "unauthorized".to_string()))?;

    let Some(control_methods) = profile.control else {
        return Err(control_error(
            "RESTRICTED",
            "control access denied, missing control permissions".to_string(),
        ));
    };

    if control_methods.is_empty()
        || (!control_methods.contains_key(method) && !control_methods.contains_key("ALL"))
    {
        return Err(control_error(
            "RESTRICTED",
            "control access denied, insufficient permission".to_string(),
        ));
    }

    Ok(())
}

fn process_control_request(request: ControlRequest, caller_pubkey: &str) -> ControlResponse {
    let method = request.method.clone();

    if let Err(error) = authorize_control_method(caller_pubkey, &request.method) {
        return ControlResponse {
            result_type: method,
            result: None,
            error: Some(error),
        };
    }

    if !SUPPORTED_CONTROL_METHODS
        .iter()
        .any(|method| *method == request.method)
    {
        return ControlResponse {
            result_type: request.method.clone(),
            result: None,
            error: Some(control_error(
                "NOT_IMPLEMENTED",
                format!("unknown control method: {}", request.method),
            )),
        };
    }

    if request.method == "open_channel" {
        let params = match serde_json::from_value::<OpenChannelParams>(request.params.clone()) {
            Ok(params) => params,
            Err(e) => {
                return ControlResponse {
                    result_type: request.method,
                    result: None,
                    error: Some(control_error(
                        "OTHER",
                        format!("invalid open_channel params: {e}"),
                    )),
                };
            }
        };
        if params.amount == 0 {
            return ControlResponse {
                result_type: "open_channel".to_string(),
                result: None,
                error: Some(control_error(
                    "OTHER",
                    "amount must be greater than 0".to_string(),
                )),
            };
        }
        let Some(ldk_service) = get_ldk_service() else {
            return ControlResponse {
                result_type: "open_channel".to_string(),
                result: None,
                error: Some(control_error(
                    "OTHER",
                    "ldk service unavailable".to_string(),
                )),
            };
        };

        let push_msat = params.push_amount.map(|sats| sats * 1000);
        let address = if let Some(ref host) = params.host {
            host.clone()
        } else {
            // Look up address from connected peers
            match ldk_service
                .list_peers()
                .into_iter()
                .find(|p| p.pubkey == params.pubkey && p.connected)
            {
                Some(peer) => peer.address,
                None => {
                    return ControlResponse {
                        result_type: "open_channel".to_string(),
                        result: None,
                        error: Some(control_error(
                            "OTHER",
                            "peer not connected and no host provided".to_string(),
                        )),
                    };
                }
            }
        };
        if let Err(e) = ldk_service.open_channel(
            &params.pubkey,
            &address,
            params.amount,
            push_msat,
        ) {
            return ControlResponse {
                result_type: "open_channel".to_string(),
                result: None,
                error: Some(control_error(
                    "OTHER",
                    format!("open_channel failed: {e}"),
                )),
            };
        }

        return ControlResponse {
            result_type: "open_channel".to_string(),
            result: Some(json!({})),
            error: None,
        };
    }

    if request.method == "connect_peer" {
        let params = match serde_json::from_value::<ConnectPeerParams>(request.params.clone()) {
            Ok(params) => params,
            Err(e) => {
                return ControlResponse {
                    result_type: request.method,
                    result: None,
                    error: Some(control_error(
                        "OTHER",
                        format!("invalid connect_peer params: {e}"),
                    )),
                };
            }
        };
        let Some(ldk_service) = get_ldk_service() else {
            return ControlResponse {
                result_type: "connect_peer".to_string(),
                result: None,
                error: Some(control_error(
                    "OTHER",
                    "ldk service unavailable".to_string(),
                )),
            };
        };
        if let Err(e) = ldk_service.connect_peer(&params.pubkey, &params.host) {
            return ControlResponse {
                result_type: "connect_peer".to_string(),
                result: None,
                error: Some(control_error(
                    "OTHER",
                    format!("connect_peer failed: {e}"),
                )),
            };
        }
        return ControlResponse {
            result_type: "connect_peer".to_string(),
            result: Some(json!({ "status": "connected" })),
            error: None,
        };
    }

    if request.method == "disconnect_peer" {
        let params = match serde_json::from_value::<DisconnectPeerParams>(request.params.clone()) {
            Ok(params) => params,
            Err(e) => {
                return ControlResponse {
                    result_type: request.method,
                    result: None,
                    error: Some(control_error(
                        "OTHER",
                        format!("invalid disconnect_peer params: {e}"),
                    )),
                };
            }
        };
        let Some(ldk_service) = get_ldk_service() else {
            return ControlResponse {
                result_type: "disconnect_peer".to_string(),
                result: None,
                error: Some(control_error(
                    "OTHER",
                    "ldk service unavailable".to_string(),
                )),
            };
        };
        if let Err(e) = ldk_service.disconnect_peer(&params.pubkey) {
            return ControlResponse {
                result_type: "disconnect_peer".to_string(),
                result: None,
                error: Some(control_error(
                    "OTHER",
                    format!("disconnect_peer failed: {e}"),
                )),
            };
        }
        return ControlResponse {
            result_type: "disconnect_peer".to_string(),
            result: Some(json!({ "status": "disconnected" })),
            error: None,
        };
    }

    if request.method == "list_channels" {
        let channels = if let Some(ldk_service) = get_ldk_service() {
            ldk_service.list_channels()
        } else {
            Vec::new()
        };
        return ControlResponse {
            result_type: request.method,
            result: Some(json!({ "channels": channels })),
            error: None,
        };
    }

    if request.method == "list_peers" {
        let peers = if let Some(ldk_service) = get_ldk_service() {
            ldk_service.list_peers()
        } else {
            Vec::new()
        };
        return ControlResponse {
            result_type: request.method,
            result: Some(json!({ "peers": peers })),
            error: None,
        };
    }

    if request.method == "close_channel" {
        let params = match serde_json::from_value::<CloseChannelParams>(request.params.clone()) {
            Ok(params) => params,
            Err(e) => {
                return ControlResponse {
                    result_type: request.method,
                    result: None,
                    error: Some(control_error(
                        "OTHER",
                        format!("invalid close_channel params: {e}"),
                    )),
                };
            }
        };
        let Some(ldk_service) = get_ldk_service() else {
            return ControlResponse {
                result_type: "close_channel".to_string(),
                result: None,
                error: Some(control_error(
                    "OTHER",
                    "ldk service unavailable".to_string(),
                )),
            };
        };
        if let Err(e) = ldk_service.close_channel(&params.id, params.force) {
            return ControlResponse {
                result_type: "close_channel".to_string(),
                result: None,
                error: Some(control_error(
                    "OTHER",
                    format!("close_channel failed: {e}"),
                )),
            };
        }
        return ControlResponse {
            result_type: "close_channel".to_string(),
            result: Some(json!({})),
            error: None,
        };
    }

    if request.method == "get_channel_fees" {
        #[derive(Deserialize)]
        struct GetChannelFeesParams {
            #[serde(default)]
            id: Option<String>,
        }
        let params = match serde_json::from_value::<GetChannelFeesParams>(request.params.clone()) {
            Ok(params) => params,
            Err(e) => {
                return ControlResponse {
                    result_type: request.method,
                    result: None,
                    error: Some(control_error(
                        "OTHER",
                        format!("invalid get_channel_fees params: {e}"),
                    )),
                };
            }
        };
        let fees = if let Some(ldk_service) = get_ldk_service() {
            ldk_service.get_channel_fees(params.id.as_deref())
        } else {
            Vec::new()
        };
        return ControlResponse {
            result_type: "get_channel_fees".to_string(),
            result: Some(json!({ "fees": fees })),
            error: None,
        };
    }

    if request.method == "set_channel_fees" {
        #[derive(Deserialize)]
        struct SetChannelFeesParams {
            id: String,
            #[serde(default)]
            #[serde(alias = "base_fee_msat")]
            base_fee: Option<u32>,
            #[serde(default)]
            fee_rate: Option<u32>,
        }
        let params = match serde_json::from_value::<SetChannelFeesParams>(request.params.clone()) {
            Ok(params) => params,
            Err(e) => {
                return ControlResponse {
                    result_type: request.method,
                    result: None,
                    error: Some(control_error(
                        "OTHER",
                        format!("invalid set_channel_fees params: {e}"),
                    )),
                };
            }
        };
        let Some(ldk_service) = get_ldk_service() else {
            return ControlResponse {
                result_type: "set_channel_fees".to_string(),
                result: None,
                error: Some(control_error(
                    "OTHER",
                    "ldk service unavailable".to_string(),
                )),
            };
        };
        if let Err(e) = ldk_service.set_channel_fees(
            &params.id,
            params.base_fee,
            params.fee_rate,
        ) {
            return ControlResponse {
                result_type: "set_channel_fees".to_string(),
                result: None,
                error: Some(control_error(
                    "OTHER",
                    format!("set_channel_fees failed: {e}"),
                )),
            };
        }
        return ControlResponse {
            result_type: "set_channel_fees".to_string(),
            result: Some(json!({})),
            error: None,
        };
    }

    if request.method == "list_network_nodes" {
        #[derive(Deserialize)]
        struct ListNetworkNodesParams {
            #[serde(default = "default_limit")]
            limit: usize,
            #[serde(default)]
            offset: usize,
        }
        fn default_limit() -> usize { 100 }
        let params = match serde_json::from_value::<ListNetworkNodesParams>(request.params.clone()) {
            Ok(params) => params,
            Err(e) => {
                return ControlResponse {
                    result_type: request.method,
                    result: None,
                    error: Some(control_error(
                        "OTHER",
                        format!("invalid list_network_nodes params: {e}"),
                    )),
                };
            }
        };
        let nodes = if let Some(ldk_service) = get_ldk_service() {
            ldk_service.list_network_nodes(params.limit, params.offset)
        } else {
            Vec::new()
        };
        return ControlResponse {
            result_type: "list_network_nodes".to_string(),
            result: Some(json!({ "nodes": nodes })),
            error: None,
        };
    }

    if request.method == "get_network_node" {
        #[derive(Deserialize)]
        struct GetNetworkNodeParams {
            pubkey: String,
        }
        let params = match serde_json::from_value::<GetNetworkNodeParams>(request.params.clone()) {
            Ok(params) => params,
            Err(e) => {
                return ControlResponse {
                    result_type: request.method,
                    result: None,
                    error: Some(control_error(
                        "OTHER",
                        format!("invalid get_network_node params: {e}"),
                    )),
                };
            }
        };
        let Some(ldk_service) = get_ldk_service() else {
            return ControlResponse {
                result_type: "get_network_node".to_string(),
                result: None,
                error: Some(control_error(
                    "OTHER",
                    "ldk service unavailable".to_string(),
                )),
            };
        };
        match ldk_service.get_network_node(&params.pubkey) {
            Ok(Some(node)) => {
                return ControlResponse {
                    result_type: "get_network_node".to_string(),
                    result: Some(serde_json::to_value(node).unwrap()),
                    error: None,
                };
            }
            Ok(None) => {
                return ControlResponse {
                    result_type: "get_network_node".to_string(),
                    result: None,
                    error: Some(control_error(
                        "NOT_FOUND",
                        format!("node not found: {}", params.pubkey),
                    )),
                };
            }
            Err(e) => {
                return ControlResponse {
                    result_type: "get_network_node".to_string(),
                    result: None,
                    error: Some(control_error(
                        "OTHER",
                        format!("get_network_node failed: {e}"),
                    )),
                };
            }
        }
    }

    if request.method == "get_network_stats" {
        let stats = if let Some(ldk_service) = get_ldk_service() {
            ldk_service.get_network_stats()
        } else {
            crate::lightning::NetworkStats {
                num_nodes: 0,
                num_channels: 0,
                total_capacity: 0,
                avg_channel_size: 0,
                max_channel_size: 0,
            }
        };
        return ControlResponse {
            result_type: "get_network_stats".to_string(),
            result: Some(serde_json::to_value(stats).unwrap()),
            error: None,
        };
    }

    if request.method == "get_network_channel" {
        #[derive(Deserialize)]
        struct GetNetworkChannelParams {
            short_channel_id: String,
        }
        let params = match serde_json::from_value::<GetNetworkChannelParams>(request.params.clone()) {
            Ok(params) => params,
            Err(e) => {
                return ControlResponse {
                    result_type: request.method,
                    result: None,
                    error: Some(control_error(
                        "OTHER",
                        format!("invalid get_network_channel params: {e}"),
                    )),
                };
            }
        };
        let Some(ldk_service) = get_ldk_service() else {
            return ControlResponse {
                result_type: "get_network_channel".to_string(),
                result: None,
                error: Some(control_error(
                    "OTHER",
                    "ldk service unavailable".to_string(),
                )),
            };
        };
        match ldk_service.get_network_channel(&params.short_channel_id) {
            Ok(Some(channel)) => {
                return ControlResponse {
                    result_type: "get_network_channel".to_string(),
                    result: Some(serde_json::to_value(channel).unwrap()),
                    error: None,
                };
            }
            Ok(None) => {
                return ControlResponse {
                    result_type: "get_network_channel".to_string(),
                    result: None,
                    error: Some(control_error(
                        "NOT_FOUND",
                        format!("channel not found: {}", params.short_channel_id),
                    )),
                };
            }
            Err(e) => {
                return ControlResponse {
                    result_type: "get_network_channel".to_string(),
                    result: None,
                    error: Some(control_error(
                        "OTHER",
                        format!("get_network_channel failed: {e}"),
                    )),
                };
            }
        }
    }

    if request.method == "get_forwarding_history" {
        #[derive(Deserialize)]
        struct GetForwardingHistoryParams {
            #[serde(default)]
            from: Option<u64>,
            #[serde(default)]
            until: Option<u64>,
            #[serde(default = "default_fwd_limit")]
            limit: usize,
            #[serde(default)]
            offset: usize,
        }
        fn default_fwd_limit() -> usize { 100 }
        let params = match serde_json::from_value::<GetForwardingHistoryParams>(request.params.clone()) {
            Ok(params) => params,
            Err(e) => {
                return ControlResponse {
                    result_type: request.method,
                    result: None,
                    error: Some(control_error(
                        "OTHER",
                        format!("invalid get_forwarding_history params: {e}"),
                    )),
                };
            }
        };
        let forwards = forwarding_store::get_history(
            params.from,
            params.until,
            params.limit,
            params.offset,
        );
        return ControlResponse {
            result_type: "get_forwarding_history".to_string(),
            result: Some(json!({ "forwards": forwards })),
            error: None,
        };
    }

    if request.method == "subscribe_notifications" {
        #[derive(Deserialize)]
        struct SubscribeParams {
            types: Vec<String>,
        }
        let params = match serde_json::from_value::<SubscribeParams>(request.params.clone()) {
            Ok(params) => params,
            Err(e) => {
                return ControlResponse {
                    result_type: request.method,
                    result: None,
                    error: Some(control_error(
                        "OTHER",
                        format!("invalid subscribe_notifications params: {e}"),
                    )),
                };
            }
        };

        // Validate that all requested types are known NNC notification types
        for t in &params.types {
            if !KNOWN_NNC_NOTIFICATION_TYPES.contains(&t.as_str()) {
                return ControlResponse {
                    result_type: "subscribe_notifications".to_string(),
                    result: None,
                    error: Some(control_error(
                        "OTHER",
                        format!("unknown notification type: {t}"),
                    )),
                };
            }
        }

        if params.types.is_empty() {
            subscription_store::unsubscribe(caller_pubkey);
        } else {
            subscription_store::subscribe(caller_pubkey, &params.types);
        }

        return ControlResponse {
            result_type: "subscribe_notifications".to_string(),
            result: Some(json!({})),
            error: None,
        };
    }

    if request.method == "query_routes" {
        let destination = request
            .params
            .get("destination")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let amount_msat = request
            .params
            .get("amount")
            .and_then(|v| v.as_u64())
            .unwrap_or(0);
        let max_routes = request
            .params
            .get("max_routes")
            .and_then(|v| v.as_u64())
            .unwrap_or(1) as usize;

        if destination.is_empty() || amount_msat == 0 {
            return ControlResponse {
                result_type: request.method,
                result: None,
                error: Some(control_error(
                    "OTHER",
                    "destination and amount required".to_string(),
                )),
            };
        }

        let routes = if let Some(ldk_service) = get_ldk_service() {
            ldk_service.find_routes(destination, amount_msat, max_routes)
        } else {
            Vec::new()
        };

        if routes.is_empty() {
            return ControlResponse {
                result_type: request.method,
                result: None,
                error: Some(control_error(
                    "NOT_FOUND",
                    "no route found to destination".to_string(),
                )),
            };
        }

        return ControlResponse {
            result_type: request.method,
            result: Some(json!({ "routes": routes })),
            error: None,
        };
    }

    ControlResponse {
        result_type: request.method,
        result: None,
        error: Some(control_error(
            "NOT_IMPLEMENTED",
            "control method not implemented yet".to_string(),
        )),
    }
}

fn handle_nwc_subscribe_notifications(request: &Request, event: &Event) -> Response {
    let caller_pubkey = event.pubkey.to_string();

    if let RequestParams::SubscribeNotifications(params) = &request.params {
        // Validate that all requested types are known NWC notification types
        for t in &params.types {
            if !KNOWN_NWC_NOTIFICATION_TYPES.contains(&t.as_str()) {
                return Response {
                    result_type: Method::SubscribeNotifications,
                    error: Some(NIP47Error {
                        code: ErrorCode::Other,
                        message: format!("unknown notification type: {t}"),
                    }),
                    result: None,
                };
            }
        }

        if params.types.is_empty() {
            // Empty types list means unsubscribe
            subscription_store::unsubscribe(&caller_pubkey);
        } else {
            subscription_store::subscribe(&caller_pubkey, &params.types);
        }

        return Response {
            result_type: Method::SubscribeNotifications,
            error: None,
            result: Some(ResponseResult::SubscribeNotifications(
                SubscribeNotificationsResponse {},
            )),
        };
    }

    Response {
        result_type: Method::SubscribeNotifications,
        error: Some(NIP47Error {
            code: ErrorCode::Other,
            message: "invalid params for subscribe_notifications".to_string(),
        }),
        result: None,
    }
}

async fn handle_control_request(
    client: &Client,
    keys: &Keys,
    event: &Event,
) -> std::result::Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let sender_pubkey = event.pubkey;
    let scheme = get_encryption_scheme(event);

    // Return UNSUPPORTED_ENCRYPTION for unknown schemes
    if scheme != "nip04" && scheme != "nip44_v2" {
        let error_resp = ControlResponse {
            result_type: "unknown".to_string(),
            result: None,
            error: Some(control_error(
                "UNSUPPORTED_ENCRYPTION",
                format!("unsupported encryption: {scheme}"),
            )),
        };
        let encrypted = nip04::encrypt(
            keys.secret_key(),
            &sender_pubkey,
            serde_json::to_string(&error_resp)?,
        )?;
        let response_event = EventBuilder::new(Kind::Custom(CONTROL_RESPONSE_KIND), encrypted)
            .tag(Tag::public_key(sender_pubkey))
            .tag(Tag::event(event.id));
        client.send_event_builder(response_event).await?;
        return Ok(());
    }

    let decrypted = decrypt_content(keys.secret_key(), &sender_pubkey, &event.content, scheme)?;

    let response = match serde_json::from_str::<ControlRequest>(&decrypted) {
        Ok(request) => process_control_request(request, &sender_pubkey.to_string()),
        Err(e) => ControlResponse {
            result_type: "unknown".to_string(),
            result: None,
            error: Some(control_error(
                "OTHER",
                format!("invalid control request payload: {e}"),
            )),
        },
    };

    let response_json = serde_json::to_string(&response)?;
    let encrypted = encrypt_content(keys.secret_key(), &sender_pubkey, &response_json, scheme)?;
    let mut response_event = EventBuilder::new(Kind::Custom(CONTROL_RESPONSE_KIND), encrypted)
        .tag(Tag::public_key(sender_pubkey))
        .tag(Tag::event(event.id));
    if scheme != "nip04" {
        response_event = response_event.tag(Tag::parse(["encryption", scheme]).unwrap());
    }
    client.send_event_builder(response_event).await?;
    Ok(())
}

// ── LDK Event Loop ──────────────────────────────────────────────────────────

/// Kind 23196 — NWC wallet notification (NIP-04)
const NWC_NOTIFICATION_KIND_NIP04: u16 = 23196;
/// Kind 23197 — NWC wallet notification (NIP-44)
const NWC_NOTIFICATION_KIND_NIP44: u16 = 23197;
/// Kind 23200 — NNC node control notification
const NNC_NOTIFICATION_KIND: u16 = 23200;

async fn handle_ldk_event(
    client: &Client,
    keys: &Keys,
    ldk_service: &Arc<LdkService>,
    event: &ldk_node::Event,
) -> std::result::Result<(), Box<dyn std::error::Error + Send + Sync>> {
    use ldk_node::Event;

    match event {
        Event::PaymentReceived {
            payment_hash,
            amount_msat,
            ..
        } => {
            let hex_hash = hex_payment_hash(&payment_hash.0);
            let notif = build_payment_received_notification(&hex_hash, *amount_msat);
            let json = notif.as_json();
            publish_nwc_notification(client, keys, "payment_received", &json).await?;

            // Record BOLT-12 offer payments
            if let Ok(details) = ldk_service.lookup_payment_by_hash(&hex_hash) {
                if details.direction == PaymentDirection::Inbound {
                    if let PaymentKind::Bolt12Offer { offer_id, .. } = &details.kind {
                        if let Some(offer_key) = offer_store::find_by_offer_id(&offer_id.0) {
                            offer_store::record_payment(&offer_key, *amount_msat);
                        }
                    }
                }
            }
        }
        Event::PaymentSuccessful {
            payment_hash,
            fee_paid_msat,
            ..
        } => {
            let notif = build_payment_sent_notification(
                &hex_payment_hash(&payment_hash.0),
                fee_paid_msat,
            );
            let json = notif.as_json();
            publish_nwc_notification(client, keys, "payment_sent", &json).await?;
        }
        Event::PaymentClaimable {
            payment_hash,
            claimable_amount_msat,
            claim_deadline,
            ..
        } => {
            let notif = build_hold_invoice_accepted_notification(
                &hex_payment_hash(&payment_hash.0),
                *claimable_amount_msat,
                *claim_deadline,
            );
            let json = notif.as_json();
            publish_nwc_notification(client, keys, "hold_invoice_accepted", &json).await?;
        }
        Event::ChannelReady {
            channel_id,
            user_channel_id,
            counterparty_node_id,
            funding_txo,
            ..
        } => {
            // Notify VLS signer that the channel is ready (CheckOutpoint + LockOutpoint)
            ldk_service.notify_channel_ready(user_channel_id.0);

            let notif = build_channel_opened_notification(
                ldk_service,
                &channel_id.to_string(),
                counterparty_node_id.as_ref(),
                funding_txo.as_ref(),
            );
            let json = notif.as_json();
            publish_nnc_notification(client, keys, "channel_opened", &json).await?;
        }
        Event::ChannelClosed {
            channel_id,
            counterparty_node_id,
            reason,
            ..
        } => {
            let notif = build_channel_closed_notification(
                &channel_id.to_string(),
                counterparty_node_id.as_ref(),
                reason.as_ref(),
            );
            let json = notif.as_json();
            publish_nnc_notification(client, keys, "channel_closed", &json).await?;
        }
        Event::PaymentForwarded {
            prev_channel_id,
            next_channel_id,
            total_fee_earned_msat,
            outbound_amount_forwarded_msat,
            ..
        } => {
            let fee = total_fee_earned_msat.unwrap_or(0);
            let outgoing = outbound_amount_forwarded_msat.unwrap_or(0);
            let incoming = outgoing.saturating_add(fee);
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs();
            forwarding_store::record_forward(forwarding_store::ForwardingEntry {
                incoming_channel_id: prev_channel_id.to_string(),
                outgoing_channel_id: next_channel_id.to_string(),
                incoming_amount: incoming,
                outgoing_amount: outgoing,
                fee_earned: fee,
                settled_at: now,
            });
        }
        _ => {} // Ignore other events for now
    }
    Ok(())
}

// ── Notification builders ───────────────────────────────────────────────────

fn build_payment_received_notification(payment_hash: &str, amount_msat: u64) -> Notification {
    Notification {
        notification_type: NotificationType::PaymentReceived,
        notification: NotificationResult::PaymentReceived(PaymentNotification {
            transaction_type: Some(TransactionType::Incoming),
            state: Some(TransactionState::Settled),
            invoice: String::new(),
            description: None,
            description_hash: None,
            preimage: String::new(),
            payment_hash: payment_hash.to_string(),
            amount: amount_msat,
            fees_paid: 0,
            created_at: Some(Timestamp::now()),
            expires_at: None,
            settled_at: Timestamp::now(),
            metadata: None,
        }),
    }
}

fn build_payment_sent_notification(
    payment_hash: &str,
    fee_paid_msat: &Option<u64>,
) -> Notification {
    Notification {
        notification_type: NotificationType::PaymentSent,
        notification: NotificationResult::PaymentSent(PaymentNotification {
            transaction_type: Some(TransactionType::Outgoing),
            state: Some(TransactionState::Settled),
            invoice: String::new(),
            description: None,
            description_hash: None,
            preimage: String::new(),
            payment_hash: payment_hash.to_string(),
            amount: 0,
            fees_paid: fee_paid_msat.unwrap_or(0),
            created_at: Some(Timestamp::now()),
            expires_at: None,
            settled_at: Timestamp::now(),
            metadata: None,
        }),
    }
}

fn build_hold_invoice_accepted_notification(
    payment_hash: &str,
    claimable_amount_msat: u64,
    claim_deadline: Option<u32>,
) -> Notification {
    use nwc::nostr::nips::nip47::HoldInvoiceAcceptedNotification;
    Notification {
        notification_type: NotificationType::HoldInvoiceAccepted,
        notification: NotificationResult::HoldInvoiceAccepted(HoldInvoiceAcceptedNotification {
            transaction_type: TransactionType::Incoming,
            state: Some(TransactionState::Accepted),
            invoice: String::new(),
            description: None,
            description_hash: None,
            payment_hash: payment_hash.to_string(),
            amount: claimable_amount_msat,
            created_at: Timestamp::now(),
            expires_at: Timestamp::now(),
            settle_deadline: claim_deadline.unwrap_or(0),
            metadata: None,
        }),
    }
}

fn build_channel_opened_notification(
    ldk_service: &Arc<LdkService>,
    channel_id: &str,
    counterparty: Option<&ldk_node::bitcoin::secp256k1::PublicKey>,
    funding_txo: Option<&ldk_node::bitcoin::OutPoint>,
) -> NncNotification {
    // Try to find the channel in the channel list for richer details
    let channels = ldk_service.list_channels();
    let channel = channels.iter().find(|c| c.id == channel_id);

    NncNotification {
        notification_type: NncNotificationType::ChannelOpened,
        notification: NncNotificationResult::ChannelOpened(ChannelOpenedNotification {
            id: channel_id.to_string(),
            short_channel_id: channel.and_then(|c| c.short_channel_id.clone()),
            peer_pubkey: counterparty
                .map(|p| p.to_string())
                .unwrap_or_default(),
            capacity: channel.map(|c| c.capacity).unwrap_or(0),
            local_balance: channel.map(|c| c.local_balance).unwrap_or(0),
            remote_balance: channel.map(|c| c.remote_balance).unwrap_or(0),
            funding_txid: funding_txo
                .map(|txo| txo.txid.to_string())
                .unwrap_or_default(),
            is_private: channel.map(|c| c.is_private).unwrap_or(false),
        }),
    }
}

fn build_channel_closed_notification(
    channel_id: &str,
    counterparty: Option<&ldk_node::bitcoin::secp256k1::PublicKey>,
    reason: Option<&ldk_node::lightning::events::ClosureReason>,
) -> NncNotification {
    let close_type = match reason {
        Some(ldk_node::lightning::events::ClosureReason::CounterpartyForceClosed { .. }) => {
            "force"
        }
        Some(ldk_node::lightning::events::ClosureReason::HolderForceClosed { .. }) => "force",
        Some(ldk_node::lightning::events::ClosureReason::LegacyCooperativeClosure) => {
            "cooperative"
        }
        Some(ldk_node::lightning::events::ClosureReason::LocallyInitiatedCooperativeClosure) => {
            "cooperative"
        }
        Some(ldk_node::lightning::events::ClosureReason::CounterpartyInitiatedCooperativeClosure) => {
            "cooperative"
        }
        _ => "unknown",
    };

    NncNotification {
        notification_type: NncNotificationType::ChannelClosed,
        notification: NncNotificationResult::ChannelClosed(ChannelClosedNotification {
            id: channel_id.to_string(),
            short_channel_id: None,
            peer_pubkey: counterparty
                .map(|p| p.to_string())
                .unwrap_or_default(),
            capacity: 0,
            closing_txid: None,
            close_type: close_type.to_string(),
        }),
    }
}

// ── Notification publishers ─────────────────────────────────────────────────

/// Publish dual NWC notifications: Kind 23196 (NIP-04) + Kind 23197 (NIP-44).
/// Per NIP-47 spec, wallet services supporting both schemes publish both kinds.
async fn publish_nwc_notification(
    client: &Client,
    keys: &Keys,
    notification_type: &str,
    payload: &str,
) -> std::result::Result<(), Box<dyn std::error::Error + Send + Sync>> {
    for sub_pubkey_hex in subscription_store::get_subscribers(notification_type) {
        let sub_pubkey = PublicKey::from_hex(&sub_pubkey_hex)?;

        // Kind 23196 — NIP-04 encrypted (backward compat)
        let encrypted_04 = nip04::encrypt(keys.secret_key(), &sub_pubkey, payload)?;
        let event_04 = EventBuilder::new(Kind::Custom(NWC_NOTIFICATION_KIND_NIP04), encrypted_04)
            .tag(Tag::public_key(sub_pubkey));
        client.send_event_builder(event_04).await?;

        // Kind 23197 — NIP-44 encrypted
        let encrypted_44 = nip44::encrypt(keys.secret_key(), &sub_pubkey, payload, nip44::Version::V2)?;
        let event_44 = EventBuilder::new(Kind::Custom(NWC_NOTIFICATION_KIND_NIP44), encrypted_44)
            .tag(Tag::public_key(sub_pubkey));
        client.send_event_builder(event_44).await?;
    }
    Ok(())
}

/// Publish NNC notification using NIP-44 only (no legacy NIP-04 for NNC).
async fn publish_nnc_notification(
    client: &Client,
    keys: &Keys,
    notification_type: &str,
    payload: &str,
) -> std::result::Result<(), Box<dyn std::error::Error + Send + Sync>> {
    for sub_pubkey_hex in subscription_store::get_subscribers(notification_type) {
        let sub_pubkey = PublicKey::from_hex(&sub_pubkey_hex)?;
        let encrypted = nip44::encrypt(keys.secret_key(), &sub_pubkey, payload, nip44::Version::V2)?;
        let event = EventBuilder::new(Kind::Custom(NNC_NOTIFICATION_KIND), encrypted)
            .tag(Tag::public_key(sub_pubkey));
        client.send_event_builder(event).await?;
    }
    Ok(())
}
