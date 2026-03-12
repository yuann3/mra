use std::collections::HashMap;
use std::sync::Arc;

use tokio::sync::{broadcast, mpsc, watch};
use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;

use crate::agent::mailbox::MailboxSlot;
use crate::agent::AgentHandle;
use crate::agent::ProgressState;
use crate::error::SupervisorError;
use crate::ids::AgentId;

use super::child::{ChildContext, ChildSpec};
use super::config::SupervisorConfig;
use super::event::SupervisorEvent;
use super::handle::SupervisorCommand;
use super::tracker::{IntensityTracker, RestartTracker};
use super::ChildExit;

#[allow(dead_code)]
struct ChildState {
    spec: ChildSpec,
    id: AgentId,
    generation: u64,
    progress: Option<watch::Receiver<ProgressState>>,
    child_cancel: Option<CancellationToken>,
    mailbox: Arc<MailboxSlot>,
    logical_cancel: CancellationToken,
    tracker: RestartTracker,
    alive: bool,
}

#[allow(dead_code)]
pub(crate) struct SupervisorRunner {
    config: SupervisorConfig,
    children: HashMap<String, ChildState>,
    child_order: Vec<String>,
    task_map: HashMap<tokio::task::Id, String>,
    join_set: JoinSet<ChildExit>,
    command_rx: mpsc::Receiver<SupervisorCommand>,
    event_tx: broadcast::Sender<SupervisorEvent>,
    cancel: CancellationToken,
    intensity: IntensityTracker,
}

impl SupervisorRunner {
    pub(crate) fn new(
        config: SupervisorConfig,
        command_rx: mpsc::Receiver<SupervisorCommand>,
        event_tx: broadcast::Sender<SupervisorEvent>,
    ) -> Self {
        let intensity = IntensityTracker::new(config.intensity.clone());
        Self {
            config,
            children: HashMap::new(),
            child_order: Vec::new(),
            task_map: HashMap::new(),
            join_set: JoinSet::new(),
            command_rx,
            event_tx,
            cancel: CancellationToken::new(),
            intensity,
        }
    }

    pub(crate) async fn run(mut self) -> Result<(), SupervisorError> {
        let mut hang_tick = tokio::time::interval(self.config.hang_check_interval);
        hang_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

        self.emit(SupervisorEvent::SupervisorStarted);

        loop {
            tokio::select! {
                biased;

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

        // Call factory
        let ctx = ChildContext {
            id,
            generation: 0,
            cancel: child_cancel.clone(),
            peers: HashMap::new(),
            llm: None,
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

        let tracker = RestartTracker::new(&spec.config.restart_policy);

        let state = ChildState {
            spec,
            id,
            generation: 0,
            progress: Some(spawned.progress),
            child_cancel: Some(child_cancel),
            mailbox,
            logical_cancel,
            tracker,
            alive: true,
        };

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

        let Some(child) = self.children.get_mut(&name) else {
            return Ok(());
        };

        let old_gen = child.generation;
        child.alive = false;
        child.child_cancel = None;
        child.progress = None;

        let is_failure = exit.is_failure();
        let should_restart = child.spec.restart.should_restart(is_failure);

        let _ = self.event_tx.send(SupervisorEvent::ChildExited {
            name: name.clone(),
            generation: old_gen,
            exit,
        });

        if !should_restart || self.cancel.is_cancelled() {
            return Ok(());
        }

        // Record in per-child tracker
        let now = tokio::time::Instant::now();
        child.tracker.record(now);

        if child.tracker.exceeded() {
            let restarts = child.tracker.total_restarts;
            let _ = self.event_tx.send(SupervisorEvent::ChildRestartLimitExceeded {
                name,
                restarts,
            });
            return Ok(());
        }

        // Record in per-supervisor intensity tracker
        self.intensity.record(now);
        if self.intensity.exceeded() {
            let total_restarts = self.intensity.total_restarts;
            let _ = self.event_tx.send(SupervisorEvent::RestartIntensityExceeded { total_restarts });
            return Err(SupervisorError::RestartIntensityExceeded { total_restarts });
        }

        // Compute backoff delay and sleep
        let delay = child.tracker.backoff_delay();
        tokio::time::sleep(delay).await;

        // Don't restart if cancelled during backoff sleep
        if self.cancel.is_cancelled() {
            return Ok(());
        }

        // Increment generation and create new per-generation cancel token
        let new_gen = old_gen + 1;
        let child_cancel = child.logical_cancel.child_token();

        let ctx = ChildContext {
            id: child.id,
            generation: new_gen,
            cancel: child_cancel.clone(),
            peers: HashMap::new(),
            llm: None,
        };

        let spawned = match (child.spec.factory)(ctx).await {
            Ok(s) => s,
            Err(_) => {
                // Factory failed — leave child dead
                return Ok(());
            }
        };

        // Hot-swap sender into stable mailbox
        child.mailbox.swap(spawned.sender);

        // Spawn new future in JoinSet
        let abort = self.join_set.spawn(spawned.future);
        let new_task_id = abort.id();
        self.task_map.insert(new_task_id, name.clone());

        // Update child state
        child.generation = new_gen;
        child.progress = Some(spawned.progress);
        child.child_cancel = Some(child_cancel);
        child.alive = true;

        let _ = self.event_tx.send(SupervisorEvent::ChildRestarted {
            name,
            old_gen,
            new_gen,
            delay,
        });

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
        // Implemented in Task 11
    }
}
