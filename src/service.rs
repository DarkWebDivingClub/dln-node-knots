//! Starting the node: a handler on each side, and the crate around them.
//!
//! What this file does **not** contain is the point of the mission it came
//! from. There is no relay subscription, no event kind, no NIP-44, no
//! grant parsing and no rate bucket here, because `nostr-ln` owns all of
//! it — for this node and for every other consumer at the same time.

use std::sync::Arc;

use nostr_sdk::prelude::{Keys, PublicKey};
use nostr_ln::service::Service;

use crate::control::Control;
use crate::events;
use crate::lightning::LdkService;
use crate::wallet::Wallet;

/// Everything needed to serve.
pub struct NodeConfig {
    /// The node's own key.
    pub keys: Keys,
    /// Relays to serve on.
    pub relays: Vec<String>,
    /// **Whose grants this node accepts.**
    ///
    /// An empty list accepts none, and therefore answers nothing. That is
    /// [dln-node#1](https://github.com/DarkWebDivingClub/dln-node/issues/1)
    /// closed by construction: the old code kept an `OWNERS` list that
    /// nothing ever wrote to, and applied every grant it saw regardless of
    /// who signed it. Absent configuration now fails closed rather than
    /// being read as "any owner".
    pub owners: Vec<PublicKey>,
    /// What to call itself.
    pub alias: Option<String>,
}

/// Start the service and the LDK event pump. Runs until stopped.
pub async fn run(cfg: NodeConfig, ldk: Arc<LdkService>) -> Result<(), nostr_ln::service::transport::Error> {
    let wallet = Arc::new(Wallet::new(ldk.clone(), cfg.alias));

    let service = Service::new(cfg.keys, cfg.relays, cfg.owners);
    // Built before `run`, because the notifier has to exist before the
    // handler that holds it does.
    let control = Arc::new(Control::new(ldk.clone()).with_notifier(service.notifier()));
    let notifier = service.notifier();
    let service = service.wallet(wallet).control(control);

    // The event pump. Separate from the service loop because an LDK event
    // arrives whether or not anybody is connected, and the node must keep
    // up with the chain regardless.
    let event_ldk = ldk.clone();
    tokio::spawn(async move {
        loop {
            let event = event_ldk.node().next_event_async().await;
            events::handle(&notifier, &event_ldk, &event).await;
            // Marked handled after the notification attempt, so a delivery
            // failure does not silently drop the event from LDK's queue.
            if let Err(e) = event_ldk.node().event_handled() {
                tracing::warn!("could not mark an LDK event handled: {e}");
            }
        }
    });

    service.run().await
}
