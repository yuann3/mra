//! Swarm runtime — thin wrapper around the root supervisor.

use tokio::sync::broadcast;
use tokio::task::JoinHandle;

use crate::agent::AgentHandle;
use crate::error::SupervisorError;
use crate::supervisor::{ChildSpec, SupervisorConfig, SupervisorEvent, SupervisorHandle};

/// Manages a supervised set of agents.
///
/// Thin wrapper around a root [`SupervisorHandle`]. Agents are spawned
/// via [`ChildSpec`] factories and managed by the supervisor's restart
/// and hang-detection policies.
pub struct SwarmRuntime {
    supervisor: SupervisorHandle,
    join: JoinHandle<Result<(), SupervisorError>>,
}

impl SwarmRuntime {
    /// Creates a new runtime backed by a supervisor with the given config.
    pub fn new(config: SupervisorConfig) -> Self {
        let (supervisor, join) = SupervisorHandle::start(config);
        Self { supervisor, join }
    }

    /// Spawns a child agent via the supervisor.
    pub async fn spawn(&self, spec: ChildSpec) -> Result<AgentHandle, SupervisorError> {
        self.supervisor.start_child(spec).await
    }

    /// Looks up a child handle by name.
    pub async fn get_handle_by_name(&self, name: &str) -> Option<AgentHandle> {
        self.supervisor.child(name).await
    }

    /// Subscribes to supervisor events.
    pub fn subscribe(&self) -> broadcast::Receiver<SupervisorEvent> {
        self.supervisor.subscribe()
    }

    /// Gracefully shuts down all agents and the supervisor.
    pub async fn shutdown(self) {
        self.supervisor.shutdown().await;
        let _ = self.join.await;
    }
}
