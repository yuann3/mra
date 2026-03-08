use crate::ids::AgentId;

/// Runtime context passed to [`AgentBehavior::handle`](super::AgentBehavior::handle).
///
/// Provides access to the agent's identity and (in later tasks) LLM
/// clients, tool registries, budget meters, and the agent registry.
pub struct AgentCtx {
    pub id: AgentId,
}
