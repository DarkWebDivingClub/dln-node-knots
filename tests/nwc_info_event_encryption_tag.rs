use nostr_sdk::prelude::*;
use std::time::Duration;

use dln_node::{clear_usage_profiles, set_relay_pubkey, CONTROL_INFO_KIND};

mod common;
use common::{start_relay, test_guard};

/// Verify that the Kind 13194 (NWC) and Kind 13198 (NNC) info events
/// include the `["encryption", "nip44_v2 nip04"]` tag.
#[tokio::test]
async fn test_info_events_have_encryption_tag() -> Result<()> {
    let _guard = test_guard();
    clear_usage_profiles();
    let (_container, relay_url) = start_relay().await;

    let relay_pubkey = Keys::generate().public_key();
    set_relay_pubkey(relay_pubkey.clone());

    let service_keys = Keys::generate();
    let service_pubkey = service_keys.public_key();
    let _service_client = dln_node::run_nwc_service(service_keys, &relay_url).await?;
    tokio::time::sleep(Duration::from_secs(1)).await;

    let reader = Client::builder().signer(Keys::generate()).build();
    reader.add_relay(&relay_url).await?;
    reader.connect().await;

    // Fetch Kind 13194 (WalletConnectInfo) events
    let nwc_info_filter = Filter::new()
        .kind(Kind::WalletConnectInfo)
        .author(service_pubkey);
    let nwc_info_events = reader
        .fetch_events(nwc_info_filter)
        .timeout(Duration::from_secs(5))
        .await?;
    assert_eq!(nwc_info_events.len(), 1);
    let nwc_info = nwc_info_events.iter().next().unwrap();

    // Check encryption tag
    let enc_tag = nwc_info
        .tags
        .iter()
        .find(|tag| tag.as_slice().first().map(|v| v.as_str()) == Some("encryption"));
    assert!(enc_tag.is_some(), "NWC info event must have encryption tag");
    assert_eq!(
        enc_tag.unwrap().as_slice().get(1).unwrap().as_str(),
        "nip44_v2 nip04"
    );

    // Check notifications tag
    let notif_tag = nwc_info
        .tags
        .iter()
        .find(|tag| tag.as_slice().first().map(|v| v.as_str()) == Some("notifications"));
    assert!(
        notif_tag.is_some(),
        "NWC info event must have notifications tag"
    );
    let notif_value = notif_tag.unwrap().as_slice().get(1).unwrap().as_str();
    assert!(notif_value.contains("payment_received"));
    assert!(notif_value.contains("payment_sent"));

    // Fetch Kind 13198 (NNC info) events
    let nnc_info_filter = Filter::new()
        .kind(Kind::Custom(CONTROL_INFO_KIND))
        .author(service_pubkey);
    let nnc_info_events = reader
        .fetch_events(nnc_info_filter)
        .timeout(Duration::from_secs(5))
        .await?;
    assert_eq!(nnc_info_events.len(), 1);
    let nnc_info = nnc_info_events.iter().next().unwrap();

    // Check encryption tag on NNC info
    let nnc_enc_tag = nnc_info
        .tags
        .iter()
        .find(|tag| tag.as_slice().first().map(|v| v.as_str()) == Some("encryption"));
    assert!(
        nnc_enc_tag.is_some(),
        "NNC info event must have encryption tag"
    );
    assert_eq!(
        nnc_enc_tag.unwrap().as_slice().get(1).unwrap().as_str(),
        "nip44_v2 nip04"
    );

    // Check notifications tag on NNC info
    let nnc_notif_tag = nnc_info
        .tags
        .iter()
        .find(|tag| tag.as_slice().first().map(|v| v.as_str()) == Some("notifications"));
    assert!(
        nnc_notif_tag.is_some(),
        "NNC info event must have notifications tag"
    );
    let nnc_notif_value = nnc_notif_tag.unwrap().as_slice().get(1).unwrap().as_str();
    assert!(nnc_notif_value.contains("channel_opened"));
    assert!(nnc_notif_value.contains("channel_closed"));

    Ok(())
}
