use std::sync::Arc;

use tokio::sync::{broadcast, mpsc, oneshot};
use tokio::task::JoinHandle;

use crate::agent::AgentHandle;
use crate::budget::BudgetTracker;
use crate::error::SupervisorError;

use super::child::ChildSpec;
use super::config::SupervisorConfig;
use super::event::SupervisorEvent;
use super::runner::SupervisorRunner;

pub(crate) enum SupervisorCommand {
    StartChild {
        spec: ChildSpec,
        reply: oneshot::Sender<Result<AgentHandle, SupervisorError>>,
    },
    StopChild {
        name: String,
        reply: oneshot::Sender<Result<(), SupervisorError>>,
    },
    GetChild {
        name: String,
        reply: oneshot::Sender<Option<AgentHandle>>,
    },
    Shutdown,
}

/// Cloneable handle for sending commands to a running supervisor.
///
/// All operations go through a bounded `mpsc` channel, so the handle
/// is safe to share across tasks. Dropping every clone of the handle
/// causes the supervisor to drain its children and exit.
#[derive(Clone)]
pub struct SupervisorHandle {
    command_tx: mpsc::Sender<SupervisorCommand>,
    event_tx: broadcast::Sender<SupervisorEvent>,
}

impl SupervisorHandle {
    /// Starts a supervisor with no token budget.
    pub fn start(config: SupervisorConfig) -> (Self, JoinHandle<Result<(), SupervisorError>>) {
        Self::start_with_budget(config, None)
    }

    /// Starts a supervisor with an optional shared [`BudgetTracker`].
    pub fn start_with_budget(
        config: SupervisorConfig,
        budget: Option<Arc<BudgetTracker>>,
    ) -> (Self, JoinHandle<Result<(), SupervisorError>>) {
        let (command_tx, command_rx) = mpsc::channel(32);
        let (event_tx, _) = broadcast::channel(config.event_capacity);
        let runner = SupervisorRunner::new(config, command_rx, event_tx.clone(), budget);
        let join = tokio::spawn(runner.run());
        let handle = Self {
            command_tx,
            event_tx,
        };
        (handle, join)
    }

    /// Returns a clone of the event broadcast sender.
    pub fn event_sender(&self) -> &broadcast::Sender<SupervisorEvent> {
        &self.event_tx
    }

    /// Spawns a new child from a [`ChildSpec`] and returns its handle.
    pub async fn start_child(&self, spec: ChildSpec) -> Result<AgentHandle, SupervisorError> {
        let (tx, rx) = oneshot::channel();
        self.command_tx
            .send(SupervisorCommand::StartChild { spec, reply: tx })
            .await
            .map_err(|_| SupervisorError::SpawnFailed("supervisor shut down".into()))?;
        rx.await
            .map_err(|_| SupervisorError::SpawnFailed("supervisor dropped reply".into()))?
    }

    /// Cancels a running child by name.
    pub async fn stop_child(&self, name: &str) -> Result<(), SupervisorError> {
        let (tx, rx) = oneshot::channel();
        self.command_tx
            .send(SupervisorCommand::StopChild {
                name: name.into(),
                reply: tx,
            })
            .await
            .map_err(|_| SupervisorError::SpawnFailed("supervisor shut down".into()))?;
        rx.await
            .map_err(|_| SupervisorError::SpawnFailed("supervisor dropped reply".into()))?
    }

    /// Looks up a child's handle by name, or `None` if it doesn't exist.
    pub async fn child(&self, name: &str) -> Option<AgentHandle> {
        let (tx, rx) = oneshot::channel();
        self.command_tx
            .send(SupervisorCommand::GetChild {
                name: name.into(),
                reply: tx,
            })
            .await
            .ok()?;
        rx.await.ok()?
    }

    /// Subscribes to [`SupervisorEvent`]s.
    pub fn subscribe(&self) -> broadcast::Receiver<SupervisorEvent> {
        self.event_tx.subscribe()
    }

    /// Sends a shutdown command; all children are drained.
    pub async fn shutdown(&self) {
        let _ = self.command_tx.send(SupervisorCommand::Shutdown).await;
    }
}
