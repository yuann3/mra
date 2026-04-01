//! Typed builder for wiring an agent's dependencies and spawning
//! the handle + runner pair.
//!
//! Only a name and behavior are required. Optional setters inject
//! LLM, tools, budget, peers, and cancellation tokens.

use std::collections::HashMap;
use std::sync::Arc;

use tokio_util::sync::CancellationToken;

use crate::budget::BudgetTracker;
use crate::config::AgentConfig;
use crate::ids::AgentId;
use crate::llm::LlmProvider;
use crate::supervisor::child::{ChildContext, SpawnedChild};
use crate::tool::ToolRegistry;

use super::AgentBehavior;
use super::handle::AgentHandle;
use super::runner::SpawnedAgent;

/// Builder for spawning agents with sensible defaults.
///
/// Only a name and behavior are required — everything else defaults
/// to empty/None. Use optional setters to override.
///
/// Two terminal methods:
/// - [`.spawn()`](Self::spawn) — standalone Tokio task
/// - [`.spawn_child()`](Self::spawn_child) — unspawned future for supervisor
#[must_use]
pub struct AgentSpawn<B> {
    id: AgentId,
    config: AgentConfig,
    behavior: B,
    peers: HashMap<String, AgentHandle>,
    llm: Option<Arc<dyn LlmProvider>>,
    cancel: CancellationToken,
    budget: Option<Arc<BudgetTracker>>,
    tools: ToolRegistry,
}

impl<B: AgentBehavior> AgentSpawn<B> {
    /// Creates a builder with a name and behavior.
    ///
    /// Defaults: fresh `AgentId`, `AgentConfig::new(name)`, no peers,
    /// no LLM, fresh `CancellationToken`, no budget, empty tools.
    ///
    /// # Examples
    ///
    /// ```no_run
    /// # use mra::agent::{AgentBehavior, AgentCtx, AgentReply, AgentSpawn, Task};
    /// # use mra::error::AgentError;
    /// # struct Echo;
    /// # impl AgentBehavior for Echo {
    /// #     async fn handle(&mut self, _ctx: &mut AgentCtx, input: Task)
    /// #         -> Result<AgentReply, AgentError> {
    /// #         Ok(AgentReply { task_id: input.id, output: input.instruction,
    /// #             self_tokens: 0, total_tokens: 0 })
    /// #     }
    /// # }
    /// let spawned = AgentSpawn::new("echo", Echo).spawn();
    /// ```
    pub fn new(name: impl Into<String>, behavior: B) -> Self {
        let name = name.into();
        Self::from_config(AgentConfig::new(&name), behavior)
    }

    /// Creates a builder with a pre-built config and behavior.
    ///
    /// Use when you need to customize `AgentConfig` fields like
    /// `mailbox_size` or `restart_policy` beyond the defaults.
    pub fn from_config(config: AgentConfig, behavior: B) -> Self {
        Self {
            id: AgentId::new(),
            config,
            behavior,
            peers: HashMap::new(),
            llm: None,
            cancel: CancellationToken::new(),
            budget: None,
            tools: ToolRegistry::new(),
        }
    }

    /// Applies all fields from a [`ChildContext`], overriding id,
    /// cancel, peers, llm, budget, and tools.
    ///
    /// Used internally by [`ChildSpec::from_behavior`](crate::supervisor::ChildSpec::from_behavior).
    pub(crate) fn with_child_ctx(mut self, ctx: ChildContext) -> Self {
        self.id = ctx.id;
        self.cancel = ctx.cancel;
        self.peers = ctx.peers;
        self.llm = ctx.llm;
        self.budget = ctx.budget;
        self.tools = ctx.tools;
        self
    }

    /// Overrides the agent's unique identifier.
    pub fn id(mut self, id: AgentId) -> Self {
        self.id = id;
        self
    }

    /// Injects named peer handles for inter-agent delegation.
    pub fn peers(mut self, peers: HashMap<String, AgentHandle>) -> Self {
        self.peers = peers;
        self
    }

    /// Sets the shared LLM provider.
    pub fn llm(mut self, llm: Arc<dyn LlmProvider>) -> Self {
        self.llm = Some(llm);
        self
    }

    /// Sets the cancellation token.
    pub fn cancel(mut self, cancel: CancellationToken) -> Self {
        self.cancel = cancel;
        self
    }

    /// Sets the shared budget tracker.
    pub fn budget(mut self, budget: Arc<BudgetTracker>) -> Self {
        self.budget = Some(budget);
        self
    }

    /// Sets the shared LLM provider from an `Option`.
    ///
    /// Useful when forwarding from a [`ChildContext`] where the LLM may be `None`.
    pub fn llm_opt(mut self, llm: Option<Arc<dyn LlmProvider>>) -> Self {
        self.llm = llm;
        self
    }

    /// Sets the shared budget tracker from an `Option`.
    ///
    /// Useful when forwarding from a [`ChildContext`] where the budget may be `None`.
    pub fn budget_opt(mut self, budget: Option<Arc<BudgetTracker>>) -> Self {
        self.budget = budget;
        self
    }

    /// Replaces the tool registry.
    pub fn tools(mut self, tools: ToolRegistry) -> Self {
        self.tools = tools;
        self
    }

    /// Spawns the agent as an independent Tokio task.
    #[allow(deprecated)]
    pub fn spawn(self) -> SpawnedAgent {
        AgentHandle::spawn(
            self.id,
            self.config,
            self.behavior,
            self.peers,
            self.llm,
            self.cancel,
            self.budget,
            self.tools,
        )
    }

    /// Creates the agent without spawning a Tokio task.
    ///
    /// Returns a [`SpawnedChild`] whose future the supervisor
    /// spawns via its own `JoinSet`.
    #[allow(deprecated)]
    pub fn spawn_child(self) -> SpawnedChild {
        AgentHandle::spawn_child(
            self.id,
            self.config,
            self.behavior,
            self.peers,
            self.llm,
            self.cancel,
            self.budget,
            self.tools,
        )
    }
}
