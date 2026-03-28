use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::{broadcast, mpsc, watch};
use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;

use crate::agent::AgentHandle;
use crate::agent::ProgressState;
use crate::agent::mailbox::MailboxSlot;
use crate::budget::BudgetTracker;
use crate::error::SupervisorError;
use crate::ids::AgentId;
use crate::tool::ToolRegistry;

use super::ChildExit;
use super::child::{ChildContext, ChildSpec};
use super::config::{ChildRestart, Strategy, SupervisorConfig};
use super::event::SupervisorEvent;
use super::handle::SupervisorCommand;
use super::restart_manager::{RestartDecision, RestartManager};
use super::tracker::{IntensityTracker, RestartTracker};

struct ChildState {
    spec: ChildSpec,
    id: AgentId,
    generation: u64,
    progress: Option<watch::Receiver<ProgressState>>,
    child_cancel: Option<CancellationToken>,
    mailbox: Arc<MailboxSlot>,
    logical_cancel: CancellationToken,
    alive: bool,
    hung: bool,
}

#[derive(Debug)]
struct PendingRestart {
    name: String,
    when: tokio::time::Instant,
    old_gen: u64,
}

pub(crate) struct SupervisorRunner {
    config: SupervisorConfig,
    children: HashMap<String, ChildState>,
    child_order: Vec<String>,
    task_map: HashMap<tokio::task::Id, String>,
    join_set: JoinSet<ChildExit>,
    command_rx: mpsc::Receiver<SupervisorCommand>,
    event_tx: broadcast::Sender<SupervisorEvent>,
    cancel: CancellationToken,
    restart_mgr: RestartManager,
    pending_restarts: Vec<PendingRestart>,
    budget: Option<Arc<BudgetTracker>>,
}

impl SupervisorRunner {
    pub(crate) fn new(
        config: SupervisorConfig,
        command_rx: mpsc::Receiver<SupervisorCommand>,
        event_tx: broadcast::Sender<SupervisorEvent>,
        budget: Option<Arc<BudgetTracker>>,
    ) -> Self {
        let restart_mgr = RestartManager::new(&config);
        Self {
            config,
            children: HashMap::new(),
            child_order: Vec::new(),
            task_map: HashMap::new(),
            join_set: JoinSet::new(),
            command_rx,
            event_tx,
            cancel: CancellationToken::new(),
            restart_mgr,
            pending_restarts: Vec::new(),
            budget,
        }
    }

    pub(crate) async fn run(mut self) -> Result<(), SupervisorError> {
        let mut hang_tick = tokio::time::interval(self.config.hang_check_interval);
        hang_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

        self.emit(SupervisorEvent::SupervisorStarted);

        loop {
            tokio::select! {
                _ = self.cancel.cancelled() => {
                    self.drain_all().await;
                    break Ok(());
                }

                Some(result) = self.join_set.join_next_with_id() => {
                    self.handle_child_exit(result).await?;
                }

                cmd = self.command_rx.recv() => match cmd {
                    None => {
                        self.drain_all().await;
                        break Ok(());
                    }
                    Some(cmd) => self.handle_command(cmd).await?,
                },

                _ = hang_tick.tick() => {
                    self.check_hangs().await;
                }
            }
        }
    }

    fn emit(&self, event: SupervisorEvent) {
        let _ = self.event_tx.send(event);
    }

    async fn handle_command(&mut self, cmd: SupervisorCommand) -> Result<(), SupervisorError> {
        match cmd {
            SupervisorCommand::StartChild { spec, reply } => {
                let result = self.do_start_child(spec).await;
                let _ = reply.send(result);
            }
            SupervisorCommand::StopChild { name, reply } => {
                let result = self.do_stop_child(&name).await;
                let _ = reply.send(result);
            }
            SupervisorCommand::GetChild { name, reply } => {
                let handle = self.children.get(&name).map(|child| {
                    AgentHandle::new(
                        child.id,
                        child.mailbox.clone(),
                        child.logical_cancel.clone(),
                    )
                });
                let _ = reply.send(handle);
            }
            SupervisorCommand::Shutdown => {
                self.drain_all().await;
                self.cancel.cancel();
            }
        }
        Ok(())
    }

