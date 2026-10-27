//! State shared by the desktop commands.

use std::collections::HashMap;
use std::sync::Mutex;

use tokio::sync::watch;

use crate::daemon::{Daemon, Endpoint, StreamTarget};

/// A running event subscription.
pub struct Subscription {
    /// Identifier of the subscription.
    pub id: String,
    /// Object whose events are streamed.
    pub target: StreamTarget,
    /// Sends `true` when the subscription has to stop.
    pub cancel: watch::Sender<bool>,
}

/// State managed by the application.
pub struct AppState {
    /// Client of the daemon.
    pub daemon: Daemon,
    /// Active subscriptions, keyed by subscription identifier.
    pub subscriptions: Mutex<HashMap<String, Subscription>>,
}

impl AppState {
    /// Build the state for a resolved daemon endpoint.
    pub fn new(endpoint: Endpoint) -> Result<Self, crate::error::CommandError> {
        Ok(Self {
            daemon: Daemon::connect(endpoint)?,
            subscriptions: Mutex::new(HashMap::new()),
        })
    }

    /// Insert a subscription and return its identifier.
    pub fn insert_subscription(&self, subscription: Subscription) -> String {
        let id = subscription.id.clone();
        let mut guard = self
            .subscriptions
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        guard.insert(id.clone(), subscription);
        id
    }

    /// Remove a subscription and stop its task.
    pub fn remove_subscription(&self, id: &str) -> Option<Subscription> {
        let mut guard = self
            .subscriptions
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let subscription = guard.remove(id);
        drop(guard);
        if let Some(subscription) = &subscription {
            let _ = subscription.cancel.send(true);
        }
        subscription
    }

    /// Stop every subscription. Used when the window goes away.
    pub fn cancel_all(&self) -> usize {
        let mut guard = self
            .subscriptions
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let count = guard.len();
        for subscription in guard.values() {
            let _ = subscription.cancel.send(true);
        }
        guard.clear();
        count
    }

    /// Stop one subscription. Returns whether it existed.
    pub fn cancel_subscription(&self, id: &str) -> bool {
        self.remove_subscription(id).is_some()
    }

    /// Number of active subscriptions.
    pub fn subscription_count(&self) -> usize {
        self.subscriptions
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .len()
    }
}
