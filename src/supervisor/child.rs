use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::{mpsc, watch};
use tokio_util::sync::CancellationToken;

use crate::agent::AgentHandle;
use crate::agent::ProgressState;
use crate::budget::BudgetTracker;
use crate::config::AgentConfig;
use crate::error::SupervisorError;
use crate::ids::AgentId;
use crate::llm::LlmProvider;
use crate::tool::ToolRegistry;

use super::ChildExit;
use super::config::{ChildRestart, ShutdownPolicy};

use crate::agent::AgentMessage;

/// Context passed to a [`ChildFactory`] on each spawn/restart.
pub struct ChildContext {
    /// Stable logical id for this child.
    pub id: AgentId,
    /// Restart generation (0 = first start, 1 = first restart, …).
    pub generation: u64,
    /// Per-generation cancellation token.
    pub cancel: CancellationToken,
    /// Named peer handles.
    pub peers: HashMap<String, AgentHandle>,
    /// Shared LLM provider.
    pub llm: Option<Arc<dyn LlmProvider>>,
    /// Shared budget tracker.
    pub budget: Option<Arc<BudgetTracker>>,
    /// Tool registry for this agent.
    pub tools: ToolRegistry,
}

/// What a child factory returns on success.
///
/// Contains the future to spawn (supervisor will spawn via `JoinSet`),
/// the progress watch receiver, and the mpsc sender for the mailbox.
///
/// Constructed via [`AgentHandle::spawn_child`] — not directly by user code.
pub struct SpawnedChild {
    /// The agent's run future. Supervisor spawns this in its `JoinSet`.
    pub(crate) future: Pin<Box<dyn Future<Output = ChildExit> + Send>>,
    /// Watch receiver for the agent's progress state.
    pub(crate) progress: watch::Receiver<ProgressState>,
    /// The mpsc sender for the agent's mailbox.
    /// Supervisor swaps this into the stable `MailboxSlot`.
    pub(crate) sender: mpsc::Sender<AgentMessage>,
}

/// Async, fallible factory that produces a [`SpawnedChild`].
///
/// Called by the supervisor on initial start and on each restart.
///
/// # Deadlock warning
///
/// The factory runs **inside** the supervisor's event loop. It **must not**
/// call back into the supervisor (e.g. `supervisor.child()` or
/// `supervisor.start_child()`) — doing so will deadlock because the
/// supervisor cannot process commands while it is awaiting the factory.
pub type ChildFactory = Arc<
    dyn Fn(
            ChildContext,
        ) -> Pin<Box<dyn Future<Output = Result<SpawnedChild, SupervisorError>> + Send>>
        + Send
        + Sync,
>;

/// Specification for a supervised child agent.
pub struct ChildSpec {
    /// Human-readable name (unique within the supervisor).
    pub name: String,
    /// Agent configuration (mailbox size, restart policy, etc.).
    pub config: AgentConfig,
    /// Factory closure invoked on start and restart.
    pub factory: ChildFactory,
    /// Restart strategy for this child.
    pub restart: ChildRestart,
    /// Shutdown policy (grace period before hard-kill).
    pub shutdown_policy: ShutdownPolicy,
    /// Optional hang-detection timeout override.
    pub hang_timeout: Option<Duration>,
    /// Optional per-agent token budget.
    pub token_budget: Option<u64>,
}

impl SpawnedChild {
    /// Creates a `SpawnedChild` from a bare future.
    ///
    /// The mailbox sender and progress watch are dummies — useful for testing
    /// and for supervisors that manage non-agent children.
    pub fn from_future(future: Pin<Box<dyn Future<Output = ChildExit> + Send>>) -> Self {
        let (tx, _rx) = mpsc::channel(1);
        let (_ptx, prx) = watch::channel(crate::agent::ProgressState {
            last_progress: tokio::time::Instant::now(),
            busy: false,
        });
        Self {
            future,
            progress: prx,
            sender: tx,
        }
    }
}

impl ChildSpec {
    /// Creates a new `ChildSpec` with sensible defaults.
    ///
    /// Defaults: `Transient` restart, 5-second shutdown grace, no hang timeout.
    ///
    /// # Deadlock warning
    ///
    /// The `factory` closure runs inside the supervisor's event loop and
    /// **must not** call back into the supervisor. See [`ChildFactory`] docs.
    pub fn new(name: impl Into<String>, config: AgentConfig, factory: ChildFactory) -> Self {
        Self {
            name: name.into(),
            config,
            factory,
            restart: ChildRestart::default(),
            shutdown_policy: ShutdownPolicy::default(),
            hang_timeout: None,
            token_budget: None,
        }
    }

    /// Creates a `ChildSpec` from a closure that returns a behavior.
    ///
    /// This is the common case: the factory just needs to produce a fresh
    /// behavior value on each start/restart. All `ChildContext` fields
    /// (peers, llm, cancel, budget, tools) are forwarded automatically.
    ///
    /// The spec name is derived from `config.name` — single source of truth.
    ///
    /// **Note:** The factory captures a clone of `config` at construction time.
    /// Mutating `self.config` after construction will not affect the factory's
    /// copy. In practice this is not a problem because `ChildSpec` is consumed
    /// by [`SupervisorHandle::start_child`] and not mutated after construction.
    ///
    /// ```ignore
    /// ChildSpec::from_behavior(AgentConfig::new("echo"), |_| EchoBehavior)
    /// ```
    pub fn from_behavior<B, F>(config: AgentConfig, make_behavior: F) -> Self
    where
        B: crate::agent::AgentBehavior,
        F: Fn(&ChildContext) -> B + Send + Sync + 'static,
    {
        use crate::agent::AgentSpawn;

        let name = config.name.clone();
        Self::new(
            &name,
            config.clone(),
            Arc::new(move |ctx: ChildContext| {
                let behavior = make_behavior(&ctx);
                let config = config.clone();
                Box::pin(async move {
                    Ok(AgentSpawn::from_config(config, behavior)
                        .with_child_ctx(ctx)
                        .spawn_child())
                })
                    as Pin<
                        Box<
                            dyn Future<Output = Result<SpawnedChild, SupervisorError>>
                                + Send,
                        >,
                    >
            }),
        )
    }

    /// Sets the restart strategy.
    pub fn with_restart(mut self, restart: ChildRestart) -> Self {
        self.restart = restart;
        self
    }

    /// Sets the shutdown policy.
    pub fn with_shutdown_policy(mut self, policy: ShutdownPolicy) -> Self {
        self.shutdown_policy = policy;
        self
    }

    /// Sets the hang-detection timeout override.
    pub fn with_hang_timeout(mut self, timeout: Duration) -> Self {
        self.hang_timeout = Some(timeout);
        self
    }

    /// Sets the per-agent token budget.
    pub fn with_token_budget(mut self, limit: u64) -> Self {
        self.token_budget = Some(limit);
        self
    }
}