    async fn do_start_child(&mut self, spec: ChildSpec) -> Result<AgentHandle, SupervisorError> {
        let name = spec.name.clone();
        let id = AgentId::new();
        let logical_cancel = CancellationToken::new();
        let child_cancel = logical_cancel.child_token();

        // Create stable mailbox with dummy sender
        let (dummy_tx, _) = mpsc::channel(1);
        let mailbox = Arc::new(MailboxSlot::new(dummy_tx));

        // Build peers map from existing alive children
        let peers: HashMap<String, AgentHandle> = self
            .children
            .iter()
            .filter(|(_, c)| c.alive)
            .map(|(n, c)| {
                (
                    n.clone(),
                    AgentHandle::new(c.id, c.mailbox.clone(), c.logical_cancel.clone()),
                )
            })
            .collect();

        // Register agent budget slot if budget tracking is active
        if let Some(ref budget) = self.budget {
            budget.register_agent(&name, spec.token_budget);
        }

        // Call factory
        let ctx = ChildContext {
            id,
            generation: 0,
            cancel: child_cancel.clone(),
            peers,
            llm: None,
            budget: self.budget.clone(),
            tools: ToolRegistry::new(),
        };
        let spawned = (spec.factory)(ctx)
            .await
            .map_err(|e| SupervisorError::SpawnFailed(e.to_string()))?;

        // Swap real sender into stable mailbox
        mailbox.swap(spawned.sender);

        // Build the stable handle
        let handle = AgentHandle::new(id, mailbox.clone(), logical_cancel.clone());

        // Spawn future in JoinSet
        let abort = self.join_set.spawn(spawned.future);
        let task_id = abort.id();
        self.task_map.insert(task_id, name.clone());

        let state = ChildState {
            spec,
            id,
            generation: 0,
            progress: Some(spawned.progress),
            child_cancel: Some(child_cancel),
            mailbox,
            logical_cancel,
            alive: true,
            hung: false,
        };

        // Register with restart manager
        self.restart_mgr.register(&name, state.spec.restart, &state.spec.config.restart_policy);

        self.children.insert(name.clone(), state);
        self.child_order.push(name.clone());

        self.emit(SupervisorEvent::ChildStarted {
            name,
            generation: 0,
        });

        Ok(handle)
    }

    async fn do_stop_child(&mut self, name: &str) -> Result<(), SupervisorError> {
        let child = self
            .children
            .get_mut(name)
            .ok_or_else(|| SupervisorError::ChildNotFound(name.into()))?;

        if let Some(cancel) = child.child_cancel.take() {
            cancel.cancel();
        }
        child.alive = false;
        Ok(())
    }

    async fn handle_child_exit(
        &mut self,
        result: Result<(tokio::task::Id, ChildExit), tokio::task::JoinError>,
    ) -> Result<(), SupervisorError> {
        let (task_id, exit) = match result {
            Ok((id, exit)) => (id, exit),
            Err(e) => {
                let id = e.id();
                (id, ChildExit::Failed(format!("task panicked: {e}")))
            }
        };

        let Some(name) = self.task_map.remove(&task_id) else {
            return Ok(());
        };

        let (old_gen, hung) = {
            let Some(child) = self.children.get_mut(&name) else {
                return Ok(());
            };

            let old_gen = child.generation;
            child.alive = false;
            child.child_cancel = None;
            child.progress = None;

            let hung = child.hung;
            child.hung = false;

            (old_gen, hung)
        };

        self.emit(SupervisorEvent::ChildExited {
            name: name.clone(),
            generation: old_gen,
            exit: exit.clone(),
        });

        if self.cancel.is_cancelled() {
            return Ok(());
        }

        // Delegate restart decision to RestartManager
        let now = tokio::time::Instant::now();
        let decision = self.restart_mgr.decide(&name, &exit, hung, now);

        match decision {
            RestartDecision::NoRestart => Ok(()),

            RestartDecision::RestartAfter { delay } => {
                // Schedule non-blocking restart
                self.pending_restarts.push(PendingRestart {
                    name,
                    when: now + delay,
                    old_gen,
                });
                Ok(())
            }

            RestartDecision::RestartAll => {
                self.restart_all(&name).await
            }

            RestartDecision::ChildLimitExceeded { restarts } => {
                self.emit(SupervisorEvent::ChildRestartLimitExceeded {
                    name,
                    restarts,
                });
                Ok(())
            }

            RestartDecision::IntensityExceeded { total_restarts } => {
                self.emit(SupervisorEvent::RestartIntensityExceeded { total_restarts });
                self.drain_all().await;
                Err(SupervisorError::RestartIntensityExceeded { total_restarts })
            }
        }
    }

