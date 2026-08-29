use std::collections::HashMap;
use std::time::Duration;

use dln_node::{
    run_nwc_service, MethodAccessRule, UsageProfile, CONTROL_REQUEST_KIND, CONTROL_RESPONSE_KIND,
};
use nostr_sdk::prelude::*;
use nwc::nostr::nips::nip04;
use serde_json::{json, Value};

#[path = "common/mod.rs"]
mod common;

async fn read_control_response_event(client: &Client, service_pubkey: PublicKey) -> Event {
    let timeout = Duration::from_secs(10);
    let maybe_event = tokio::time::timeout(timeout, async {
        let mut notifications = client.notifications();
        while let Some(notification) = notifications.next().await {
            if let ClientNotification::Event { event, .. } = notification {
                let event = event.as_ref();
                if event.kind == Kind::Custom(CONTROL_RESPONSE_KIND) && event.pubkey == service_pubkey
                {
                    return Some(event.clone());
                }
            }
        }
        None
    })
    .await
    .expect("timeout waiting for control response");
    if let Some(event) = maybe_event {
        return event;
    }
    panic!("notification stream ended before control response")
}

async fn send_control_request(
    controller: &Client,
    controller_secret: &SecretKey,
    service_pubkey: PublicKey,
    payload: Value,
) -> Result<Value> {
    let encrypted = nip04::encrypt(controller_secret, &service_pubkey, payload.to_string())?;
    let request_event = EventBuilder::new(Kind::Custom(CONTROL_REQUEST_KIND), encrypted)
        .tag(Tag::public_key(service_pubkey));
    controller.send_event_builder(request_event).await?;

    let response_event = read_control_response_event(controller, service_pubkey).await;
    let decrypted = nip04::decrypt(controller_secret, &service_pubkey, &response_event.content)?;
    let response: Value = serde_json::from_str(&decrypted)?;
    Ok(response)
}

