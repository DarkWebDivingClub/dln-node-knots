use crate::bitcoin_integration_suite::common::bitcoind::BitcoindHarness;
use crate::bitcoin_integration_suite::common::test_guard;
use ldk_node::bitcoin::Network;
use ldk_node::lightning::ln::msgs::SocketAddress;
use ldk_node::lightning_invoice::{Bolt11InvoiceDescription, Description};
use ldk_node::{Builder, Event};
use std::net::TcpListener;
use std::str::FromStr;
use std::time::Duration;

fn free_local_port() -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind ephemeral port");
    let port = listener.local_addr().expect("read local addr").port();
    drop(listener);
    port
}

fn unique_storage_dir(prefix: &str) -> String {
    format!(
        "/tmp/{prefix}-{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock should be monotonic")
            .as_nanos()
    )
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn two_ldk_nodes_can_pay_each_other() {
    let _guard = test_guard();

    let bitcoind = BitcoindHarness::start().await;
    let miner_address = bitcoind.get_new_address().await;
    bitcoind.mine_blocks(101, &miner_address).await;

    let port_a = free_local_port();
    let port_b = free_local_port();
    let addr_a = SocketAddress::from_str(&format!("127.0.0.1:{port_a}"))
        .expect("valid node A socket address");
    let addr_b = SocketAddress::from_str(&format!("127.0.0.1:{port_b}"))
        .expect("valid node B socket address");

    let mut builder_a = Builder::new();
    builder_a.set_network(Network::Regtest);
    builder_a.set_chain_source_bitcoind_rpc(
        bitcoind.rpc_host().to_string(),
        bitcoind.rpc_port(),
        bitcoind.rpc_user().to_string(),
        bitcoind.rpc_password().to_string(),
    );
    builder_a
        .set_listening_addresses(vec![addr_a.clone()])
        .expect("set node A listening address");
    builder_a.set_storage_dir_path(unique_storage_dir("ldk-node-a"));
    let node_a = builder_a.build().expect("build node A");

    let mut builder_b = Builder::new();
    builder_b.set_network(Network::Regtest);
    builder_b.set_chain_source_bitcoind_rpc(
        bitcoind.rpc_host().to_string(),
        bitcoind.rpc_port(),
        bitcoind.rpc_user().to_string(),
        bitcoind.rpc_password().to_string(),
    );
    builder_b
        .set_listening_addresses(vec![addr_b.clone()])
        .expect("set node B listening address");
    builder_b.set_storage_dir_path(unique_storage_dir("ldk-node-b"));
    let node_b = builder_b.build().expect("build node B");

    node_a.start().expect("start node A");
    node_b.start().expect("start node B");

    // Fund node A so it can open a channel.
    let node_a_funding_addr = node_a
        .onchain_payment()
        .new_address()
        .expect("node A funding address")
        .to_string();
    bitcoind.send_to_address(&node_a_funding_addr, 0.05).await;
    bitcoind.mine_blocks(1, &miner_address).await;

    let sync_timeout = Duration::from_secs(20);
    let sync_start = tokio::time::Instant::now();
    loop {
        node_a.sync_wallets().expect("node A wallet sync");
        if node_a.list_balances().spendable_onchain_balance_sats >= 5_000_000 {
            break;
        }
        assert!(sync_start.elapsed() <= sync_timeout, "node A did not receive funding in time");
        tokio::time::sleep(Duration::from_millis(200)).await;
    }

    // Open channel A -> B.
    // Push initial liquidity to node B so both directions are reliably payable.
    node_a
        .open_channel(
            node_b.node_id(),
            addr_b,
            2_000_000,
            Some(1_000_000_000),
            None,
        )
        .expect("open channel A->B");

    // Confirm channel funding tx and wait until both nodes report channel ready.
    bitcoind.mine_blocks(1, &miner_address).await;

    let channel_timeout = Duration::from_secs(40);
    let channel_start = tokio::time::Instant::now();
    let mut a_channel_ready_event = false;
    let mut b_channel_ready_event = false;
    loop {
        node_a.sync_wallets().expect("node A wallet sync after channel open");
        node_b.sync_wallets().expect("node B wallet sync after channel open");

        let a_ready = node_a
            .list_channels()
            .iter()
            .any(|c| c.counterparty_node_id == node_b.node_id() && c.is_channel_ready);
        let b_ready = node_b
            .list_channels()
            .iter()
            .any(|c| c.counterparty_node_id == node_a.node_id() && c.is_channel_ready);

        if let Some(event) = node_a.next_event() {
            if matches!(event, Event::ChannelReady { .. }) {
                a_channel_ready_event = true;
            }
            node_a
                .event_handled()
                .expect("mark node A channel event handled");
        }
        if let Some(event) = node_b.next_event() {
            if matches!(event, Event::ChannelReady { .. }) {
                b_channel_ready_event = true;
            }
            node_b
                .event_handled()
                .expect("mark node B channel event handled");
        }

        if a_ready && b_ready && a_channel_ready_event && b_channel_ready_event {
            break;
        }

        assert!(
            channel_start.elapsed() <= channel_timeout,
            "channel did not become usable in time"
        );
        bitcoind.mine_blocks(1, &miner_address).await;
        tokio::time::sleep(Duration::from_millis(250)).await;
    }

    // B creates invoice, A pays it.
    let invoice_description =
        Bolt11InvoiceDescription::Direct(Description::new("node-b-receive".to_string()).unwrap());
    let invoice = node_b
        .bolt11_payment()
        .receive(1_000_000, &invoice_description, 3600)
        .expect("node B invoice creation");

    node_a
        .bolt11_payment()
        .send(&invoice, None)
        .expect("node A send payment");

    // Wait for receiver and sender payment events.
    let pay_timeout = Duration::from_secs(30);
    let pay_start = tokio::time::Instant::now();
    let mut receiver_got_payment = false;
    let mut sender_got_success = false;

    while !(receiver_got_payment && sender_got_success) {
        if let Some(event) = node_b.next_event() {
            if let Event::PaymentReceived { amount_msat, .. } = event {
                if amount_msat == 1_000_000 {
                    receiver_got_payment = true;
                }
            }
            node_b.event_handled().expect("mark node B event handled");
        }

        if let Some(event) = node_a.next_event() {
            if let Event::PaymentSuccessful { .. } = event {
                sender_got_success = true;
            }
            node_a.event_handled().expect("mark node A event handled");
        }

        assert!(pay_start.elapsed() <= pay_timeout, "payment did not complete in time");
        tokio::time::sleep(Duration::from_millis(100)).await;
    }

    // A creates invoice, B pays it (reverse direction).
    let reverse_invoice_description =
        Bolt11InvoiceDescription::Direct(Description::new("node-a-receive".to_string()).unwrap());
    let reverse_invoice = node_a
        .bolt11_payment()
        .receive(500_000, &reverse_invoice_description, 3600)
        .expect("node A reverse invoice creation");

    node_b
        .bolt11_payment()
        .send(&reverse_invoice, None)
        .expect("node B send reverse payment");

    let reverse_timeout = Duration::from_secs(30);
    let reverse_start = tokio::time::Instant::now();
    let mut reverse_receiver_got_payment = false;
    let mut reverse_sender_got_success = false;

    while !(reverse_receiver_got_payment && reverse_sender_got_success) {
        if let Some(event) = node_a.next_event() {
            if let Event::PaymentReceived { amount_msat, .. } = event {
                if amount_msat == 500_000 {
                    reverse_receiver_got_payment = true;
                }
            }
            node_a
                .event_handled()
                .expect("mark node A reverse event handled");
        }

        if let Some(event) = node_b.next_event() {
            if let Event::PaymentSuccessful { .. } = event {
                reverse_sender_got_success = true;
            }
            node_b
                .event_handled()
                .expect("mark node B reverse event handled");
        }

        assert!(
            reverse_start.elapsed() <= reverse_timeout,
            "reverse payment did not complete in time"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }

    node_a.stop().expect("stop node A");
    node_b.stop().expect("stop node B");
}
