use std::sync::Arc;
use std::time::Duration;

use crate::budget::BudgetTracker;
use crate::llm::LlmProvider;
use crate::tool::ToolRegistry;

#[derive(Debug, Clone, Copy)]
pub enum Strategy {
    OneForOne,
    OneForAll,
}

#[derive(Debug, Clone, Copy, Default)]
pub enum ChildRestart {
    Permanent,
    #[default]
    Transient,
    Temporary,
}

impl ChildRestart {
    pub fn should_restart(&self, is_failure: bool) -> bool {
        match self {
            Self::Permanent => true,
            Self::Transient => is_failure,
            Self::Temporary => false,
        }
    }
}

#[derive(Debug, Clone)]
pub struct ShutdownPolicy {
    pub grace: Duration,
}

impl Default for ShutdownPolicy {
    fn default() -> Self {
        Self {
            grace: Duration::from_secs(5),
        }
    }
}

#[derive(Debug, Clone)]
pub struct RestartIntensity {
    pub max_restarts: u32,
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

#[derive(Clone)]
pub struct SupervisorConfig {
    pub strategy: Strategy,
    pub intensity: RestartIntensity,
    pub hang_check_interval: Duration,
    pub event_capacity: usize,
    /// Shared LLM provider injected into all children.
    pub llm: Option<Arc<dyn LlmProvider>>,
    /// Shared tool registry injected into all children.
    pub tools: ToolRegistry,
    /// Shared budget tracker injected into all children.
    pub budget: Option<Arc<BudgetTracker>>,
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_config_has_no_resources() {
        let config = SupervisorConfig::default();
        assert!(config.llm.is_none());
        assert!(config.budget.is_none());
    }

    #[test]
    fn with_budget_sets_budget() {
        let budget = Arc::new(BudgetTracker::builder().global_limit(1000).build_unconnected());
        let config = SupervisorConfig::default().with_budget(budget.clone());
        assert!(config.budget.is_some());
    }
}

impl SupervisorConfig {
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