async fn setup_service_and_controller() -> Result<(
    testcontainers::ContainerAsync<testcontainers::GenericImage>,
    String,
    PublicKey,
    Client,
    SecretKey,
    PublicKey,
)> {
    let (relay_container, relay_url) = common::start_relay().await;

    let service_keys = Keys::generate();
    let service_pubkey = service_keys.public_key();
    let _service_client = run_nwc_service(service_keys, &relay_url).await?;

    tokio::time::sleep(Duration::from_secs(1)).await;

    let controller_keys = Keys::generate();
    let controller_secret = controller_keys.secret_key().clone();
    let controller_pubkey = controller_keys.public_key();

    let controller = Client::builder().signer(controller_keys).build();
    controller.add_relay(&relay_url).await?;
    controller.connect().await;
    tokio::time::sleep(Duration::from_secs(1)).await;

    controller
        .subscribe(
            Filter::new()
                .kind(Kind::Custom(CONTROL_RESPONSE_KIND))
                .author(service_pubkey),
        )
        .await?;

    Ok((
        relay_container,
        relay_url,
        service_pubkey,
        controller,
        controller_secret,
        controller_pubkey,
    ))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn control_denied_when_control_missing() -> Result<()> {
    let _guard = common::test_guard();
    let (_relay_container, relay_url, service_pubkey, controller, controller_secret, controller_pubkey) =
        setup_service_and_controller().await?;

    let owner_keys = Keys::generate();
    let relay_pubkey = Keys::generate().public_key();
    let profile = UsageProfile {
        quota: None,
        methods: None,
        control: None,
    };
    common::grant_usage_profile(
        &owner_keys,
        &relay_url,
        relay_pubkey,
        controller_pubkey,
        &profile,
    )
    .await?;

    tokio::time::sleep(Duration::from_secs(1)).await;

    let response = send_control_request(
        &controller,
        &controller_secret,
        service_pubkey,
        json!({
            "method": "list_channels",
            "params": {}
        }),
    )
    .await?;

    assert_eq!(response["result_type"], "list_channels");
    assert_eq!(response["error"]["code"], "RESTRICTED");
    assert!(
        response["error"]["message"]
            .as_str()
            .unwrap_or_default()
            .contains("missing control permissions"),
        "unexpected message: {:?}",
        response["error"]["message"]
    );

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn control_denied_when_method_not_listed() -> Result<()> {
    let _guard = common::test_guard();
    let (_relay_container, relay_url, service_pubkey, controller, controller_secret, controller_pubkey) =
        setup_service_and_controller().await?;

    let owner_keys = Keys::generate();
    let relay_pubkey = Keys::generate().public_key();

    let mut control = HashMap::new();
    control.insert("connect_peer".to_string(), MethodAccessRule { access_rate: None });

    let profile = UsageProfile {
        quota: None,
        methods: None,
        control: Some(control),
    };
    common::grant_usage_profile(
        &owner_keys,
        &relay_url,
        relay_pubkey,
        controller_pubkey,
        &profile,
    )
    .await?;

    tokio::time::sleep(Duration::from_secs(1)).await;

    let response = send_control_request(
        &controller,
        &controller_secret,
        service_pubkey,
        json!({
            "method": "list_channels",
            "params": {}
        }),
    )
    .await?;

    assert_eq!(response["result_type"], "list_channels");
    assert_eq!(response["error"]["code"], "RESTRICTED");
    assert!(
        response["error"]["message"]
            .as_str()
            .unwrap_or_default()
            .contains("insufficient permission"),
        "unexpected message: {:?}",
        response["error"]["message"]
    );

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn control_allowed_when_method_listed_returns_channels_array() -> Result<()> {
    let _guard = common::test_guard();
    let (_relay_container, relay_url, service_pubkey, controller, controller_secret, controller_pubkey) =
        setup_service_and_controller().await?;

    let owner_keys = Keys::generate();
    let relay_pubkey = Keys::generate().public_key();

    let mut control = HashMap::new();
    control.insert("list_channels".to_string(), MethodAccessRule { access_rate: None });

    let profile = UsageProfile {
        quota: None,
        methods: None,
        control: Some(control),
    };
    common::grant_usage_profile(
        &owner_keys,
        &relay_url,
        relay_pubkey,
        controller_pubkey,
        &profile,
    )
    .await?;

    tokio::time::sleep(Duration::from_secs(1)).await;

    let response = send_control_request(
        &controller,
        &controller_secret,
        service_pubkey,
        json!({
            "method": "list_channels",
            "params": {}
        }),
    )
    .await?;

    assert_eq!(response["result_type"], "list_channels");
    assert!(response["error"].is_null(), "expected no error, got: {:?}", response);
    let channels = &response["result"]["channels"];
    assert!(
        channels.is_array(),
        "expected channels array in result, got: {:?}",
        response["result"]
    );

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn control_allowed_list_peers_returns_array() -> Result<()> {
    let _guard = common::test_guard();
    let (_relay_container, relay_url, service_pubkey, controller, controller_secret, controller_pubkey) =
        setup_service_and_controller().await?;

    let owner_keys = Keys::generate();
    let relay_pubkey = Keys::generate().public_key();

    let mut control = HashMap::new();
    control.insert("list_peers".to_string(), MethodAccessRule { access_rate: None });

    let profile = UsageProfile {
        quota: None,
        methods: None,
        control: Some(control),
    };
    common::grant_usage_profile(
        &owner_keys,
        &relay_url,
        relay_pubkey,
        controller_pubkey,
        &profile,
    )
    .await?;

    tokio::time::sleep(Duration::from_secs(1)).await;

    let response = send_control_request(
        &controller,
        &controller_secret,
        service_pubkey,
        json!({
            "method": "list_peers",
            "params": {}
        }),
    )
    .await?;

    assert_eq!(response["result_type"], "list_peers");
    assert!(response["error"].is_null(), "expected no error, got: {:?}", response);
    let peers = &response["result"]["peers"];
    assert!(
        peers.is_array(),
        "expected peers array in result, got: {:?}",
        response["result"]
    );

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn control_malformed_payload_returns_other() -> Result<()> {
    let _guard = common::test_guard();
    let (_relay_container, _relay_url, service_pubkey, controller, controller_secret, _controller_pubkey) =
        setup_service_and_controller().await?;

    let encrypted = nip04::encrypt(&controller_secret, &service_pubkey, "not-json".to_string())?;
    let request_event = EventBuilder::new(Kind::Custom(CONTROL_REQUEST_KIND), encrypted)
        .tag(Tag::public_key(service_pubkey));
    controller.send_event_builder(request_event).await?;

    let response_event = read_control_response_event(&controller, service_pubkey).await;
    let decrypted = nip04::decrypt(&controller_secret, &service_pubkey, &response_event.content)?;
    let response: Value = serde_json::from_str(&decrypted)?;

    assert_eq!(response["result_type"], "unknown");
    assert_eq!(response["error"]["code"], "OTHER");
    assert!(
        response["error"]["message"]
            .as_str()
            .unwrap_or_default()
            .contains("invalid control request payload"),
        "unexpected message: {:?}",
        response["error"]["message"]
    );

    Ok(())
}
