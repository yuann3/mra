//! Configuration types for agents and the swarm runtime.

use std::time::Duration;

/// Controls how the supervisor restarts a failed agent.
///
/// The supervisor tracks restart timestamps within a rolling [`window`](Self::window).
/// If the agent is restarted more than [`max_restarts`](Self::max_restarts) times
/// within that window, the supervisor gives up. Each restart waits for an
/// exponentially increasing backoff, capped at [`backoff_max`](Self::backoff_max).
#[derive(Debug, Clone)]
pub struct RestartPolicy {
    /// Maximum number of restarts allowed within `window` before giving up.
    pub max_restarts: u32,
    /// Rolling time window for counting restarts.
    pub window: Duration,
    /// Initial backoff duration after the first restart.
    pub backoff_base: Duration,
    /// Maximum backoff duration (caps exponential growth).
    pub backoff_max: Duration,
}

impl Default for RestartPolicy {
    fn default() -> Self {
        Self {
            max_restarts: 5,
            window: Duration::from_secs(60),
            backoff_base: Duration::from_secs(1),
            backoff_max: Duration::from_secs(30),
        }
    }
}

/// Per-agent configuration.
///
/// Created with [`AgentConfig::new`] and customized via builder methods.
///
/// # Example
///
/// ```
/// use mra::config::{AgentConfig, RestartPolicy};
///
/// let config = AgentConfig::new("researcher")
///     .with_mailbox_size(64)
///     .with_restart_policy(RestartPolicy {
///         max_restarts: 10,
///         ..Default::default()
///     });
/// ```
#[derive(Debug, Clone)]
pub struct AgentConfig {
    /// Human-readable name for logging and tracing.
    pub name: String,
    /// Bounded channel capacity for this agent's inbox.
    pub mailbox_size: usize,
    /// Supervisor restart behavior for this agent.
    pub restart_policy: RestartPolicy,
}

impl AgentConfig {
    /// Creates a new agent config with the given name and sensible defaults.
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            mailbox_size: 32,
            restart_policy: RestartPolicy::default(),
        }
    }

    /// Sets the bounded channel capacity for this agent's inbox.
    pub fn with_mailbox_size(mut self, size: usize) -> Self {
        self.mailbox_size = size;
        self
    }

    /// Sets the supervisor restart policy for this agent.
    pub fn with_restart_policy(mut self, policy: RestartPolicy) -> Self {
        self.restart_policy = policy;
        self
    }
}

/// Global runtime configuration for the swarm.
#[derive(Debug, Clone)]
pub struct RuntimeConfig {
    /// Maximum number of agents the runtime will accept.
    pub max_agents: usize,
    /// Hard timeout for graceful shutdown before aborting remaining tasks.
    pub shutdown_timeout: Duration,
}

impl Default for RuntimeConfig {
    fn default() -> Self {
        Self {
            max_agents: 100,
            shutdown_timeout: Duration::from_secs(30),
        }
    }
}
