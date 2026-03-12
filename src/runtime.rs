//! Swarm runtime — manages agent lifecycles and shutdown.

use std::collections::HashMap;
use std::sync::Arc;

use tokio_util::sync::CancellationToken;

use crate::agent::{AgentBehavior, AgentHandle, SpawnedAgent};
use crate::config::{AgentConfig, RuntimeConfig};
use crate::ids::AgentId;
use crate::llm::LlmProvider;

/// Manages a set of running agents with lifecycle control.
///
/// Agents are spawned with explicit peer wiring (no dynamic discovery).
/// Shutdown cancels the global token and joins all tasks within the
/// configured timeout.
pub struct SwarmRuntime {
    agents: HashMap<AgentId, SpawnedAgent>,
    handles_by_name: HashMap<String, AgentHandle>,
    cancel: CancellationToken,
    config: RuntimeConfig,
}

impl SwarmRuntime {
    /// Creates a new runtime with the given config.
    pub fn new(config: RuntimeConfig) -> Self {
        Self {
            agents: HashMap::new(),
            handles_by_name: HashMap::new(),
            cancel: CancellationToken::new(),
            config,
        }
    }

    /// Spawns an agent and registers it in the runtime.
    ///
    /// `peers` maps peer names to their handles — injected into the
    /// agent's [`AgentCtx`]. `llm` is an optional shared provider.
    pub fn spawn<B: AgentBehavior>(
        &mut self,
        name: &str,
        config: AgentConfig,
        behavior: B,
        peers: HashMap<String, AgentHandle>,
        llm: Option<Arc<dyn LlmProvider>>,
    ) -> AgentHandle {
        let id = AgentId::new();
        let child_cancel = self.cancel.child_token();

        let spawned = AgentHandle::spawn(id, config, behavior, peers, llm, child_cancel);
        let handle = spawned.handle.clone();

        self.agents.insert(id, spawned);
        self.handles_by_name.insert(name.into(), handle.clone());

        handle
    }

    /// Returns a cloned handle by agent id.
    pub fn get_handle(&self, id: AgentId) -> Option<AgentHandle> {
        self.agents.get(&id).map(|s| s.handle.clone())
    }

    /// Returns a cloned handle by agent name.
    pub fn get_handle_by_name(&self, name: &str) -> Option<AgentHandle> {
        self.handles_by_name.get(name).cloned()
    }

    /// Gracefully shuts down all agents.
    ///
    /// Cancels the global token, then joins all agent tasks within
    /// `RuntimeConfig::shutdown_timeout`.
    pub async fn shutdown(&mut self) {
        self.cancel.cancel();

        let timeout = self.config.shutdown_timeout;
        let agents = std::mem::take(&mut self.agents);

        let join_all = async {
            for (_id, spawned) in agents {
                let _ = spawned.join.await;
            }
        };

        let _ = tokio::time::timeout(timeout, join_all).await;
    }
}
