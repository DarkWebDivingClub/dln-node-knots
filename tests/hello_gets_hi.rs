use nostr_sdk::prelude::*;
use std::time::Duration;

mod common;
use common::{start_relay, test_guard};

/// End-to-end test: send "hello", expect the app to respond with "Hi".
#[tokio::test]
async fn test_hello_gets_hi_response() -> Result<()> {
    let _guard = test_guard();
    let (_container, relay_url) = start_relay().await;

    // App client — this is what we're testing.
    let app_keys = Keys::generate();
    let app_pubkey = app_keys.public_key();
    let _app_client = dln_node::run_client(app_keys, &relay_url).await?;

    tokio::time::sleep(Duration::from_secs(1)).await;

    // Sender client — simulates an external user sending "hello"
    let sender_keys = Keys::generate();
    let sender_client = Client::builder().signer(sender_keys).build();
    sender_client.add_relay(&relay_url).await?;
    sender_client.connect().await;
    tokio::time::sleep(Duration::from_secs(1)).await;

    // Subscribe to text notes from the app's pubkey so we can see its "Hi" response
    let filter = Filter::new().kind(Kind::TextNote).author(app_pubkey);
    sender_client.subscribe(filter).await?;

    // Send "hello"
    let builder = EventBuilder::text_note("hello");
    sender_client.send_event_builder(builder).await?;

    // Wait for the app's "Hi" response (timeout after 10 seconds)
    let timeout = Duration::from_secs(10);
    let result = tokio::time::timeout(timeout, async {
        let mut notifications = sender_client.notifications();
        while let Some(notification) = notifications.next().await {
            if let ClientNotification::Event { event, .. } = notification {
                let event = event.as_ref();
                if event.pubkey == app_pubkey && event.content == "Hi" {
                    break;
                }
            }
        }
        Ok::<(), nostr_sdk::client::Error>(())
    })
    .await;

    match result {
        Ok(Ok(())) => Ok(()),
        Ok(Err(e)) => panic!("Notification handler error: {}", e),
        Err(_) => panic!("Timeout: did not receive 'Hi' response within 10 seconds"),
    }
}
