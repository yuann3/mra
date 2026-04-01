//! Configuration types for the supervisor.
//!
//! Restart strategy, per-child restart policy, supervisor-wide restart
//! intensity, shutdown grace period, and the [`SupervisorConfig`] builder.

use std::sync::Arc;
use std::time::Duration;

use crate::budget::BudgetTracker;
use crate::llm::LlmProvider;
use crate::tool::ToolRegistry;

/// Restart strategy applied when a child exits.
#[derive(Debug, Clone, Copy)]
pub enum Strategy {
    /// Only the failed child is restarted.
    OneForOne,
    /// All children are terminated and restarted when any one fails.
    OneForAll,
}

/// Per-child restart policy evaluated on exit.
#[derive(Debug, Clone, Copy, Default)]
pub enum ChildRestart {
    /// Always restart, regardless of exit reason.
    Permanent,
    /// Restart only on failure (not on normal exit or shutdown).
    #[default]
    Transient,
    /// Never restart. The child runs at most once.
    Temporary,
}

impl ChildRestart {
    /// Returns `true` if this policy says the child should be restarted
    /// given whether the exit was a failure.
    pub fn should_restart(&self, is_failure: bool) -> bool {
        match self {
            Self::Permanent => true,
            Self::Transient => is_failure,
            Self::Temporary => false,
        }
    }
}

/// Controls how long the supervisor waits for a child to drain work
/// before hard-killing it. Default grace period is 5 seconds.
#[derive(Debug, Clone)]
pub struct ShutdownPolicy {
    /// Time to wait after sending shutdown before cancelling the child.
    pub grace: Duration,
}

impl Default for ShutdownPolicy {
    fn default() -> Self {
        Self {
            grace: Duration::from_secs(5),
        }
    }
}

/// Supervisor-wide restart rate limit.
///
/// If more than `max_restarts` occur within `window`, the supervisor
/// shuts down all children and returns an error.
#[derive(Debug, Clone)]
pub struct RestartIntensity {
    /// Maximum number of restarts allowed within `window`.
    pub max_restarts: u32,
    /// Rolling time window for counting restarts.
    pub window: Duration,
}

impl Default for RestartIntensity {
    fn default() -> Self {
        Self {
            max_restarts: 10,
            window: Duration::from_secs(60),
        }
    }
}

/// Complete configuration for a supervisor instance.
///
/// Use [`SupervisorConfig::builder()`] to construct.
#[derive(Clone)]
pub struct SupervisorConfig {
    /// Restart strategy (`OneForOne` or `OneForAll`).
    pub strategy: Strategy,
    /// Supervisor-wide restart rate limit.
    pub intensity: RestartIntensity,
    /// How often the supervisor polls children for hang detection.
    pub hang_check_interval: Duration,
    /// Broadcast channel capacity for [`SupervisorEvent`](super::SupervisorEvent)s.
    pub event_capacity: usize,
    /// Shared LLM provider injected into all children.
    llm: Option<Arc<dyn LlmProvider>>,
    /// Shared tool registry injected into all children.
    tools: ToolRegistry,
    /// Shared budget tracker injected into all children.
    budget: Option<Arc<BudgetTracker>>,
}

impl Default for SupervisorConfig {
    fn default() -> Self {
        Self {
            strategy: Strategy::OneForOne,
            intensity: RestartIntensity::default(),
            hang_check_interval: Duration::from_secs(1),
            event_capacity: 64,
            llm: None,
            tools: ToolRegistry::new(),
            budget: None,
        }
    }
}

impl std::fmt::Debug for SupervisorConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SupervisorConfig")
            .field("strategy", &self.strategy)
            .field("intensity", &self.intensity)
            .field("hang_check_interval", &self.hang_check_interval)
            .field("event_capacity", &self.event_capacity)
            .field("llm", &self.llm.as_ref().map(|_| "..."))
            .field("budget", &self.budget.as_ref().map(|_| "..."))
            .finish()
    }
}

impl SupervisorConfig {
    /// Returns a builder pre-filled with default values.
    ///
    /// # Examples
    ///
    /// ```
    /// use std::time::Duration;
    /// use mra::supervisor::{SupervisorConfig, Strategy};
    ///
    /// let config = SupervisorConfig::builder()
    ///     .strategy(Strategy::OneForAll)
    ///     .hang_check_interval(Duration::from_secs(10))
    ///     .build();
    /// ```
    pub fn builder() -> SupervisorConfigBuilder {
        SupervisorConfigBuilder {
            inner: Self::default(),
        }
    }

    /// Returns the shared LLM provider, if configured.
    pub fn llm(&self) -> Option<&Arc<dyn LlmProvider>> {
        self.llm.as_ref()
    }

    /// Returns the shared tool registry.
    pub fn tools(&self) -> &ToolRegistry {
        &self.tools
    }

    /// Returns the shared budget tracker, if configured.
    pub fn budget(&self) -> Option<&Arc<BudgetTracker>> {
        self.budget.as_ref()
    }

    /// Sets the shared LLM provider for all children.
    pub fn with_llm(mut self, llm: Arc<dyn LlmProvider>) -> Self {
        self.llm = Some(llm);
        self
    }

    /// Sets the shared tool registry for all children.
    pub fn with_tools(mut self, tools: ToolRegistry) -> Self {
        self.tools = tools;
        self
    }

    /// Sets the shared budget tracker for all children.
    pub fn with_budget(mut self, budget: Arc<BudgetTracker>) -> Self {
        self.budget = Some(budget);
        self
    }
}

/// Builder for [`SupervisorConfig`] that avoids exhaustive struct literals.
pub struct SupervisorConfigBuilder {
    inner: SupervisorConfig,
}

impl SupervisorConfigBuilder {
    /// Sets the restart strategy (`OneForOne` or `OneForAll`).
    pub fn strategy(mut self, strategy: Strategy) -> Self {
        self.inner.strategy = strategy;
        self
    }

    /// Sets the restart intensity (max restarts within a time window).
    pub fn intensity(mut self, intensity: RestartIntensity) -> Self {
        self.inner.intensity = intensity;
        self
    }

    /// Sets how often the supervisor checks for hung children.
    pub fn hang_check_interval(mut self, interval: Duration) -> Self {
        self.inner.hang_check_interval = interval;
        self
    }

    /// Sets the broadcast channel capacity for supervisor events.
    pub fn event_capacity(mut self, capacity: usize) -> Self {
        self.inner.event_capacity = capacity;
        self
    }

    /// Sets the shared LLM provider injected into all children.
    pub fn llm(mut self, llm: Arc<dyn LlmProvider>) -> Self {
        self.inner.llm = Some(llm);
        self
    }

    /// Sets the shared tool registry injected into all children.
    pub fn tools(mut self, tools: ToolRegistry) -> Self {
        self.inner.tools = tools;
        self
    }

    /// Sets the shared budget tracker injected into all children.
    pub fn budget(mut self, budget: Arc<BudgetTracker>) -> Self {
        self.inner.budget = Some(budget);
        self
    }

    /// Builds the [`SupervisorConfig`].
    pub fn build(self) -> SupervisorConfig {
        self.inner
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_config_has_no_resources() {
        let config = SupervisorConfig::default();
        assert!(config.llm().is_none());
        assert!(config.budget().is_none());
    }

    #[test]
    fn with_budget_sets_budget() {
        let budget = Arc::new(
            BudgetTracker::builder()
                .global_limit(1000)
                .build_unconnected(),
        );
        let config = SupervisorConfig::default().with_budget(budget.clone());
        assert!(config.budget().is_some());
    }
}
