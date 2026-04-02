//! Configuration types for agents and the swarm runtime.

use std::time::Duration;

use figment::{
    Figment,
    providers::{Env, Format, Serialized, Toml},
};
use serde::Deserialize;

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
            mailbox_size: 8,
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

/// WASM sandbox configuration.
#[derive(Debug, Clone)]
pub struct WasmConfig {
    /// Path to the directory containing WASM tool subdirectories.
    pub tools_dir: std::path::PathBuf,
    /// Number of threads in the dedicated WASM thread pool.
    /// Defaults to the number of CPU cores.
    pub thread_pool_size: Option<usize>,
    /// Epoch tick interval in milliseconds. Defaults to 100.
    pub epoch_tick_ms: Option<u64>,
}

/// LLM provider configuration.
#[derive(Debug, Clone, Deserialize)]
pub struct LlmConfig {
    /// API key for the LLM provider.
    pub api_key: String,
    /// Default model identifier.
    pub model: String,
    /// Base URL for the provider's API.
    pub base_url: String,
}

/// File-friendly runtime configuration with seconds instead of `Duration`.
#[derive(Debug, Clone, Deserialize, serde::Serialize)]
pub struct RuntimeConfigFile {
    /// Maximum number of agents the runtime will accept.
    pub max_agents: usize,
    /// Shutdown timeout in seconds.
    pub shutdown_timeout_secs: u64,
}

/// Top-level configuration loaded from `mra.toml` + env vars.
#[derive(Debug, Clone, Deserialize)]
pub struct MraConfig {
    /// LLM provider settings.
    pub llm: LlmConfig,
    /// Runtime settings.
    pub runtime: RuntimeConfigFile,
}

#[derive(Debug, Clone, serde::Serialize, Deserialize)]
struct MraConfigDefaults {
    llm: LlmConfigDefaults,
    runtime: RuntimeConfigFile,
}

#[derive(Debug, Clone, serde::Serialize, Deserialize)]
struct LlmConfigDefaults {
    api_key: String,
    model: String,
    base_url: String,
}

impl Default for MraConfigDefaults {
    fn default() -> Self {
        Self {
            llm: LlmConfigDefaults {
                api_key: String::new(),
                model: "openai/gpt-4o-mini".into(),
                base_url: "https://openrouter.ai/api/v1".into(),
            },
            runtime: RuntimeConfigFile {
                max_agents: 100,
                shutdown_timeout_secs: 30,
            },
        }
    }
}

impl MraConfig {
    /// Loads config: defaults → `mra.toml` → `MRA_` env vars.
    #[allow(clippy::result_large_err)]
    pub fn load() -> Result<Self, figment::Error> {
        Figment::new()
            .merge(Serialized::defaults(MraConfigDefaults::default()))
            .merge(Toml::file("mra.toml"))
            .merge(Env::prefixed("MRA_").split("__"))
            .extract()
    }

    /// Returns a config with only hardcoded defaults.
    pub fn defaults() -> Self {
        let d = MraConfigDefaults::default();
        Self {
            llm: LlmConfig {
                api_key: d.llm.api_key,
                model: d.llm.model,
                base_url: d.llm.base_url,
            },
            runtime: d.runtime,
        }
    }

    /// Converts to the runtime's [`RuntimeConfig`].
    pub fn runtime_config(&self) -> RuntimeConfig {
        RuntimeConfig {
            max_agents: self.runtime.max_agents,
            shutdown_timeout: Duration::from_secs(self.runtime.shutdown_timeout_secs),
        }
    }
}
