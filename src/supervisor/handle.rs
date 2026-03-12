use tokio::sync::{broadcast, mpsc, oneshot};
use tokio::task::JoinHandle;

use crate::agent::AgentHandle;
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

#[derive(Clone)]
pub struct SupervisorHandle {
    command_tx: mpsc::Sender<SupervisorCommand>,
    event_tx: broadcast::Sender<SupervisorEvent>,
}

impl SupervisorHandle {
    pub fn start(config: SupervisorConfig) -> (Self, JoinHandle<Result<(), SupervisorError>>) {
        let (command_tx, command_rx) = mpsc::channel(32);
        let (event_tx, _) = broadcast::channel(config.event_capacity);
        let runner = SupervisorRunner::new(config, command_rx, event_tx.clone());
        let join = tokio::spawn(runner.run());
        let handle = Self { command_tx, event_tx };
        (handle, join)
    }

    pub async fn start_child(&self, spec: ChildSpec) -> Result<AgentHandle, SupervisorError> {
        let (tx, rx) = oneshot::channel();
        self.command_tx
            .send(SupervisorCommand::StartChild { spec, reply: tx })
            .await
            .map_err(|_| SupervisorError::SpawnFailed("supervisor shut down".into()))?;
        rx.await
            .map_err(|_| SupervisorError::SpawnFailed("supervisor dropped reply".into()))?
    }

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

    pub fn subscribe(&self) -> broadcast::Receiver<SupervisorEvent> {
        self.event_tx.subscribe()
    }

    pub async fn shutdown(&self) {
        let _ = self.command_tx.send(SupervisorCommand::Shutdown).await;
    }
}
