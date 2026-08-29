use nostr_sdk::prelude::*;
use std::time::Duration;

mod common;
use common::{start_relay, test_guard};

/// End-to-end test: connect to relay and publish a simple event.
#[tokio::test]
async fn test_relay_connect_and_publish() -> Result<()> {
    let _guard = test_guard();
    let (_container, relay_url) = start_relay().await;

    let publisher_keys = Keys::generate();
    let publisher_pubkey = publisher_keys.public_key();
    let publisher = Client::builder().signer(publisher_keys).build();
    publisher.add_relay(&relay_url).await?;
    publisher.connect().await;
    tokio::time::sleep(Duration::from_secs(1)).await;

    let filter = Filter::new().kind(Kind::TextNote).author(publisher_pubkey);
    let subscriber = Client::builder().signer(Keys::generate()).build();
    subscriber.add_relay(&relay_url).await?;
    subscriber.connect().await;
    tokio::time::sleep(Duration::from_secs(1)).await;
    subscriber.subscribe(filter).await?;

    let builder = EventBuilder::text_note("ping");
    publisher.send_event_builder(builder).await?;

    let timeout = Duration::from_secs(10);
    let result = tokio::time::timeout(timeout, async {
        let mut notifications = subscriber.notifications();
        while let Some(notification) = notifications.next().await {
            if let ClientNotification::Event { event, .. } = notification {
                let event = event.as_ref();
                if event.pubkey == publisher_pubkey && event.content == "ping" {
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
        Err(_) => panic!("Timeout: did not receive ping within 10 seconds"),
    }
}
