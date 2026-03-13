use std::collections::HashMap;
use std::sync::Arc;

use tokio::sync::watch;

use crate::budget::BudgetTracker;
use crate::error::AgentError;
use crate::ids::AgentId;
use crate::llm::{LlmProvider, LlmRequest, LlmResponse};

use super::handle::AgentHandle;
use super::runner::ProgressState;

/// Runtime context passed to [`AgentBehavior::handle`](super::AgentBehavior::handle).
///
/// Provides access to the agent's identity, named peer handles for
/// delegation, an optional shared LLM provider, and budget tracking.
pub struct AgentCtx {
    pub id: AgentId,
    /// Human-readable name, used as the key for budget tracking.
    pub name: String,
    /// Named handles to peer agents, injected at spawn time.
    /// Agents call `ctx.peers["writer"].execute(task)` to delegate.
    pub peers: HashMap<String, AgentHandle>,
    /// Shared LLM provider. `None` for agents that don't need LLM access.
    /// Private to enforce budget tracking via [`Self::chat()`].
    pub(crate) llm: Option<Arc<dyn LlmProvider>>,
    /// Shared budget tracker. `None` if no budget is configured.
    pub(crate) budget: Option<Arc<BudgetTracker>>,
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

    /// Call LLM with automatic budget enforcement.
    ///
    /// Pre-checks whether the budget has already been tripped before
    /// calling the LLM, then charges this agent's direct token usage
    /// against both per-agent and global budgets.
    ///
    /// Returns `Err(AgentError::BudgetExceeded)` if the budget was
    /// already tripped or if this call crosses a limit.
    pub async fn chat(&self, request: &LlmRequest) -> Result<LlmResponse, AgentError> {
        if let Some(ref budget) = self.budget
            && budget.is_exceeded(&self.name)
        {
            return Err(AgentError::BudgetExceeded);
        }

        let llm = self.llm.as_ref().expect("no llm configured");
        let response = llm.chat(request).await.map_err(AgentError::Llm)?;

        if let Some(ref budget) = self.budget {
            budget.charge(&self.name, response.total_tokens())?;
        }

        Ok(response)
    }
}
