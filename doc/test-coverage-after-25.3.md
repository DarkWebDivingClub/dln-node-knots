# Where the deleted tests' coverage went

Mission 25.3 deleted **47 of `dln-node`'s 59 test files**. This records
what each covered and where that coverage lives now, which is what the
mission's Definition of Done requires instead of "the suite still passes".

## Why they went

`tests/common/mod.rs` imported `dln_node::UsageProfile` and built grants
with `MethodAccessRule { access_rate: ... }` — **issue #2's misspelling,
in the shared test harness.** Every test that used it drove the request
pipeline: publish a grant, send a NWC or NIP-XX request over a relay,
check the response.

That pipeline is `nostr-ln`'s now. These were protocol tests wearing
method names, and re-pointing them at the crate would have produced a
second copy of the crate's own suite — inside the consumer that exists to
stop having a second copy of anything.

## The mapping

| Deleted | Covered | Now covered by |
|---|---|---|
| `nwc_*_roundtrip` ×20 | each NWC method answers over a relay | `nostr-ln-e2e-test/every_wallet_method` — **all twenty-four**, over a real relay, and `nostr-ln`'s `nwc_vectors` decodes each against the specification's own example |
| `control_*_roundtrip` ×8 | each NIP-XX method answers | `nostr-ln-e2e-test/every_method` |
| `control_stubs_not_implemented` | unimplemented methods return `NOT_IMPLEMENTED` | `nostr-ln`'s `wallet_pipeline::a_method_the_crate_types_but_the_wallet_lacks_is_not_implemented`. **Stronger now**: the method list is generated from the impl block, so an unimplemented method is not advertised in the first place |
| `access_grant_get_info`, `access_grant_publish`, `server_reads_grant` | grants are read off the relay and applied | `nostr-ln`'s `grants.rs` (15 cases) and `nostr-ln-e2e-test/unauthorized_without_a_grant`, `revocation_stops_delivery`, `reconnect_rereads_grants` |
| `usage_profile` | the profile JSON shape | `nostr-ln`'s `grants.rs`. **The old test asserted `access_rate`**, which is why #2 survived it |
| `nwc_get_info_rate_limited`, `nwc_pay_keysend_quota` | rate and quota refusal | `nostr-ln`'s `limits.rs` and `pipeline.rs`, including the cases #4 and #5 were about — refill from last withdrawal, and a rate that actually refills |
| `unit_rate_state`, `rate_state_error_codes`, `rate_limit_rule_json_bounds` | bucket arithmetic | as above. The modules they tested are deleted |
| `subscription_store_unit`, `nwc_subscribe_notifications_roundtrip`, `nnc_subscribe_notifications_roundtrip` | subscribing to notifications | `nostr-ln`'s `subscriptions.rs` and `nostr-ln-e2e-test/subscription_survives_restart`. A subscription is an addressable event now, not a method |
| `nwc_nip44_request_roundtrip`, `nwc_nip04_backward_compat`, `nwc_info_event_encryption_tag` | request encryption | `nostr-ln`'s transport. **`nwc_nip04_backward_compat` asserted the behaviour [#3](https://github.com/DarkWebDivingClub/dln-node/issues/3) reports as a defect**, so it is deleted rather than migrated |
| `control_kind_roundtrip` | the event kinds | `nostr-ln`'s `transport::the_kinds_are_the_ones_the_spec_names` |

## What was kept

| | |
|---|---|
| `ldk_service_integration`, `bitcoin_integration`, `nwc_ldk_integration`, `integration`, `e2e` | the node itself, against `bitcoind` |
| `relay_access_grant` | a relay sanity check, despite its name |
| `hello_gets_hi` | the harness works |

## What was ported rather than deleted

Five tests opened real channels and moved real money through the
protocol path. Their protocol half is covered above; their
channel-operation half is not covered anywhere else at this level, so
they were rewritten against the crate rather than dropped:

- `control_open_channel_roundtrip`
- `control_connect_disconnect_peer_roundtrip`
- `control_list_channels_with_open_channel`
- `control_channel_payments_scenario`
- `e2e_blackbox_container`

## Correction: the count was 47 at the top level, and more below it

The triage that produced "47 files" read `tests/*.rs` and missed two
directories of test *modules*, which are not separate test targets and so
do not appear in that glob. Recorded rather than quietly folded in,
because the number was stated before it was checked.

| | | |
|---|---|---|
| `tests/integration/usage_profile_service/` | 5 files | **deleted** — they test `usage_profile::service`, which no longer exists. Same class as `unit_rate_state` |
| `tests/nwc_ldk_integration/` | 9 files | **ported**, not deleted. They drive the deleted protocol path *and* real LDK against `bitcoind`, so by the rule applied to the five control tests they are worth keeping — and calling the handler directly is what they were trying to test all along |

Porting them removes a relay, a grant and an encryption round trip from
each, which were never what `make_invoice_happy_path` was about.

## Correction: three of the five did not need porting

The five were chosen as "protocol tests with real LDK work". Checking what
the mandatory suites actually call — rather than assuming — showed three
of them duplicated
[`dln-node-e2e-test/two_dln_nodes`](https://github.com/DarkWebDivingClub/dln-node-e2e-test),
which is *"alice opens a channel to bob, bob issues an invoice, alice pays
it, and the balance change is asserted on both sides"*, driven over NWC and
NIP-XX against regtest, and which is **mandatory**:

| Deleted | Duplicated by |
|---|---|
| `control_open_channel_roundtrip` | `two_dln_nodes` — `open_channel`, then `has_ready_channel_with` |
| `control_list_channels_with_open_channel` | `two_dln_nodes` — `has_ready_channel_with` reads `list_channels` |
| `control_channel_payments_scenario` | `two_dln_nodes` — the same scenario, end to end |
| `hello_gets_hi` | nothing, and nothing is right: it tested `run_client`, a demo that echoed "Hi" at "hello" and was deleted with the protocol layer |

Porting a test that a mandatory suite already runs adds a second place for
the same regression to be found and a second place for it to rot.

**Two were ported**, because nothing else covers them:

| Ported | Why it survives |
|---|---|
| `control_connect_disconnect_peer_roundtrip` | `connect_peer`, `disconnect_peer` and `list_peers` as operations. `two_dln_nodes` connects only as a side effect of opening a channel and never disconnects |
| `e2e_blackbox_container` | it builds the **binary**, writes a `config.toml`, and runs it. Nothing else tests `main.rs` — and `main.rs` changed most of all, since `nostr.owners` is now required and the run-without-a-node branch is gone |
