//! Swarm runtime — thin wrapper around the root supervisor.

use std::sync::Arc;

use tokio::sync::broadcast;
use tokio::task::JoinHandle;

use crate::agent::AgentHandle;
use crate::budget::{AgentUsage, BudgetTracker, RunUsage};
use crate::error::SupervisorError;
use crate::supervisor::{ChildSpec, SupervisorConfig, SupervisorEvent, SupervisorHandle};

/// Manages a supervised set of agents.
///
/// Thin wrapper around a root [`SupervisorHandle`]. Agents are spawned
/// via [`ChildSpec`] factories and managed by the supervisor's restart
/// and hang-detection policies.
pub struct SwarmRuntime {
    supervisor: SupervisorHandle,
    join: JoinHandle<Result<(), SupervisorError>>,
    budget: Option<Arc<BudgetTracker>>,
    #[cfg(feature = "wasm")]
    _wasm_runtime: Option<Arc<crate::wasm::WasmRuntime>>,
}

impl SwarmRuntime {
    /// Creates a new runtime backed by a supervisor with the given config.
    ///
    /// # Examples
    ///
    /// ```no_run
    /// use mra::runtime::SwarmRuntime;
    /// use mra::supervisor::SupervisorConfig;
    ///
    /// # async fn example() {
    /// let runtime = SwarmRuntime::new(SupervisorConfig::default());
    /// runtime.shutdown().await;
    /// # }
    /// ```
    pub fn new(config: SupervisorConfig) -> Self {
        let (supervisor, join) = SupervisorHandle::start(config);
        Self {
            supervisor,
            join,
            budget: None,
            #[cfg(feature = "wasm")]
            _wasm_runtime: None,
        }
    }

    /// Creates a new runtime with a global token budget.
    ///
    /// When the budget is exceeded, agents receive
    /// `AgentError::BudgetExceeded` from
    /// [`AgentCtx::chat`](crate::agent::AgentCtx::chat).
    pub fn with_budget(config: SupervisorConfig, global_limit: u64) -> Self {
        let budget = Arc::new(
            BudgetTracker::builder()
                .global_limit(global_limit)
                .build_unconnected(),
        );
        let (supervisor, join) = SupervisorHandle::start_with_budget(config, Some(budget.clone()));
        Self {
            supervisor,
            join,
            budget: Some(budget),
            #[cfg(feature = "wasm")]
            _wasm_runtime: None,
        }
    }

    /// Loads WASM tools from the given config and registers them in the provided registry.
    ///
    /// Returns the number of tools loaded. The `WasmRuntime` is kept alive for
    /// the lifetime of this `SwarmRuntime`.
    #[cfg(feature = "wasm")]
    pub fn load_wasm_tools(
        &mut self,
        wasm_config: &crate::config::WasmConfig,
        registry: &mut crate::tool::ToolRegistry,
    ) -> Result<usize, crate::wasm::WasmError> {
        let pool_size = wasm_config.thread_pool_size.unwrap_or_else(num_cpus::get);
        let tick_ms = wasm_config
            .epoch_tick_ms
            .unwrap_or(crate::wasm::EPOCH_TICK_INTERVAL_MS);

        let runtime = Arc::new(crate::wasm::WasmRuntime::with_options(pool_size, tick_ms)?);

        let tools = runtime.load_tools(&wasm_config.tools_dir)?;
        let count = tools.len();

        for tool in tools {
            registry.register(Arc::new(tool))?;
        }

        self._wasm_runtime = Some(runtime);
        Ok(count)
    }

    /// Spawns a child agent via the supervisor.
    pub async fn spawn(&self, spec: ChildSpec) -> Result<AgentHandle, SupervisorError> {
        self.supervisor.start_child(spec).await
    }

    /// Looks up a child handle by name.
    pub async fn get_handle_by_name(&self, name: &str) -> Option<AgentHandle> {
        self.supervisor.child(name).await
    }

    /// Subscribes to supervisor events.
    pub fn subscribe(&self) -> broadcast::Receiver<SupervisorEvent> {
        self.supervisor.subscribe()
    }

    /// Returns current global token usage, if a budget is configured.
    pub fn token_usage(&self) -> Option<RunUsage> {
        self.budget.as_ref().map(|budget| budget.run_usage())
    }

    /// Returns per-agent token usage, if a budget is configured.
    pub fn agent_token_usage(&self, name: &str) -> Option<AgentUsage> {
        self.budget
            .as_ref()
            .and_then(|budget| budget.agent_usage(name))
    }

    /// Gracefully shuts down all agents and the supervisor.
    pub async fn shutdown(self) {
        self.supervisor.shutdown().await;
        let _ = self.join.await;
    }
}
