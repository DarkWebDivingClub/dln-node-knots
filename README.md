# Diamond Lightning Node

`dln-node` is a Lightning node built on [`ldk-node`], controlled entirely over
Nostr and designed to run without holding its own keys.

It takes its chain data from a Bitcoin Core RPC endpoint, exposes no HTTP or
gRPC surface, and delegates signing to an external signer that can live in
another process — or, for tests, in-process or not at all.

Forked from `FlowRateHQ/eln-node` and renamed for dwdc.

## Control plane

Nostr is the node's only API. There is no CLI and no local socket. Two message
channels carry different classes of operation:

| Channel | Kind | Operations |
|---|---|---|
| **NWC** (NIP-47) | standard | `get_info`, `get_balance`, `make_invoice`, `pay_invoice`, `pay_onchain`, `make_new_address` |
| **NCC** | 23198 request / 23199 response, NIP-04 encrypted | `open_channel`, `list_channels`, `close_channel` |

Both are gated by **grants**: kind-30078 events addressed with a `d` tag of
`{service_pubkey}:{client_pubkey}`, carrying a `methods` map with a per-method
`access_rate`. Authorisation is per method *and* per client key, so different
clients can hold different capabilities against the same node. A method absent
from the grant is refused with `Restricted`.

## Signing

On the default paths the node never sees raw keys. LDK's `KeysInterface` is
backed by a signing client, so every signing operation is a request the signer
validates against policy before honouring — and can refuse.

`signer.transport` selects how:

| Mode | Where keys live | Policy validation | Use |
|---|---|---|---|
| `nostr` | separate signer process, reached over a relay | full | production |
| `embedded` | in-process signer, no transport | two routing-balance policies downgraded to warnings | tests |
| `none` | the node itself, via ldk-node's own `KeysManager` | none | bring-up only |

`embedded` also runs without a state persister, so signer state does not
survive a restart.

`none` bypasses the signer entirely. It exists because two nodes running
embedded signers derive the same `node_id` and therefore cannot peer with each
other, which makes multi-node scenarios impossible. **It is not a production
configuration**: the node holds its own keys, and nothing validates what it
signs.

## Configuration

`config.toml`, read from the working directory:

```toml
[node]
network = "regtest"          # regtest | testnet | signet | bitcoin
listening_port = 9735
data_dir = "./data"
# alias = "optional"

[nostr]
relay = "ws://localhost:7777"
private_key = "<nsec or hex>"

[wallet]
max_channel_size_sats = 10000000
min_channel_size_sats = 20000
auto_accept_channels = false

[bitcoind]
rpc_host = "127.0.0.1"
rpc_port = 18443
rpc_user = "rpcuser"
rpc_password = "rpcpass"

[signer]
transport = "nostr"          # nostr | embedded | none
relay = "ws://localhost:7777"   # nostr transport only
nsec = "<node proxy nsec hex>"  # nostr transport only
signer_pubkey = "<signer pubkey hex>"  # nostr transport only
```

`[bitcoind]` and `[signer]` are optional; omitting `[bitcoind]` starts the NWC
service without a Lightning node attached.

## Building

```
cargo build --bin dln-node
```

## Testing

End-to-end scenarios live in [`dln-e2e-test`](../dln-e2e-test), which drives
real nodes against a Bitcoin Core regtest container:

- `two_dln_nodes` — two nodes, channel open, invoice, Lightning payment
- `onchain_payment` — on-chain send, verified against bitcoind

Both currently run with `transport = "none"`.

[`ldk-node`]: https://github.com/lightningdevkit/ldk-node
