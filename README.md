# Diamond Lightning Node — Bitcoin Knots variant

The Bitcoin Knots build of [`dln-node`](https://github.com/DarkWebDivingClub/dln-node),
for the chain that splits away with the BLAKE2b proof-of-work change.

It is a Lightning node built on [`ldk-node`], controlled entirely over Nostr
and designed to run without holding its own keys. It takes its chain data
from a bitcoind RPC endpoint, exposes no HTTP or gRPC surface, and delegates
signing to an external signer that can live in another process — or, for
tests, in-process or not at all.

## Why this repo exists

`dln-node` and this fork produce different binaries because Cargo's `[patch]`
is workspace-global: one workspace cannot build both a stock node and one
patched for BLAKE2b headers. A cross-chain swap needs both binaries running
at the same time, so they need separate working trees.

It began byte-identical to `dln-node`. What separates them now is the
`[patch]` section and one feature flag — no node logic.

## Level 0

The BLAKE2b hard fork is height-activated on `consensus.Blake2bHeight`.
Without `-testactivationheight=blake2b@N` that height stays at `INT_MAX`, so
a Knots chain produces only **v1 headers** — 80 bytes, SHA256d, bit 31 of
`nVersion` clear. At v1 a Knots chain is behaviourally Bitcoin Core, and this
node needs no changes to work against one.

That is verified rather than assumed: `dln-e2e-test`'s `knots_backend`
scenario asserts the headers are v1 and runs this binary against a Knots
regtest chain, and `two_chains` and `lightning_swap` run it alongside a
Core-side `dln-node`.

## What changes after activation

At and above the activation height, headers become **164 bytes**, flagged by
**bit 31 of `nVersion`**, carrying three nonces, a 128-bit extranonce,
height, txcount, flags, an XOR key and a merge-mining commitment. Proof of
work becomes a layered BLAKE2b over those rather than SHA256d over 80 bytes.
Test vectors live in Knots' `src/test/data/block_header_v2.json`; there is no
BIP, and PR `bitcoinknots#359` is the specification.

**That work is not in this repo.** This node never parses a block header —
`ldk-node`, `bdk` and ultimately the `bitcoin` crate do. What is here is the
`[patch]` section pointing at header-aware forks, and a feature that turns
them on.

## The `blake2b` feature

Off by default. With it off this builds and behaves as `dln-node` does, and
the patched crates are drop-in replacements for the published ones. Turning
it on enables `bitcoin/blake2b` throughout the tree:

    cargo build --features blake2b

Two forks are involved. `rust-bitcoin-knots` puts the v2 fields on
`bitcoin::block::Header` and hashes them the way Knots does.
`rust-lightning-knots` reads those fields out of the verbose
`getblockheader` JSON, which is the only place in the tree where a header is
built from named fields rather than from its serialization.

Both are patched in, so `bdk` and `ldk-node` need no changes at all.

The feature is off by default because it is not purely additive: bit 31 of
`nVersion` was a usable version bit in Bitcoin, and Knots repurposes it as
the header format flag, so with the feature on those bytes mean something
else. A node built with it still follows a Core chain — that is covered by
`bitcoin_integration`, which passes either way.

One detail worth carrying: the activation block gets a one-off
`Blake2bTargetShift` — 2²⁰ by default, but **2²² on mainnet**. Anything
that re-derives difficulty across the switch must special-case it or reject
the first BLAKE2b block. Nothing here re-derives it: the shift is applied
inside `GetNextWorkRequired` and arrives already folded into `nBits`.

## Relationship to `dln-node`

`upstream` points at `DarkWebDivingClub/dln-node`. Node changes are made
there and pulled here; this fork carries only what is specific to Knots.

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