    async fn restart_all(&mut self, trigger_name: &str) -> Result<(), SupervisorError> {
        // 1. Cancel all alive children (except the one that already exited)
        for (name, child) in &self.children {
            if name != trigger_name
                && let Some(ref cancel) = child.child_cancel
            {
                cancel.cancel();
            }
        }

        // 2. Wait for all to exit
        while self.join_set.join_next().await.is_some() {}
        self.task_map.clear();

        // 3. Record restart in per-supervisor intensity tracker
        let now = tokio::time::Instant::now();
        self.intensity.record(now);
        if self.intensity.exceeded() {
            let total = self.intensity.total_restarts;
            let _ = self
                .event_tx
                .send(SupervisorEvent::RestartIntensityExceeded {
                    total_restarts: total,
                });
            self.drain_all().await;
            return Err(SupervisorError::RestartIntensityExceeded {
                total_restarts: total,
            });
        }

        // 4. Respawn all non-Temporary children in insertion order
        let order = self.child_order.clone();
        for child_name in &order {
            // Skip Temporary children
            {
                let child = self.children.get_mut(child_name).unwrap();
                if matches!(child.spec.restart, ChildRestart::Temporary) {
                    child.alive = false;
                    continue;
                }
            }

            if self.cancel.is_cancelled() {
                return Ok(());
            }

            let (old_gen, new_gen, child_cancel, child_id) = {
                let child = self.children.get(child_name).unwrap();
                let old_gen = child.generation;
                let new_gen = old_gen + 1;
                let child_cancel = child.logical_cancel.child_token();
                (old_gen, new_gen, child_cancel, child.id)
            };

            // Build peers from already-respawned siblings
            let peers: HashMap<String, AgentHandle> = self
                .children
                .iter()
                .filter(|(n, c)| c.alive && *n != child_name)
                .map(|(n, c)| {
                    (
                        n.clone(),
                        AgentHandle::new(c.id, c.mailbox.clone(), c.logical_cancel.clone()),
                    )
                })
                .collect();

            let child = self.children.get_mut(child_name).unwrap();
            let ctx = ChildContext {
                id: child_id,
                generation: new_gen,
                cancel: child_cancel.clone(),
                peers,
                llm: None,
                budget: self.budget.clone(),
                tools: ToolRegistry::new(),
            };

            let spawned = match (child.spec.factory)(ctx).await {
                Ok(s) => s,
                Err(_) => {
                    child.alive = false;
                    continue;
                }
            };

            child.mailbox.swap(spawned.sender);
            let abort = self.join_set.spawn(spawned.future);
            self.task_map.insert(abort.id(), child_name.clone());

            child.generation = new_gen;
            child.progress = Some(spawned.progress);
            child.child_cancel = Some(child_cancel);
            child.alive = true;
            child.tracker.record(now);

            let _ = self.event_tx.send(SupervisorEvent::ChildRestarted {
                name: child_name.clone(),
                old_gen,
                new_gen,
                delay: Duration::from_millis(0),
            });
        }

        Ok(())
    }

    async fn drain_all(&mut self) {
        self.emit(SupervisorEvent::SupervisorStopping);
        for child in self.children.values() {
            if let Some(ref cancel) = child.child_cancel {
                cancel.cancel();
            }
        }
        while self.join_set.join_next().await.is_some() {}
    }

    async fn check_hangs(&mut self) {
        let mut hangs = Vec::new();

        for (name, child) in &self.children {
            if !child.alive {
                continue;
            }
            let Some(hang_timeout) = child.spec.hang_timeout else {
                continue;
            };
            let Some(ref progress_rx) = child.progress else {
                continue;
            };

            let progress = progress_rx.borrow();
            if progress.busy && progress.last_progress.elapsed() > hang_timeout {
                let elapsed = progress.last_progress.elapsed();
                hangs.push((name.clone(), child.generation, elapsed));
            }
        }

        for (name, generation, elapsed) in hangs {
            let _ = self.event_tx.send(SupervisorEvent::HangDetected {
                name: name.clone(),
                generation,
                elapsed,
            });

            if let Some(child) = self.children.get_mut(&name)
                && let Some(cancel) = child.child_cancel.take()
            {
                child.hung = true;
                cancel.cancel();
            }
        }
    }
}
