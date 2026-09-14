use nostr_sdk::prelude::*;
use std::sync::{Mutex, OnceLock};
use std::time::Duration;
use testcontainers::{
    core::{IntoContainerPort, WaitFor},
    runners::AsyncRunner,
    GenericImage,
};

pub mod bitcoind;

/// Starts a fresh strfry relay container and returns (container, relay_url).
/// The container is automatically removed when dropped.
pub async fn start_relay() -> (testcontainers::ContainerAsync<GenericImage>, String) {
    let container = GenericImage::new("strfry-strfry", "latest")
        // strfry listens on port 7777 inside the container
        .with_exposed_port(7777.tcp())
        // Wait until strfry logs that the websocket server is ready
        .with_wait_for(WaitFor::message_on_stderr("Started websocket server"))
        .start()
        .await
        .expect("Failed to start strfry container");

    // testcontainers maps the container's port to a random host port
    let host_port = container
        .get_host_port_ipv4(7777)
        .await
        .expect("Failed to get mapped port");

    let relay_url = format!("ws://localhost:{}", host_port);
    println!("Strfry relay started on {}", relay_url);

    (container, relay_url)
}

pub fn test_guard() -> std::sync::MutexGuard<'static, ()> {
    static TEST_LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    TEST_LOCK
        .get_or_init(|| Mutex::new(()))
        .lock()
        .expect("test lock poisoned")
}

/// Publish a grant, signed by the owner.
///
/// Takes the profile as **JSON text** rather than a type. `dln-node` no
/// longer defines one: the profile belongs to `nostr-ln`, and a helper
/// that constructed the node's own type is how this harness came to write
/// `access_rate` — [dln-node#2](https://github.com/DarkWebDivingClub/dln-node/issues/2)
/// — and to keep asserting it for months.
///
/// Text means a test writes what a conforming issuer would write, and the
/// node either accepts it or does not.
#[allow(dead_code)]
pub async fn grant_usage_profile(
    owner_keys: &Keys,
    relay_url: &str,
    node_pubkey: PublicKey,
    target_pubkey: PublicKey,
    profile: &str,
) -> Result<()> {
    let content = profile.to_string();
    let d_value = format!("{}:{}", node_pubkey, target_pubkey);

    let owner_client = Client::builder().signer(owner_keys.clone()).build();
    owner_client.add_relay(relay_url).await?;
    owner_client.connect().await;
    tokio::time::sleep(Duration::from_secs(1)).await;

    // The kind is the crate's constant, not a number this harness
    // remembers. A test harness with its own copy of a protocol detail is
    // the same defect one level down.
    let grant_event = EventBuilder::new(Kind::Custom(nostr_ln::GRANT_KIND), content)
        .tag(Tag::parse(["d", d_value.as_str()]).expect("create d tag"))
        .tag(Tag::public_key(node_pubkey));
    owner_client.send_event_builder(grant_event).await?;

    Ok(())
}
