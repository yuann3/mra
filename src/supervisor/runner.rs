//! The supervisor's main `select!` loop.
//!
//! Coordinates child exits, restart scheduling, hang detection, and
//! command processing. Internal only — users interact through
//! [`SupervisorHandle`](super::SupervisorHandle).

use std::collections::HashMap;
use std::time::Duration;

use tokio::sync::{broadcast, mpsc};
use tokio_util::sync::CancellationToken;

use crate::agent::AgentHandle;
use crate::error::SupervisorError;

use super::ChildExit;
use super::child::ChildSpec;
use super::config::{ChildRestart, SupervisorConfig};
use super::event::SupervisorEvent;
use super::handle::SupervisorCommand;
use super::lifecycle::{ChildExitInfo, ChildLifecycle, LifecycleConfig};
use super::restart_manager::{RestartDecision, RestartManager};

#[derive(Debug)]
struct PendingRestart {
    name: String,
    when: tokio::time::Instant,
    old_gen: u64,
}

pub(crate) struct SupervisorRunner {
    config: SupervisorConfig,
    lifecycle: ChildLifecycle,
    specs: HashMap<String, ChildSpec>,
    child_order: Vec<String>,
    command_rx: mpsc::Receiver<SupervisorCommand>,
    event_tx: broadcast::Sender<SupervisorEvent>,
    cancel: CancellationToken,
    restart_mgr: RestartManager,
    pending_restarts: Vec<PendingRestart>,
}

impl SupervisorRunner {
    pub(crate) fn new(
        config: SupervisorConfig,
        command_rx: mpsc::Receiver<SupervisorCommand>,
        event_tx: broadcast::Sender<SupervisorEvent>,
    ) -> Self {
        let restart_mgr = RestartManager::new(&config);
        let lifecycle = ChildLifecycle::new(LifecycleConfig {
            budget: config.budget().cloned(),
            llm: config.llm().cloned(),
            tools: config.tools().clone(),
        });
        Self {
            config,
            lifecycle,
            specs: HashMap::new(),
            child_order: Vec::new(),
            command_rx,
            event_tx,
            cancel: CancellationToken::new(),
            restart_mgr,
            pending_restarts: Vec::new(),
        }
    }

    pub(crate) async fn run(mut self) -> Result<(), SupervisorError> {
        let mut hang_tick = tokio::time::interval(self.config.hang_check_interval);
        hang_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

        self.emit(SupervisorEvent::SupervisorStarted);

        loop {
            // Calculate sleep duration for pending restarts
            let next_restart = self.next_pending_restart();
            let restart_sleep = async {
                if let Some(when) = next_restart {
                    tokio::time::sleep_until(when).await;
                } else {
                    // Sleep forever (will be cancelled by other branches)
                    std::future::pending::<()>().await;
                }
            };

            tokio::select! {
                _ = self.cancel.cancelled() => {
                    self.drain_all().await;
                    break Ok(());
                }

                Some(result) = self.lifecycle.join_set_mut().join_next_with_id() => {
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
                    if self.check_global_budget() {
                        self.drain_all().await;
                        break Ok(());
                    }
                }

                _ = restart_sleep, if !self.pending_restarts.is_empty() => {
                    let now = tokio::time::Instant::now();
                    while let Some(restart) = self.pop_ready_restart(now) {
                        if !self.cancel.is_cancelled() {
                            self.do_restart_child(&restart.name, restart.old_gen).await?;
                        }
                    }
                }
            }
        }
    }

    fn emit(&self, event: SupervisorEvent) {
        if self.event_tx.send(event).is_err() {
            tracing::debug!("supervisor event dropped: no active subscribers");
        }
    }

    fn lifecycle_budget(&self) -> Option<&std::sync::Arc<crate::budget::BudgetTracker>> {
        self.lifecycle.budget()
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
                let handle = self.lifecycle.get_handle(&name);
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

        // Build peers map from existing alive children
        let peers = self.lifecycle.peers_excluding(&name);

        // Register with restart manager
        self.restart_mgr
            .register(&name, spec.restart, &spec.config.restart_policy);

        // Store spec for restarts
        self.specs.insert(name.clone(), spec.clone());

        // Start the child via lifecycle
        let handle = self.lifecycle.start(spec, &peers).await?;

        self.child_order.push(name.clone());

        self.emit(SupervisorEvent::ChildStarted {
            name,
            generation: 0,
        });

        Ok(handle)
    }

    async fn do_stop_child(&mut self, name: &str) -> Result<(), SupervisorError> {
        if self.lifecycle.get(name).is_none() {
            return Err(SupervisorError::ChildNotFound(name.into()));
        }
        self.lifecycle.cancel_child(name);
        Ok(())
    }

