use crate::bitcoin_integration_suite::common::bitcoind::BitcoindHarness;
use crate::bitcoin_integration_suite::common::test_guard;
use ldk_node::bitcoin::Network;
use ldk_node::Builder;
use std::time::Duration;

/// Verifies an LDK node can connect to our regtest bitcoind and receive on-chain funds.
///
/// Steps:
/// 1. Start regtest bitcoind.
/// 2. Start LDK node configured with bitcoind RPC chain source.
/// 3. Mine 101 blocks to fund bitcoind wallet.
/// 4. Create LDK on-chain address and send 1 BTC to it.
/// 5. Mine 1 block and wait for wallet sync.
/// 6. Assert LDK on-chain spendable balance increased.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn receives_onchain_coin() {
    let _guard = test_guard();

    let bitcoind = BitcoindHarness::start().await;
    let miner_address = bitcoind.get_new_address().await;
    bitcoind.mine_blocks(101, &miner_address).await;

    let mut builder = Builder::new();
    builder.set_network(Network::Regtest);
    builder.set_chain_source_bitcoind_rpc(
        bitcoind.rpc_host().to_string(),
        bitcoind.rpc_port(),
        bitcoind.rpc_user().to_string(),
        bitcoind.rpc_password().to_string(),
    );
    let storage_dir = format!(
        "/tmp/ldk-node-it-{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock should be monotonic")
            .as_nanos()
    );
    builder.set_storage_dir_path(storage_dir);

    let node = builder.build().expect("LDK node should build");
    node.start().expect("LDK node should start");

    let ldk_addr = node
        .onchain_payment()
        .new_address()
        .expect("LDK should produce on-chain address")
        .to_string();

    bitcoind.send_to_address(&ldk_addr, 1.0).await;
    bitcoind.mine_blocks(1, &miner_address).await;

    let mut observed_sats: u64;
    let timeout = Duration::from_secs(20);
    let start = tokio::time::Instant::now();
    loop {
        if let Err(e) = node.sync_wallets() {
            panic!("LDK wallet sync failed: {e}");
        }

        let balances = node.list_balances();
        observed_sats = balances.spendable_onchain_balance_sats;
        if observed_sats >= 100_000_000 {
            break;
        }

        if start.elapsed() > timeout {
            break;
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }

    node.stop().expect("LDK node should stop");

    assert!(
        observed_sats >= 100_000_000,
        "expected at least 100_000_000 sats, got {observed_sats}"
    );
}
