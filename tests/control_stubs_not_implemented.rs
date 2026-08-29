use std::collections::HashMap;
use std::time::Duration;

use dln_node::{
    clear_usage_profiles, run_nwc_service, set_relay_pubkey,
    MethodAccessRule, UsageProfile, CONTROL_REQUEST_KIND, CONTROL_RESPONSE_KIND,
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
                if event.kind == Kind::Custom(CONTROL_RESPONSE_KIND)
                    && event.pubkey == service_pubkey
                {
                    return Some(event.clone());
                }
            }
        }
        None
    })
    .await
    .expect("timeout waiting for control response");
    maybe_event.expect("notification stream ended before control response")
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

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn control_stubs_return_not_implemented() -> Result<()> {
    let _guard = common::test_guard();
    clear_usage_profiles();
    let (_container, relay_url) = common::start_relay().await;

    let relay_pubkey = Keys::generate().public_key();
    set_relay_pubkey(relay_pubkey);

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

    // Grant ALL control access
    let owner_keys = Keys::generate();
    let mut control = HashMap::new();
    control.insert("ALL".to_string(), MethodAccessRule { access_rate: None });
    let profile = UsageProfile {
        quota: None,
        methods: None,
        control: Some(control),
    };
    common::grant_usage_profile(&owner_keys, &relay_url, relay_pubkey, controller_pubkey, &profile)
        .await?;
    tokio::time::sleep(Duration::from_secs(1)).await;

    let stub_methods = ["get_pending_htlcs", "estimate_route_fee", "query_routes"];

    for method in stub_methods {
        let response = send_control_request(
            &controller,
            &controller_secret,
            service_pubkey,
            json!({
                "method": method,
                "params": {}
            }),
        )
        .await?;

        assert_eq!(
            response["result_type"], method,
            "expected result_type={method}, got: {:?}",
            response["result_type"]
        );
        assert_eq!(
            response["error"]["code"], "NOT_IMPLEMENTED",
            "expected NOT_IMPLEMENTED for {method}, got: {:?}",
            response["error"]
        );
    }

    Ok(())
}
