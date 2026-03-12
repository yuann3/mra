use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::{mpsc, watch};
use tokio_util::sync::CancellationToken;

use crate::agent::AgentHandle;
use crate::agent::ProgressState;
use crate::config::AgentConfig;
use crate::error::SupervisorError;
use crate::ids::AgentId;
use crate::llm::LlmProvider;

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
}

/// What a child factory returns on success.
///
/// Contains the future to spawn (supervisor will spawn via `JoinSet`),
/// the progress watch receiver, and the mpsc sender for the mailbox.
///
/// Constructed via [`AgentHandle::spawn_child`] — not directly by user code.
#[allow(dead_code)]
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
pub type ChildFactory = Arc<
    dyn Fn(ChildContext) -> Pin<Box<dyn Future<Output = Result<SpawnedChild, SupervisorError>> + Send>>
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
}

impl ChildSpec {
    /// Creates a new `ChildSpec` with sensible defaults.
    ///
    /// Defaults: `Transient` restart, 5-second shutdown grace, no hang timeout.
    pub fn new(name: impl Into<String>, config: AgentConfig, factory: ChildFactory) -> Self {
        Self {
            name: name.into(),
            config,
            factory,
            restart: ChildRestart::default(),
            shutdown_policy: ShutdownPolicy::default(),
            hang_timeout: None,
        }
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
}
