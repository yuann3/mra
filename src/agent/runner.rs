use std::collections::HashMap;
use std::sync::Arc;

use tokio::sync::{mpsc, watch};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use crate::budget::BudgetTracker;
use crate::config::AgentConfig;
use crate::error::AgentError;
use crate::ids::AgentId;
use crate::llm::LlmProvider;
use crate::supervisor::ChildExit;
use crate::supervisor::child::SpawnedChild;
use crate::tool::ToolRegistry;

use super::AgentBehavior;
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
    /// `true` while the agent is inside [`AgentBehavior::handle`].
    pub busy: bool,
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

struct AgentRunner<B: AgentBehavior> {
    receiver: mpsc::Receiver<AgentMessage>,
    behavior: B,
    ctx: AgentCtx,
    cancel: CancellationToken,
}

impl<B: AgentBehavior> AgentRunner<B> {
    async fn run(mut self) -> ChildExit {
        loop {
            tokio::select! {
                biased;

                _ = self.cancel.cancelled() => return ChildExit::Shutdown,

                msg = self.receiver.recv() => match msg {
                    None => return ChildExit::Normal,
                    Some(AgentMessage::Execute { task, respond_to }) => {
                        let _ = self.ctx.progress_tx.send(ProgressState {
                            last_progress: tokio::time::Instant::now(),
                            busy: true,
                        });

                        let result = tokio::select! {
                            biased;
                            _ = self.cancel.cancelled() => Err(AgentError::Cancelled),
                            res = self.behavior.handle(&mut self.ctx, task) => res,
                        };

                        let _ = self.ctx.progress_tx.send(ProgressState {
                            last_progress: tokio::time::Instant::now(),
                            busy: false,
                        });

                        let _ = respond_to.send(result);
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
                    Some(AgentMessage::Execute { task, respond_to }) => {
                        let _ = self.ctx.progress_tx.send(ProgressState {
                            last_progress: tokio::time::Instant::now(),
                            busy: true,
                        });

                        let result = self.behavior.handle(&mut self.ctx, task).await;

                        let _ = self.ctx.progress_tx.send(ProgressState {
                            last_progress: tokio::time::Instant::now(),
                            busy: false,
                        });

                        let _ = respond_to.send(result);
                    }
                    Some(AgentMessage::Shutdown { .. }) => {}
                },
            }
        }
    }

    fn fail_remaining(&mut self) {
        while let Ok(msg) = self.receiver.try_recv() {
            if let AgentMessage::Execute { respond_to, .. } = msg {
                let _ = respond_to.send(Err(AgentError::Cancelled));
            }
        }
    }
}

impl AgentHandle {
    /// Spawns an agent as a Tokio task and returns a [`SpawnedAgent`].
    ///
    /// Creates a bounded `mpsc` channel (capacity from `config.mailbox_size`),
    /// a `watch` channel for [`ProgressState`], and spawns the internal
    /// runner loop. The runner is generic over `B` — no dynamic dispatch.
    ///
    /// # Deprecated
    ///
    /// Use [`AgentSpawn::new`](crate::agent::AgentSpawn::new) instead:
    /// ```ignore
    /// AgentSpawn::new("name", behavior).llm(llm).spawn()
    /// ```
    #[deprecated(note = "Use AgentSpawn::new(name, behavior).spawn() instead")]
    #[allow(clippy::too_many_arguments)]
    pub fn spawn<B: AgentBehavior>(
        id: AgentId,
        config: AgentConfig,
        behavior: B,
        peers: HashMap<String, AgentHandle>,
        llm: Option<Arc<dyn LlmProvider>>,
        cancel: CancellationToken,
        budget: Option<Arc<BudgetTracker>>,
        tools: ToolRegistry,
    ) -> SpawnedAgent {
        let (tx, rx) = mpsc::channel(config.mailbox_size);
        let mailbox = Arc::new(MailboxSlot::new(tx));
        let (progress_tx, progress_rx) = watch::channel(ProgressState {
            last_progress: tokio::time::Instant::now(),
            busy: false,
        });

        let handle = AgentHandle::new(id, mailbox, cancel.clone());

        let ctx = AgentCtx {
            id,
            name: config.name.clone(),
            peers,
            llm,
            budget,
            progress_tx,
            tools,
        };

        let runner = AgentRunner {
            receiver: rx,
            behavior,
            ctx,
            cancel,
        };

        let join = tokio::spawn(runner.run());

        SpawnedAgent {
            handle,
            progress: progress_rx,
            join,
        }
    }

    /// Creates an agent without spawning a Tokio task.
    ///
    /// Returns a [`SpawnedChild`] whose future the supervisor will spawn
    /// via its own `JoinSet`, giving it full control over task lifecycle.
    ///
    /// # Deprecated
    ///
    /// Use [`AgentSpawn::from_config`](crate::agent::AgentSpawn::from_config) instead:
    /// ```ignore
    /// AgentSpawn::from_config(config, behavior).spawn_child()
    /// ```
    #[deprecated(note = "Use AgentSpawn::from_config(config, behavior).spawn_child() instead")]
    #[allow(clippy::too_many_arguments)]
    pub fn spawn_child<B: AgentBehavior>(
        id: AgentId,
        config: AgentConfig,
        behavior: B,
        peers: HashMap<String, AgentHandle>,
        llm: Option<Arc<dyn LlmProvider>>,
        cancel: CancellationToken,
        budget: Option<Arc<BudgetTracker>>,
        tools: ToolRegistry,
    ) -> SpawnedChild {
        let (tx, rx) = mpsc::channel(config.mailbox_size);
        let (progress_tx, progress_rx) = watch::channel(ProgressState {
            last_progress: tokio::time::Instant::now(),
            busy: false,
        });

        let ctx = AgentCtx {
            id,
            name: config.name.clone(),
            peers,
            llm,
            budget,
            progress_tx,
            tools,
        };

        let runner = AgentRunner {
            receiver: rx,
            behavior,
            ctx,
            cancel,
        };

        SpawnedChild {
            future: Box::pin(runner.run()),
            progress: progress_rx,
            sender: tx,
        }
    }
}