    async fn handle_child_exit(
        &mut self,
        result: Result<(tokio::task::Id, ChildExit), tokio::task::JoinError>,
    ) -> Result<(), SupervisorError> {
        let Some(exit_info) = self.lifecycle.process_exit(result) else {
            return Ok(());
        };

        let ChildExitInfo {
            name,
            generation,
            exit,
            hung,
        } = exit_info;

        self.emit(SupervisorEvent::ChildExited {
            name: name.clone(),
            generation,
            exit: exit.clone(),
        });

        // Emit per-agent BudgetExceeded only when the agent has a per-agent limit.
        // If the exit was caused by the global budget, the __global__ event from
        // check_global_budget() covers it; ChildExited still fires unconditionally.
        if matches!(exit, ChildExit::BudgetExceeded)
            && let Some(usage) = self
                .lifecycle_budget()
                .and_then(|b| b.agent_usage(&name))
            && let Some(limit) = usage.limit
        {
            self.emit(SupervisorEvent::BudgetExceeded {
                name: name.clone(),
                used: usage.used,
                limit,
            });
        }

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
                    old_gen: generation,
                });
                Ok(())
            }

            RestartDecision::RestartAll => self.restart_all(&name).await,

            RestartDecision::ChildLimitExceeded { restarts } => {
                self.emit(SupervisorEvent::ChildRestartLimitExceeded { name, restarts });
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
        self.lifecycle.cancel_all_except(trigger_name);

        // 2. Wait for all to exit
        self.lifecycle.drain().await;

        // 3. Record restart in RestartManager for all non-Temporary children
        let now = tokio::time::Instant::now();
        if !self.restart_mgr.record_all(now) {
            // Intensity exceeded — emit event, drain, and fail
            let total_restarts = self.restart_mgr.intensity_total_restarts();
            self.emit(SupervisorEvent::RestartIntensityExceeded { total_restarts });
            self.drain_all().await;
            return Err(SupervisorError::RestartIntensityExceeded { total_restarts });
        }

        // 4. Respawn all non-Temporary children in insertion order
        let order = self.child_order.clone();
        for child_name in &order {
            // Skip Temporary children
            let Some(spec) = self.specs.get(child_name) else {
                continue;
            };
            if matches!(spec.restart, ChildRestart::Temporary) {
                continue;
            }

            if self.cancel.is_cancelled() {
                return Ok(());
            }

            let old_gen = self
                .lifecycle
                .get(child_name)
                .map(|c| c.generation)
                .unwrap_or(0);

            // Build peers from already-respawned siblings
            let peers = self.lifecycle.peers_excluding(child_name);

            // Restart via lifecycle
            let new_gen = match self.lifecycle.restart(child_name, spec, &peers).await {
                Ok(generation) => generation,
                Err(e) => {
                    self.emit(SupervisorEvent::ChildSpawnFailed {
                        name: child_name.clone(),
                        error: e.to_string(),
                    });
                    continue;
                }
            };

            self.emit(SupervisorEvent::ChildRestarted {
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
        self.lifecycle.cancel_all();
        self.lifecycle.drain().await;
    }

    fn next_pending_restart(&self) -> Option<tokio::time::Instant> {
        self.pending_restarts.iter().map(|r| r.when).min()
    }

    fn pop_ready_restart(&mut self, now: tokio::time::Instant) -> Option<PendingRestart> {
        if let Some(idx) = self.pending_restarts.iter().position(|r| r.when <= now) {
            Some(self.pending_restarts.swap_remove(idx))
        } else {
            None
        }
    }

    async fn do_restart_child(&mut self, name: &str, old_gen: u64) -> Result<(), SupervisorError> {
        // Check child still exists and is dead with matching generation
        let Some(child) = self.lifecycle.get(name) else {
            return Ok(());
        };
        if child.alive || child.generation != old_gen {
            return Ok(()); // Already restarted or generation mismatch
        }

        let Some(spec) = self.specs.get(name) else {
            return Ok(());
        };

        // Build peers map from alive siblings
        let peers = self.lifecycle.peers_excluding(name);

        // Restart via lifecycle
        let new_gen = match self.lifecycle.restart(name, spec, &peers).await {
            Ok(generation) => generation,
            Err(e) => {
                self.emit(SupervisorEvent::ChildSpawnFailed {
                    name: name.to_string(),
                    error: e.to_string(),
                });
                return Ok(());
            }
        };

        let delay = self.restart_mgr.backoff_delay(name);
        self.emit(SupervisorEvent::ChildRestarted {
            name: name.to_string(),
            old_gen,
            new_gen,
            delay,
        });

        Ok(())
    }

    /// Returns `true` if the global budget has been exceeded, emitting events.
    fn check_global_budget(&self) -> bool {
        let Some(budget) = self.lifecycle_budget() else {
            return false;
        };
        if !budget.is_global_exceeded() {
            return false;
        }
        let usage = budget.run_usage();
        self.emit(SupervisorEvent::BudgetExceeded {
            name: "__global__".to_string(),
            used: usage.used,
            limit: usage.limit.unwrap_or(0),
        });
        true
    }

    async fn check_hangs(&mut self) {
        // Collect hung children first (can't borrow mutably while iterating)
        let hangs: Vec<_> = self.lifecycle.check_hangs(&self.specs).collect();

        for hung in hangs {
            self.emit(SupervisorEvent::HangDetected {
                name: hung.name.clone(),
                generation: hung.generation,
                elapsed: hung.elapsed,
            });

            // Cancel the hung child (marks it as hung for restart decision)
            self.lifecycle.cancel_child(&hung.name);
        }
    }
}
