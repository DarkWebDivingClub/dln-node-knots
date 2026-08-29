use crate::ldk_service_integration_suite::common::bitcoind::BitcoindHarness;
use crate::ldk_service_integration_suite::common::test_guard;
use dln_node::lightning::{LdkService, LdkServiceConfig};
use std::time::Duration;

fn unique_storage_dir(prefix: &str) -> String {
    format!(
        "/tmp/{prefix}-{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock should be monotonic")
            .as_nanos()
    )
}

/// Verifies the first happy path for direct LdkService use:
/// start -> fund -> sync -> read balance.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn start_sync_balance() {
    let _guard = test_guard();

    let bitcoind = BitcoindHarness::start().await;
    let miner_address = bitcoind.get_new_address().await;
    bitcoind.mine_blocks(101, &miner_address).await;

    let cfg = LdkServiceConfig {
        network: "regtest".to_string(),
        bitcoind_rpc_host: bitcoind.rpc_host().to_string(),
        bitcoind_rpc_port: bitcoind.rpc_port(),
        bitcoind_rpc_user: bitcoind.rpc_user().to_string(),
        bitcoind_rpc_password: bitcoind.rpc_password().to_string(),
        ldk_storage_dir: unique_storage_dir("ldk-service-sync-balance"),
        ldk_listen_addr: None,
        node_alias: None,
        signer_transport: "embedded".to_string(),
        signer_relay: None,
        signer_nsec: None,
        signer_pubkey: None,
    };

    let service = LdkService::start_from_config(&cfg).expect("LdkService should start");
    let ldk_address = service
        .new_onchain_address()
        .expect("LdkService should provide on-chain address");

    bitcoind.send_to_address(&ldk_address, 1.0).await;
    bitcoind.mine_blocks(1, &miner_address).await;

    let timeout = Duration::from_secs(20);
    let start = tokio::time::Instant::now();
    let mut observed_msat: u64;
    loop {
        service.sync_wallets().expect("wallet sync should succeed");
        observed_msat = service
            .get_balance_msat()
            .expect("balance read should succeed");

        if observed_msat >= 100_000_000_000 {
            break;
        }

        assert!(start.elapsed() <= timeout, "balance did not update in time");
        tokio::time::sleep(Duration::from_millis(200)).await;
    }

    service.stop().expect("LdkService should stop cleanly");

    assert!(
        observed_msat >= 100_000_000_000,
        "expected at least 100_000_000_000 msat, got {observed_msat}"
    );
}
