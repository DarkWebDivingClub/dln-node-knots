use crate::ldk_service_integration_suite::common::bitcoind::BitcoindHarness;
use crate::ldk_service_integration_suite::common::test_guard;
use dln_node::lightning::{LdkService, LdkServiceConfig};

fn unique_storage_dir(prefix: &str) -> String {
    format!(
        "/tmp/{prefix}-{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock should be monotonic")
            .as_nanos()
    )
}

/// Verifies basic LdkService lifecycle and identity behavior.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn lifecycle() {
    let _guard = test_guard();

    let bitcoind = BitcoindHarness::start().await;
    let cfg = LdkServiceConfig {
        network: "regtest".to_string(),
        bitcoind_rpc_host: bitcoind.rpc_host().to_string(),
        bitcoind_rpc_port: bitcoind.rpc_port(),
        bitcoind_rpc_user: bitcoind.rpc_user().to_string(),
        bitcoind_rpc_password: bitcoind.rpc_password().to_string(),
        ldk_storage_dir: unique_storage_dir("ldk-service-lifecycle"),
        ldk_listen_addr: None,
        node_alias: None,
        signer_transport: "embedded".to_string(),
        signer_relay: None,
        signer_nsec: None,
        signer_pubkey: None,
    };

    let service = LdkService::start_from_config(&cfg).expect("LdkService should start");

    let node_id = service.node_id();
    assert!(!node_id.is_empty(), "node_id should not be empty");
    assert_eq!(service.network(), "regtest");
    assert_eq!(service.node_id(), node_id, "node_id should be stable");

    service.stop().expect("LdkService should stop cleanly");
}
