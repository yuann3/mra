use tokio::sync::{mpsc, oneshot};
use tokio_util::sync::CancellationToken;

use crate::error::AgentError;
use crate::ids::AgentId;

use super::message::{AgentMessage, AgentReply, Task};

/// Cloneable handle for communicating with a running agent.
///
/// Holds a bounded `mpsc::Sender` to the agent's mailbox and a
/// [`CancellationToken`] for hard shutdown. Sending through the channel
/// applies async backpressure — `execute` will yield (not block the OS
/// thread) when the inbox is full.
#[derive(Clone)]
pub struct AgentHandle {
    id: AgentId,
    sender: mpsc::Sender<AgentMessage>,
    cancel: CancellationToken,
}

impl AgentHandle {
    pub(crate) fn new(
        id: AgentId,
        sender: mpsc::Sender<AgentMessage>,
        cancel: CancellationToken,
    ) -> Self {
        Self { id, sender, cancel }
    }

    /// Returns this agent's unique identifier.
    pub fn id(&self) -> AgentId {
        self.id
    }

    /// Sends a [`Task`] to the agent and awaits the reply.
    ///
    /// Returns [`AgentError::Cancelled`] if the agent has already stopped
    /// (channel closed) or drops the response sender before replying.
    pub async fn execute(&self, task: Task) -> Result<AgentReply, AgentError> {
        let (tx, rx) = oneshot::channel();
        self.sender
            .send(AgentMessage::Execute {
                task,
                respond_to: tx,
            })
            .await
            .map_err(|_| AgentError::Cancelled)?;

        rx.await.map_err(|_| AgentError::Cancelled)?
    }

    /// Requests graceful shutdown. The agent closes its receiver, drains
    /// buffered work until `deadline`, then exits. Best-effort — the send
    /// is ignored if the agent is already gone.
    pub async fn shutdown(&self, deadline: tokio::time::Instant) {
        let _ = self.sender.send(AgentMessage::Shutdown { deadline }).await;
    }

    /// Triggers immediate cancellation via the [`CancellationToken`].
    ///
    /// The runner's `select!` loop will observe this and break out of
    /// both the message loop and any in-flight handler future.
    pub fn cancel(&self) {
        self.cancel.cancel();
    }
}
