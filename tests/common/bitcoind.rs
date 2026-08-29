use serde_json::{json, Value};
use std::time::Duration;
use testcontainers::{
    core::IntoContainerPort,
    runners::AsyncRunner,
    ContainerAsync, GenericImage,
};
use testcontainers::ImageExt;

const RPC_USER: &str = "rpcuser";
const RPC_PASS: &str = "rpcpass";
const RPC_PORT: u16 = 18443;

pub struct BitcoindHarness {
    _container: ContainerAsync<GenericImage>,
    rpc_url: String,
    rpc_port: u16,
    client: reqwest::Client,
}

impl BitcoindHarness {
    pub async fn start() -> Self {
        let container = GenericImage::new("ruimarinho/bitcoin-core", "latest")
            .with_exposed_port(RPC_PORT.tcp())
            .with_cmd(vec![
                "-regtest=1".to_string(),
                "-server=1".to_string(),
                "-txindex=1".to_string(),
                "-printtoconsole".to_string(),
                "-fallbackfee=0.0002".to_string(),
                "-rpcbind=0.0.0.0".to_string(),
                "-rpcallowip=0.0.0.0/0".to_string(),
                format!("-rpcuser={RPC_USER}"),
                format!("-rpcpassword={RPC_PASS}"),
            ])
            .start()
            .await
            .expect("Failed to start bitcoind container");

        let host_port = container
            .get_host_port_ipv4(RPC_PORT)
            .await
            .expect("Failed to get mapped bitcoind RPC port");

        let rpc_url = format!("http://localhost:{host_port}");
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(10))
            .build()
            .expect("Failed to build HTTP client");

        let harness = Self {
            _container: container,
            rpc_url,
            rpc_port: host_port,
            client,
        };

        harness.wait_until_ready().await;
        harness.create_wallet("testwallet").await;

        harness
    }

    pub async fn create_wallet(&self, wallet: &str) {
        let _ = self
            .rpc_call("createwallet", json!([wallet]))
            .await
            .expect("createwallet RPC should succeed");
    }

    pub async fn get_new_address(&self) -> String {
        self.rpc_call("getnewaddress", json!([]))
            .await
            .expect("getnewaddress RPC should succeed")
            .as_str()
            .expect("getnewaddress result should be a string")
            .to_string()
    }

    pub async fn mine_blocks(&self, blocks: u64, address: &str) {
        let _ = self
            .rpc_call("generatetoaddress", json!([blocks, address]))
            .await
            .expect("generatetoaddress RPC should succeed");
    }

    pub async fn send_to_address(&self, address: &str, amount_btc: f64) -> String {
        self.rpc_call("sendtoaddress", json!([address, amount_btc]))
            .await
            .expect("sendtoaddress RPC should succeed")
            .as_str()
            .expect("sendtoaddress result should be a txid string")
            .to_string()
    }

    pub async fn get_received_by_address(&self, address: &str, min_conf: u64) -> f64 {
        self.rpc_call("getreceivedbyaddress", json!([address, min_conf]))
            .await
            .expect("getreceivedbyaddress RPC should succeed")
            .as_f64()
            .expect("getreceivedbyaddress result should be a number")
    }

    pub fn rpc_host(&self) -> &'static str {
        "127.0.0.1"
    }

    pub fn rpc_port(&self) -> u16 {
        self.rpc_port
    }

    pub fn rpc_user(&self) -> &'static str {
        RPC_USER
    }

    pub fn rpc_password(&self) -> &'static str {
        RPC_PASS
    }

    async fn wait_until_ready(&self) {
        for _ in 0..60u32 {
            let ready = self.rpc_call("getblockchaininfo", json!([])).await.is_ok();
            if ready {
                return;
            }
            tokio::time::sleep(Duration::from_millis(250)).await;
        }

        panic!("bitcoind RPC did not become ready in time");
    }

    async fn rpc_call(&self, method: &str, params: Value) -> Result<Value, String> {
        let body = json!({
            "jsonrpc": "1.0",
            "id": "test",
            "method": method,
            "params": params,
        });

        let response = self
            .client
            .post(&self.rpc_url)
            .basic_auth(RPC_USER, Some(RPC_PASS))
            .json(&body)
            .send()
            .await
            .map_err(|e| format!("RPC request failed for {method}: {e}"))?;

        let status = response.status();
        let json: Value = response
            .json()
            .await
            .map_err(|e| format!("RPC response JSON decode failed for {method}: {e}"))?;

        if !status.is_success() {
            return Err(format!("RPC HTTP status error for {method}: {status}, body: {json}"));
        }

        if !json["error"].is_null() {
            return Err(format!("RPC returned error for {method}: {}", json["error"]));
        }

        Ok(json["result"].clone())
    }
}
