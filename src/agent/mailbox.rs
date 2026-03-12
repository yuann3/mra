//! Private indirection layer for hot-swapping agent mailboxes on restart.

use std::sync::Arc;

use arc_swap::ArcSwap;
use tokio::sync::mpsc;

use super::message::AgentMessage;
use crate::error::AgentError;

/// Wraps an `mpsc::Sender` behind `ArcSwap` for lock-free hot-swapping.
pub(crate) struct MailboxSlot {
    sender: ArcSwap<mpsc::Sender<AgentMessage>>,
}

impl MailboxSlot {
    pub(crate) fn new(sender: mpsc::Sender<AgentMessage>) -> Self {
        Self {
            sender: ArcSwap::from_pointee(sender),
        }
    }

    /// Atomically replaces the inner sender.
    #[allow(dead_code)]
    pub(crate) fn swap(&self, new_sender: mpsc::Sender<AgentMessage>) {
        self.sender.store(Arc::new(new_sender));
    }

    /// Sends a message with one retry on closed channel (restart race).
    pub(crate) async fn send(&self, msg: AgentMessage) -> Result<(), AgentError> {
        let sender = self.sender.load_full();
        match sender.send(msg).await {
            Ok(()) => Ok(()),
            Err(mpsc::error::SendError(msg)) => {
                let sender = self.sender.load_full();
                sender.send(msg).await.map_err(|_| AgentError::Unavailable)
            }
        }
    }
}
