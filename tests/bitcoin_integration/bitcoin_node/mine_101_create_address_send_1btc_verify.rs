use crate::bitcoin_integration_suite::common::bitcoind::BitcoindHarness;
use crate::bitcoin_integration_suite::common::test_guard;

/// Verifies the baseline regtest transaction flow under full test control.
///
/// Steps:
/// 1. Start a dedicated bitcoind regtest node in Docker.
/// 2. Mine 101 blocks to mature coinbase funds.
/// 3. Create a fresh recipient address.
/// 4. Send exactly 1 BTC to that address.
/// 5. Mine 1 confirmation block.
/// 6. Verify `getreceivedbyaddress` reports `1.0` BTC.
#[tokio::test]
async fn mine_101_create_address_send_1btc_verify() {
    let _guard = test_guard();

    let bitcoind = BitcoindHarness::start().await;

    let miner_address = bitcoind.get_new_address().await;
    bitcoind.mine_blocks(101, &miner_address).await;

    let recipient = bitcoind.get_new_address().await;
    let _txid = bitcoind.send_to_address(&recipient, 1.0).await;

    // Confirm the payment so wallet accounting is deterministic.
    bitcoind.mine_blocks(1, &miner_address).await;

    let received = bitcoind.get_received_by_address(&recipient, 1).await;
    assert!(
        (received - 1.0).abs() < f64::EPSILON,
        "expected exactly 1.0 BTC received, got {received}"
    );
}
