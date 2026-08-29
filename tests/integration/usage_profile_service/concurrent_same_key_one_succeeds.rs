use std::collections::HashMap;
use std::time::Duration;

use dln_node::{
    clear_usage_profiles, set_relay_pubkey, MethodAccessRule, RateLimitRule, UsageProfile,
};
use nostr_sdk::prelude::*;
use nwc::nostr::nips::nip47::{ErrorCode, Method, NostrWalletConnectUri, Request, Response};

use crate::integration_suite::common::{grant_usage_profile, start_relay, test_guard};
use crate::integration_suite::shared_relay_pubkey;

async fn read_n_get_info_responses(
    nwc_client: &Client,
    uri: &NostrWalletConnectUri,
    service_pubkey: PublicKey,
    n: usize,
) -> Vec<Response> {
    let timeout = Duration::from_secs(10);
    let uri_clone = uri.clone();
    tokio::time::timeout(timeout, async {
        let mut out = Vec::with_capacity(n);
        let mut notifications = nwc_client.notifications();
        while out.len() < n {
            if let Some(notification) = notifications.next().await {
                if let ClientNotification::Event { event, .. } = notification {
                    let event = event.as_ref();
                    if event.kind == Kind::WalletConnectResponse && event.pubkey == service_pubkey {
                        out.push(
                            Response::from_event(&uri_clone, event)
                                .expect("failed to decrypt get_info response"),
                        );
                    }
                }
            }
        }
        out
    })
    .await
    .expect("timeout waiting for get_info responses")
}

/// Verifies same-key concurrent access enforces capacity atomically.
///
/// Setup: access-rate `max_capacity = 1_000_000`, `rate_per_micro = 0` and each get_info debit is `1_000_000`.
/// Success condition: when two requests are sent concurrently, exactly one succeeds and exactly one is rate-limited.
#[tokio::test]
async fn concurrent_same_key_one_succeeds() -> Result<()> {
    let _guard = test_guard();
    clear_usage_profiles();

    let (_container, relay_url) = start_relay().await;
    let relay_pubkey = shared_relay_pubkey();
    set_relay_pubkey(relay_pubkey.clone());

    let service_keys = Keys::generate();
    let service_pubkey = service_keys.public_key();
    let _service_client = dln_node::run_nwc_service(service_keys, &relay_url).await?;
    tokio::time::sleep(Duration::from_secs(1)).await;

    let user_keys = Keys::generate();
    let user_pubkey = user_keys.public_key();
    let owner_keys = Keys::generate();

    let mut methods = HashMap::new();
    methods.insert(
        Method::GetInfo,
        MethodAccessRule {
            access_rate: Some(RateLimitRule {
                rate_per_micro: 0,
                max_capacity: 1_000_000,
            }),
        },
    );
    let profile = UsageProfile {
        quota: None,
        methods: Some(methods),
        control: None,
    };
    grant_usage_profile(&owner_keys, &relay_url, relay_pubkey, user_pubkey, &profile).await?;

    let relay = RelayUrl::parse(&relay_url)?;
    let client_secret = user_keys.secret_key().clone();
    let uri = NostrWalletConnectUri::new(service_pubkey, vec![relay], client_secret, None);

    let nwc_client = Client::builder().signer(user_keys).build();
    nwc_client.add_relay(&relay_url).await?;
    nwc_client.connect().await;
    tokio::time::sleep(Duration::from_secs(1)).await;
    nwc_client
        .subscribe(
            Filter::new()
                .kind(Kind::WalletConnectResponse)
                .author(service_pubkey),
        )
        .await?;

    // Send two requests concurrently for the same key.
    let req1 = Request::get_info()
        .to_event(&uri)
        .expect("failed to create first get_info request");
    let req2 = Request::get_info()
        .to_event(&uri)
        .expect("failed to create second get_info request");

    let c1 = nwc_client.clone();
    let c2 = nwc_client.clone();
    let (r1, r2) = tokio::join!(c1.send_event(&req1), c2.send_event(&req2));
    r1.expect("failed to publish first get_info request");
    r2.expect("failed to publish second get_info request");

    let responses = read_n_get_info_responses(&nwc_client, &uri, service_pubkey, 2).await;
    let success_count = responses.iter().filter(|r| r.error.is_none()).count();
    let rate_limited_count = responses
        .iter()
        .filter(|r| {
            r.error
                .as_ref()
                .map(|e| e.code == ErrorCode::RateLimited)
                .unwrap_or(false)
        })
        .count();

    assert_eq!(
        success_count, 1,
        "expected exactly one success, got responses: {:?}",
        responses.iter().map(|r| &r.error).collect::<Vec<_>>()
    );
    assert_eq!(
        rate_limited_count, 1,
        "expected exactly one rate-limited error, got responses: {:?}",
        responses.iter().map(|r| &r.error).collect::<Vec<_>>()
    );

    Ok(())
}
