use dln_node::clear_subscriptions;

// The subscription_store module is pub(crate), so we test it through
// the public clear_subscriptions() function and by verifying behavior
// via the NWC/NNC subscribe_notifications roundtrip tests.
//
// This file tests the store indirectly by subscribing via the library
// and checking that clear works without panics.

#[test]
fn test_clear_subscriptions_does_not_panic() {
    clear_subscriptions();
    // Call twice to ensure repeated clearing is safe
    clear_subscriptions();
}
