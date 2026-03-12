use std::collections::HashMap;
use std::sync::Arc;

use tokio::sync::watch;

use crate::ids::AgentId;
use crate::llm::LlmProvider;

use super::handle::AgentHandle;
use super::runner::ProgressState;

/// Runtime context passed to [`AgentBehavior::handle`](super::AgentBehavior::handle).
///
/// Provides access to the agent's identity, named peer handles for
/// delegation, and an optional shared LLM provider.
pub struct AgentCtx {
    pub id: AgentId,
    /// Named handles to peer agents, injected at spawn time.
    /// Agents call `ctx.peers["writer"].execute(task)` to delegate.
    pub peers: HashMap<String, AgentHandle>,
    /// Shared LLM provider. `None` for agents that don't need LLM access.
    pub llm: Option<Arc<dyn LlmProvider>>,
    /// Progress sender for cooperative heartbeat updates.
    pub(crate) progress_tx: watch::Sender<ProgressState>,
}

impl AgentCtx {
    /// Reports progress to the supervisor, resetting the hang-detection timer.
    ///
    /// Call this during long operations (e.g. LLM calls, tool invocations)
    /// to prevent the supervisor from treating the agent as hung.
    pub fn report_progress(&self) {
        let _ = self.progress_tx.send(ProgressState {
            last_progress: tokio::time::Instant::now(),
            busy: true,
        });
    }
}
