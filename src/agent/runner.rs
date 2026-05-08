//! Internal event loop that receives messages and calls `AgentBehavior::handle`.
//!
//! Runs inside a supervisor `JoinSet`. Not public API — users interact
//! with agents through [`AgentHandle`](super::AgentHandle).

use std::collections::HashMap;
use std::sync::Arc;

use tokio::sync::{mpsc, watch};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use crate::budget::BudgetTracker;
use crate::config::AgentConfig;
use crate::error::{AgentError, ErrorClass};
use crate::ids::AgentId;
use crate::llm::LlmProvider;
use crate::supervisor::ChildExit;
use crate::supervisor::child::SpawnedChild;
use crate::tool::ToolRegistry;

use super::ctx::AgentCtx;
use super::handle::AgentHandle;
use super::mailbox::MailboxSlot;
use super::message::AgentMessage;

/// Snapshot of an agent's activity, published via a `watch` channel.
///
/// The supervisor reads this to distinguish idle agents (stale timestamp,
/// `busy == false`) from hung agents (stale timestamp, `busy == true`).
#[derive(Debug, Clone, Copy)]
pub struct ProgressState {
    /// When the agent last started or finished a task.
    pub last_progress: tokio::time::Instant,
    /// `true` while the agent is inside [`AgentBehavior::handle`](super::AgentBehavior::handle).
    pub busy: bool,
}

impl ProgressState {
    pub(crate) fn idle_now() -> Self {
        Self {
            last_progress: tokio::time::Instant::now(),
            busy: false,
        }
    }
}

/// Returned by [`crate::agent::AgentSpawn::spawn`]. Bundles everything needed to
/// interact with and monitor a running agent.
pub struct SpawnedAgent {
    /// Cloneable handle for sending tasks and shutdown signals.
    pub handle: AgentHandle,
    /// Watch receiver for the agent's [`ProgressState`].
    pub progress: watch::Receiver<ProgressState>,
    /// Tokio `JoinHandle` for the agent's background task.
    pub join: JoinHandle<ChildExit>,
}

/// All parameters needed to spawn an agent.
///
/// Behavior is stored as `Box<dyn DynAgentBehavior>` so that a single runner
/// implementation handles both the generic (`AgentSpawn<B>`) and type-erased
/// (`Runtime`) spawn paths.
pub(crate) struct AgentInit {
    pub id: AgentId,
    pub config: AgentConfig,
    pub behavior: Box<dyn super::DynAgentBehavior>,
    pub peers: HashMap<String, AgentHandle>,
    pub llm: Option<Arc<dyn LlmProvider>>,
    pub cancel: CancellationToken,
    pub budget: Option<Arc<BudgetTracker>>,
    pub tools: ToolRegistry,
    /// Per-agent model override. `None` → use whatever model is on the request.
    pub model: Option<String>,
    /// Role registry for system prompt overlays.
    pub role_registry: crate::runtime::roles::RoleRegistry,
}

/// The single agent event loop. Dispatches via `DynAgentBehavior::handle_dyn`.
struct AgentRunner {
    receiver: mpsc::Receiver<AgentMessage>,
    behavior: Box<dyn super::DynAgentBehavior>,
    ctx: AgentCtx,
    cancel: CancellationToken,
}

impl AgentRunner {
    async fn run(mut self) -> ChildExit {
        loop {
            tokio::select! {
                biased;

                _ = self.cancel.cancelled() => return ChildExit::Shutdown,

                msg = self.receiver.recv() => match msg {
                    None => return ChildExit::Normal,
                    Some(AgentMessage::Execute { mut task, respond_to }) => {
                        // Inject session context from the task into the shared ctx.
                        // Runtime loads history before dispatch; ctx.chat() saves it back.
                        self.ctx.session_id = task.session_id.take();
                        self.ctx.history = std::mem::take(&mut task.history);
                        self.ctx.session_store = task.session_store.take();
                        self.ctx.active_role = task.role.take();

                        let _ = self.ctx.progress_tx.send(ProgressState {
                            last_progress: tokio::time::Instant::now(),
                            busy: true,
                        });

                        let result = tokio::select! {
                            biased;
                            _ = self.cancel.cancelled() => Err(AgentError::Cancelled),
                            res = self.behavior.handle_dyn(&mut self.ctx, task) => res,
                        };

                        let _ = self.ctx.progress_tx.send(ProgressState {
                            last_progress: tokio::time::Instant::now(),
                            busy: false,
                        });

                        let is_budget = result.as_ref().is_err_and(|e| {
                            e.classification() == ErrorClass::BudgetExceeded
                        });

                        if respond_to.send(result).is_err() {
                            tracing::debug!(
                                agent = %self.ctx.name,
                                "task response dropped: caller's receiver was closed"
                            );
                        }

                        if is_budget {
                            return ChildExit::BudgetExceeded;
                        }
                    }
                    Some(AgentMessage::Shutdown { deadline }) => {
                        self.receiver.close();
                        self.drain_until(deadline).await;
                        return ChildExit::Shutdown;
                    }
                },
            }
        }
    }

