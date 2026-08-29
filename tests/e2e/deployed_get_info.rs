use nostr_sdk::prelude::*;
use nwc::nostr::nips::nip47::{Method, NostrWalletConnectUri, Request, Response};
use serde_json::json;
use std::str::FromStr;
use std::time::Duration;

fn require_env(name: &str) -> String {
    std::env::var(name).unwrap_or_else(|_| panic!("missing required env var: {name}"))
}

/// Manual deployment verification:
/// sends a real NWC `get_info` request to a deployed relay/service and
/// expects a successful response.
///
/// Required environment variables:
/// - `DEPLOYED_SERVICE_PUBKEY`: Nostr pubkey of the deployed service (hex or npub)
///
/// Optional:
/// - `DEPLOYED_RELAY_URL`: relay websocket URL (default `wss://ldk-cw.flowrate.dev`)
/// - `DEPLOYED_EXPECTED_NETWORK`: expected `get_info.network` (default `signet`)
/// - `DEPLOYED_CLIENT_SECRET`: NWC client secret (hex or nsec). If omitted,
///   an ephemeral key is generated for this test run.
#[tokio::test]
#[ignore = "manual deployment verification; requires real credentials"]
async fn deployed_nwc_get_info_roundtrip() -> Result<()> {
    let relay_url =
        std::env::var("DEPLOYED_RELAY_URL").unwrap_or_else(|_| "wss://ldk-cw.flowrate.dev".into());
    let expected_network =
        std::env::var("DEPLOYED_EXPECTED_NETWORK").unwrap_or_else(|_| "signet".into());

    let service_pubkey = PublicKey::from_str(&require_env("DEPLOYED_SERVICE_PUBKEY"))
        .expect("DEPLOYED_SERVICE_PUBKEY must be a valid nostr pubkey");

    let client_keys = match std::env::var("DEPLOYED_CLIENT_SECRET") {
        Ok(secret) => Keys::parse(&secret)
            .expect("DEPLOYED_CLIENT_SECRET must be a valid nsec/secret"),
        Err(_) => Keys::generate(),
    };
    let client_pubkey = client_keys.public_key();
    let client_secret = client_keys.secret_key().clone();

    let relay = RelayUrl::parse(&relay_url)?;
    let uri = NostrWalletConnectUri::new(service_pubkey, vec![relay], client_secret, None);

    let client = Client::builder().signer(client_keys).build();
    client.add_relay(&relay_url).await?;
    client.connect().await;
    tokio::time::sleep(Duration::from_secs(1)).await;

    // Ensure this client is authorized on the deployed node by publishing
    // a node:user usage grant event.
    let grant = json!({
        "quota": null,
        "methods": null,
        "control": {
            "list_channels": { "access_rate": null },
            "connect_peer": { "access_rate": null },
            "open_channel": { "access_rate": null }
        }
    });
    let d_value = format!("{}:{}", service_pubkey, client_pubkey);
    let grant_event = EventBuilder::new(Kind::Custom(30078), grant.to_string())
        .tag(Tag::parse(["d", d_value.as_str()]).expect("create d tag"))
        .tag(Tag::public_key(service_pubkey));
    client
        .send_event_builder(grant_event)
        .await
        .expect("failed to publish grant event");
    tokio::time::sleep(Duration::from_secs(2)).await;

    client
        .subscribe(
            Filter::new()
                .kind(Kind::WalletConnectResponse)
                .author(service_pubkey),
        )
        .await?;

    let mut notifications = client.notifications();
    let request_event = Request::get_info()
        .to_event(&uri)
        .expect("failed to create get_info request");
    client.send_event(&request_event).await?;
    eprintln!(
        "published get_info request id={} pubkey={}",
        request_event.id, client_pubkey
    );

    let timeout = Duration::from_secs(30);
    let uri_clone = uri.clone();
    let wait_result = tokio::time::timeout(timeout, async {
        let mut seen_response_events = 0usize;
        while let Some(notification) = notifications.next().await {
            if let ClientNotification::Event { event, .. } = notification {
                let event = event.as_ref();
                if event.kind == Kind::WalletConnectResponse && event.pubkey == service_pubkey {
                    seen_response_events += 1;
                    let response = match Response::from_event(&uri_clone, event) {
                        Ok(response) => response,
                        Err(err) => {
                            eprintln!(
                                "saw response event but failed to decrypt/parse (count={}): {}",
                                seen_response_events, err
                            );
                            continue;
                        }
                    };
                    assert!(
                        response.error.is_none(),
                        "get_info should succeed, got: {:?}",
                        response.error
                    );

                    let info = response
                        .to_get_info()
                        .expect("response must decode as get_info");
                    assert_eq!(info.network, Some(expected_network.clone()));
                    assert!(
                        info.methods.contains(&Method::GetInfo),
                        "get_info should be listed in supported methods"
                    );
                    return;
                }
            }
        }
        panic!("notification stream ended before get_info response");
    })
    .await;

    if wait_result.is_err() {
        eprintln!(
            "timeout waiting for get_info response; fetching recent response events for diagnostics"
        );
        let recent_requests = client
            .fetch_events(
                Filter::new()
                    .kind(Kind::WalletConnectRequest)
                    .author(client_pubkey)
                    .limit(20),
            )
            .timeout(Duration::from_secs(8))
            .await?;
        eprintln!("recent WalletConnectRequest count={}", recent_requests.len());
        for event in recent_requests.iter() {
            let tagged_service = event.tags.iter().any(|tag| {
                let parts = tag.as_slice();
                parts.first().map(|v| v.as_str()) == Some("p")
                    && parts.get(1).map(|v| v.as_str()) == Some(service_pubkey.to_hex().as_str())
            });
            eprintln!(
                "request event id={} tagged_service={}",
                event.id, tagged_service
            );
        }

        let recent = client
            .fetch_events(
                Filter::new()
                    .kind(Kind::WalletConnectResponse)
                    .author(service_pubkey)
                    .limit(20),
            )
            .timeout(Duration::from_secs(8))
            .await?;
        eprintln!("recent WalletConnectResponse count={}", recent.len());
        for event in recent.iter() {
            let matches_request = event.tags.iter().any(|tag| {
                let parts = tag.as_slice();
                parts.first().map(|v| v.as_str()) == Some("e")
                    && parts.get(1).map(|v| v.as_str()) == Some(request_event.id.to_hex().as_str())
            });
            eprintln!("response event id={} matches_request={}", event.id, matches_request);
            match Response::from_event(&uri_clone, event) {
                Ok(response) => eprintln!(
                    "decrypted response result_type={:?} error={:?}",
                    response.result_type, response.error
                ),
                Err(err) => eprintln!("decrypt failed for event {}: {}", event.id, err),
            }
        }
        panic!("timed out waiting for get_info response");
    }

    Ok(())
}
