use std::collections::HashMap;
use std::sync::Arc;

use crate::ids::AgentId;
use crate::llm::LlmProvider;

use super::handle::AgentHandle;

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
}