    async fn drain_until(&mut self, deadline: tokio::time::Instant) {
        loop {
            tokio::select! {
                biased;

                _ = tokio::time::sleep_until(deadline) => {
                    self.fail_remaining();
                    break;
                }

                msg = self.receiver.recv() => match msg {
                    None => break,
                    Some(AgentMessage::Execute { mut task, respond_to }) => {
                        self.ctx.session_id = task.session_id.take();
                        self.ctx.history = std::mem::take(&mut task.history);
                        self.ctx.session_store = task.session_store.take();
                        self.ctx.active_role = task.role.take();

                        let _ = self.ctx.progress_tx.send(ProgressState {
                            last_progress: tokio::time::Instant::now(),
                            busy: true,
                        });

                        let result = self.behavior.handle_dyn(&mut self.ctx, task).await;

                        let _ = self.ctx.progress_tx.send(ProgressState {
                            last_progress: tokio::time::Instant::now(),
                            busy: false,
                        });

                        if respond_to.send(result).is_err() {
                            tracing::debug!(
                                agent = %self.ctx.name,
                                "drain response dropped: caller's receiver was closed"
                            );
                        }
                    }
                    Some(AgentMessage::Shutdown { .. }) => {}
                },
            }
        }
    }

    fn fail_remaining(&mut self) {
        while let Ok(msg) = self.receiver.try_recv() {
            if let AgentMessage::Execute { respond_to, .. } = msg
                && respond_to.send(Err(AgentError::Cancelled)).is_err()
            {
                tracing::debug!(
                    agent = %self.ctx.name,
                    "fail_remaining response dropped: caller's receiver was closed"
                );
            }
        }
    }
}

fn prepare_agent(
    init: AgentInit,
) -> (
    mpsc::Sender<AgentMessage>,
    Arc<MailboxSlot>,
    watch::Receiver<ProgressState>,
    AgentRunner,
) {
    let (sender, receiver) = mpsc::channel(init.config.mailbox_size);
    let mailbox = Arc::new(MailboxSlot::new(sender.clone()));
    let (progress_tx, progress_rx) = watch::channel(ProgressState::idle_now());

    let ctx = AgentCtx {
        id: init.id,
        name: init.config.name,
        peers: init.peers,
        llm: init.llm,
        budget: init.budget,
        progress_tx,
        tools: init.tools,
        model: init.model,
        history: Vec::new(),
        session_id: None,
        session_store: None,
        role_registry: init.role_registry,
        active_role: None,
    };

    let runner = AgentRunner {
        receiver,
        behavior: init.behavior,
        ctx,
        cancel: init.cancel,
    };

    (sender, mailbox, progress_rx, runner)
}

impl AgentHandle {
    /// Spawns an agent as an independent Tokio task.
    pub(crate) fn spawn_with_init(init: AgentInit) -> SpawnedAgent {
        let name = init.config.name.clone();
        let id = init.id;
        let cancel = init.cancel.clone();
        let (_sender, mailbox, progress, runner) = prepare_agent(init);
        let handle = AgentHandle::new(name, id, mailbox, cancel);
        let join = tokio::spawn(runner.run());
        SpawnedAgent {
            handle,
            progress,
            join,
        }
    }

    /// Creates an agent without spawning a Tokio task.
    ///
    /// Returns a [`SpawnedChild`] whose future the supervisor
    /// spawns via its own `JoinSet`.
    pub(crate) fn spawn_child_with_init(init: AgentInit) -> SpawnedChild {
        let (sender, _mailbox, progress, runner) = prepare_agent(init);
        SpawnedChild {
            future: Box::pin(runner.run()),
            progress,
            sender,
        }
    }
}
