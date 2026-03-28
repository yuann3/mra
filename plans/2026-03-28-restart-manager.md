# RestartManager Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Extract restart decision logic from `SupervisorRunner` into a dedicated `RestartManager` struct with non-blocking backoff.

**Architecture:** Create `RestartManager` that owns per-child `RestartTracker` + global `IntensityTracker`, exposes a single `decide()` method returning `RestartDecision` enum. Supervisor schedules delays via `pending_restarts` queue polled in `select!` loop instead of blocking `sleep()`.

**Tech Stack:** Rust, Tokio, tokio::time::Instant

---

## File Structure

| File | Responsibility |
|------|----------------|
| `src/supervisor/restart_manager.rs` (CREATE) | `RestartManager` struct, `RestartDecision` enum, all restart decision logic |
| `src/supervisor/tracker.rs` (MODIFY) | Make `RestartTracker` and `IntensityTracker` `pub(crate)` (already are), no other changes |
| `src/supervisor/runner.rs` (MODIFY) | Add `RestartManager` field, `pending_restarts` queue, new `select!` branch, delegate to `decide()` |
| `src/supervisor/mod.rs` (MODIFY) | Add `mod restart_manager;` |
| `tests/restart_manager.rs` (CREATE) | Unit tests for `RestartManager::decide()` covering all decision paths |

---

### Task 1: Create RestartDecision enum and RestartManager struct

**Files:**
- Create: `src/supervisor/restart_manager.rs`

- [ ] **Step 1: Create the restart_manager module with RestartDecision enum**

```rust
use std::collections::HashMap;
use std::time::Duration;

use tokio::time::Instant;

use super::config::{ChildRestart, Strategy, SupervisorConfig};
use super::tracker::{IntensityTracker, RestartTracker};
use super::ChildExit;
use crate::config::RestartPolicy;

/// Decision returned by RestartManager — tells supervisor what to do next.
#[derive(Debug, Clone)]
pub enum RestartDecision {
    /// Restart this child after the specified delay.
    RestartAfter { delay: Duration },
    /// Restart all children in order (OneForAll cascade).
    RestartAll,
    /// Don't restart — policy says no (Temporary, or Transient+Normal exit).
    NoRestart,
    /// Don't restart — child exceeded its per-child restart limit.
    ChildLimitExceeded { restarts: u64 },
    /// Don't restart — supervisor-wide intensity limit exceeded (fatal).
    IntensityExceeded { total_restarts: u64 },
}

struct ChildRestartState {
    policy: ChildRestart,
    restart_policy: RestartPolicy,
    tracker: RestartTracker,
}

/// Coordinates restart decisions, backoff calculation, and limit enforcement.
///
/// This struct owns all restart-related state and provides a single entry point
/// (`decide`) for the supervisor to determine what to do when a child exits.
/// Decisions are synchronous — the supervisor is responsible for scheduling
/// any backoff delays.
pub(crate) struct RestartManager {
    strategy: Strategy,
    children: HashMap<String, ChildRestartState>,
    intensity: IntensityTracker,
}
```

- [ ] **Step 2: Add RestartManager constructor**

Add to `src/supervisor/restart_manager.rs`:

```rust
impl RestartManager {
    /// Creates a new RestartManager with supervisor-wide config.
    pub(crate) fn new(config: &SupervisorConfig) -> Self {
        Self {
            strategy: config.strategy,
            children: HashMap::new(),
            intensity: IntensityTracker::new(config.intensity.clone()),
        }
    }
}
```

- [ ] **Step 3: Verify it compiles**

Run: `cargo check -p mra 2>&1 | head -20`
Expected: No errors related to restart_manager (module not wired yet, that's ok)

- [ ] **Step 4: Commit**

```bash
git add src/supervisor/restart_manager.rs
git commit -m "feat(supervisor): add RestartDecision enum and RestartManager struct"
```

---

### Task 2: Add register/unregister methods

**Files:**
- Modify: `src/supervisor/restart_manager.rs`

- [ ] **Step 1: Add register method**

Add to `impl RestartManager`:

```rust
    /// Registers a child for restart tracking. Call once on initial start.
    pub(crate) fn register(&mut self, name: &str, restart: ChildRestart, restart_policy: &RestartPolicy) {
        self.children.insert(
            name.to_owned(),
            ChildRestartState {
                policy: restart,
                restart_policy: restart_policy.clone(),
                tracker: RestartTracker::new(restart_policy),
            },
        );
    }

    /// Removes a child from tracking (on explicit stop or permanent removal).
    pub(crate) fn unregister(&mut self, name: &str) {
        self.children.remove(name);
    }
```

- [ ] **Step 2: Verify it compiles**

Run: `cargo check -p mra 2>&1 | head -20`
Expected: No errors

- [ ] **Step 3: Commit**

```bash
git add src/supervisor/restart_manager.rs
git commit -m "feat(supervisor): add RestartManager register/unregister methods"
```

---

### Task 3: Implement the decide() method

**Files:**
- Modify: `src/supervisor/restart_manager.rs`

- [ ] **Step 1: Add the decide method**

Add to `impl RestartManager`:

```rust
    /// The main entry point: "This child exited — what should I do?"
    ///
    /// Synchronously decides whether to restart, checking:
    /// - Restart policy (Permanent/Transient/Temporary)
    /// - Per-child restart limits
    /// - Supervisor-wide intensity limits
    /// - OneForOne vs OneForAll strategy
    ///
    /// Returns a decision with computed backoff delay (if applicable).
    /// **Does NOT sleep** — supervisor must schedule the delay.
    pub(crate) fn decide(
        &mut self,
        name: &str,
        exit: &ChildExit,
        hung: bool,
        now: Instant,
    ) -> RestartDecision {
        let Some(child) = self.children.get_mut(name) else {
            return RestartDecision::NoRestart;
        };

        // Hung children are treated as failures regardless of exit type
        let is_failure = hung || exit.is_failure();

        // 1. Evaluate restart policy
        if !child.policy.should_restart(is_failure) {
            return RestartDecision::NoRestart;
        }

        // 2. Record restart timestamp in per-child tracker
        child.tracker.record(now);

        // 3. Check per-child limit
        if child.tracker.exceeded() {
            return RestartDecision::ChildLimitExceeded {
                restarts: child.tracker.total_restarts,
            };
        }

        // 4. Record in global intensity tracker
        self.intensity.record(now);

        // 5. Check supervisor-wide intensity
        if self.intensity.exceeded() {
            return RestartDecision::IntensityExceeded {
                total_restarts: self.intensity.total_restarts,
            };
        }

        // 6. Apply strategy
        match self.strategy {
            Strategy::OneForOne => {
                let delay = child.tracker.backoff_delay();
                RestartDecision::RestartAfter { delay }
            }
            Strategy::OneForAll => RestartDecision::RestartAll,
        }
    }
```

- [ ] **Step 2: Verify it compiles**

Run: `cargo check -p mra 2>&1 | head -20`
Expected: No errors

- [ ] **Step 3: Commit**

```bash
git add src/supervisor/restart_manager.rs
git commit -m "feat(supervisor): implement RestartManager::decide() method"
```

---

### Task 4: Add helper methods for OneForAll

**Files:**
- Modify: `src/supervisor/restart_manager.rs`

- [ ] **Step 1: Add record_all and backoff_delay methods**

Add to `impl RestartManager`:

```rust
    /// For OneForAll: records restart for all children except Temporary.
    /// Call AFTER canceling all children, BEFORE respawning.
    pub(crate) fn record_all(&mut self, now: Instant) {
        for child in self.children.values_mut() {
            if !matches!(child.policy, ChildRestart::Temporary) {
                child.tracker.record(now);
            }
        }
    }

    /// Returns current backoff delay for a child (useful for diagnostics).
    pub(crate) fn backoff_delay(&self, name: &str) -> Duration {
        self.children
            .get(name)
            .map(|c| c.tracker.backoff_delay())
            .unwrap_or_default()
    }

    /// Returns the total restarts for a child (for event emission).
    pub(crate) fn child_total_restarts(&self, name: &str) -> u64 {
        self.children
            .get(name)
            .map(|c| c.tracker.total_restarts)
            .unwrap_or(0)
    }
```

- [ ] **Step 2: Verify it compiles**

Run: `cargo check -p mra 2>&1 | head -20`
Expected: No errors

- [ ] **Step 3: Commit**

```bash
git add src/supervisor/restart_manager.rs
git commit -m "feat(supervisor): add RestartManager helper methods for OneForAll"
```

---

### Task 5: Wire RestartManager into supervisor module

**Files:**
- Modify: `src/supervisor/mod.rs`

- [ ] **Step 1: Add the module declaration**

Add after `pub(crate) mod tracker;`:

```rust
pub(crate) mod restart_manager;
```

- [ ] **Step 2: Verify it compiles**

Run: `cargo check -p mra 2>&1 | head -20`
Expected: No errors

- [ ] **Step 3: Commit**

```bash
git add src/supervisor/mod.rs
git commit -m "feat(supervisor): wire restart_manager module"
```

---

### Task 6: Add PendingRestart struct and queue to SupervisorRunner

**Files:**
- Modify: `src/supervisor/runner.rs`

- [ ] **Step 1: Add import and PendingRestart struct**

Add to imports at top of file:

```rust
use super::restart_manager::{RestartDecision, RestartManager};
```

Add after `ChildState` struct:

```rust
#[derive(Debug)]
struct PendingRestart {
    name: String,
    when: tokio::time::Instant,
    old_gen: u64,
}
```

- [ ] **Step 2: Add fields to SupervisorRunner**

Add to `SupervisorRunner` struct (after `intensity` field):

```rust
    restart_mgr: RestartManager,
    pending_restarts: Vec<PendingRestart>,
```

- [ ] **Step 3: Update SupervisorRunner::new()**

In `SupervisorRunner::new()`, replace:

```rust
        let intensity = IntensityTracker::new(config.intensity.clone());
```

with:

```rust
        let restart_mgr = RestartManager::new(&config);
```

And update the `Self { ... }` to replace `intensity` with:

```rust
            restart_mgr,
            pending_restarts: Vec::new(),
```

- [ ] **Step 4: Remove the standalone intensity field usage**

Remove the `intensity` field from `SupervisorRunner` struct definition (it's now inside `RestartManager`).

- [ ] **Step 5: Verify it compiles (will have errors, that's expected)**

Run: `cargo check -p mra 2>&1 | head -40`
Expected: Errors about `self.intensity` usage — we'll fix those in next tasks

- [ ] **Step 6: Commit work in progress**

```bash
git add src/supervisor/runner.rs
git commit -m "wip(supervisor): add RestartManager and pending_restarts to runner"
```

---

### Task 7: Register children with RestartManager

**Files:**
- Modify: `src/supervisor/runner.rs`

- [ ] **Step 1: Register child in do_start_child**

In `do_start_child()`, after creating `ChildState` and before `self.children.insert(...)`, add:

```rust
        // Register with restart manager
        self.restart_mgr.register(&name, spec.restart, &spec.config.restart_policy);
```

- [ ] **Step 2: Remove tracker from ChildState**

In `ChildState` struct, remove the `tracker: RestartTracker` field entirely. It's now managed by `RestartManager`.

Also remove this line from `do_start_child()`:

```rust
        let tracker = RestartTracker::new(&spec.config.restart_policy);
```

And remove `tracker,` from the `ChildState { ... }` initialization.

- [ ] **Step 3: Verify it compiles (still has errors)**

Run: `cargo check -p mra 2>&1 | head -40`
Expected: Errors about `child.tracker` — we'll fix in next task

- [ ] **Step 4: Commit**

```bash
git add src/supervisor/runner.rs
git commit -m "feat(supervisor): register children with RestartManager"
```

---

### Task 8: Refactor handle_child_exit to use RestartManager::decide

**Files:**
- Modify: `src/supervisor/runner.rs`

- [ ] **Step 1: Replace OneForOne restart logic with decide()**

Replace the entire `handle_child_exit` method with:

```rust
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
```

- [ ] **Step 2: Verify it compiles (restart_all still has issues)**

Run: `cargo check -p mra 2>&1 | head -40`
Expected: Errors in `restart_all` — we'll fix next

- [ ] **Step 3: Commit**

```bash
git add src/supervisor/runner.rs
git commit -m "refactor(supervisor): use RestartManager::decide in handle_child_exit"
```

---

### Task 9: Refactor restart_all to use RestartManager

**Files:**
- Modify: `src/supervisor/runner.rs`

- [ ] **Step 1: Update restart_all method**

Replace the entire `restart_all` method with:

```rust
    async fn restart_all(&mut self, trigger_name: &str) -> Result<(), SupervisorError> {
        // 1. Cancel all alive children (except the one that already exited)
        for (name, child) in &self.children {
            if name != trigger_name {
                if let Some(ref cancel) = child.child_cancel {
                    cancel.cancel();
                }
            }
        }

        // 2. Wait for all to exit
        while self.join_set.join_next().await.is_some() {}
        self.task_map.clear();

        // 3. Record restart in RestartManager for all non-Temporary children
        let now = tokio::time::Instant::now();
        self.restart_mgr.record_all(now);

        // 4. Check global intensity (we need to check via a test decide call or track separately)
        // For now, we'll respawn and let individual restarts catch intensity issues

        // 5. Respawn all non-Temporary children in insertion order
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

            self.emit(SupervisorEvent::ChildRestarted {
                name: child_name.clone(),
                old_gen,
                new_gen,
                delay: Duration::from_millis(0),
            });
        }

        Ok(())
    }
```

- [ ] **Step 2: Remove unused import**

Remove `IntensityTracker` from imports if no longer used directly:

```rust
use super::tracker::RestartTracker;  // Remove if not used
```

Actually, check if `RestartTracker` is still imported — it shouldn't be needed in runner.rs anymore.

- [ ] **Step 3: Verify it compiles**

Run: `cargo check -p mra 2>&1 | head -20`
Expected: Might still have errors about pending_restarts handling

- [ ] **Step 4: Commit**

```bash
git add src/supervisor/runner.rs
git commit -m "refactor(supervisor): use RestartManager in restart_all"
```

---

### Task 10: Add pending_restarts polling to supervisor select loop

**Files:**
- Modify: `src/supervisor/runner.rs`

- [ ] **Step 1: Add helper method to get next restart time**

Add method to `impl SupervisorRunner`:

```rust
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
```

- [ ] **Step 2: Add do_restart_child method**

Add method to `impl SupervisorRunner`:

```rust
    async fn do_restart_child(&mut self, name: &str, old_gen: u64) -> Result<(), SupervisorError> {
        // Check child still exists and is dead with matching generation
        let Some(child) = self.children.get(name) else {
            return Ok(());
        };
        if child.alive || child.generation != old_gen {
            return Ok(()); // Already restarted or generation mismatch
        }

        // Build peers map from alive siblings
        let peers: HashMap<String, AgentHandle> = self
            .children
            .iter()
            .filter(|(n, c)| c.alive && *n != name)
            .map(|(n, c)| {
                (
                    n.clone(),
                    AgentHandle::new(c.id, c.mailbox.clone(), c.logical_cancel.clone()),
                )
            })
            .collect();

        let child = self.children.get_mut(name).unwrap();
        let new_gen = old_gen + 1;
        let child_cancel = child.logical_cancel.child_token();

        let ctx = ChildContext {
            id: child.id,
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
                // Factory failed — leave child dead
                return Ok(());
            }
        };

        // Hot-swap sender into stable mailbox
        child.mailbox.swap(spawned.sender);

        // Spawn new future in JoinSet
        let abort = self.join_set.spawn(spawned.future);
        let new_task_id = abort.id();
        self.task_map.insert(new_task_id, name.to_string());

        // Update child state
        child.generation = new_gen;
        child.progress = Some(spawned.progress);
        child.child_cancel = Some(child_cancel);
        child.alive = true;

        let delay = self.restart_mgr.backoff_delay(name);
        self.emit(SupervisorEvent::ChildRestarted {
            name: name.to_string(),
            old_gen,
            new_gen,
            delay,
        });

        Ok(())
    }
```

- [ ] **Step 3: Update run() select loop**

Update the `run()` method's select loop to add pending restart handling:

```rust
    pub(crate) async fn run(mut self) -> Result<(), SupervisorError> {
        let mut hang_tick = tokio::time::interval(self.config.hang_check_interval);
        hang_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

        self.emit(SupervisorEvent::SupervisorStarted);

        loop {
            // Calculate sleep duration for pending restarts
            let restart_sleep = async {
                if let Some(when) = self.next_pending_restart() {
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
```

- [ ] **Step 4: Verify it compiles**

Run: `cargo check -p mra 2>&1 | head -20`
Expected: No errors

- [ ] **Step 5: Commit**

```bash
git add src/supervisor/runner.rs
git commit -m "feat(supervisor): add non-blocking pending restart polling"
```

---

### Task 11: Clean up unused imports and code

**Files:**
- Modify: `src/supervisor/runner.rs`

- [ ] **Step 1: Remove unused tracker imports**

Remove from imports:

```rust
use super::tracker::{IntensityTracker, RestartTracker};
```

Keep only what's needed.

- [ ] **Step 2: Run cargo check and fix any remaining issues**

Run: `cargo check -p mra 2>&1`
Expected: No errors

- [ ] **Step 3: Run cargo clippy**

Run: `cargo clippy -p mra 2>&1 | head -40`
Fix any warnings.

- [ ] **Step 4: Commit**

```bash
git add src/supervisor/runner.rs
git commit -m "chore(supervisor): clean up unused imports after RestartManager refactor"
```

---

### Task 12: Write unit tests for RestartManager

**Files:**
- Create: `tests/restart_manager.rs`

- [ ] **Step 1: Create test file with basic tests**

```rust
use std::time::Duration;

use mra::config::RestartPolicy;
use mra::supervisor::{ChildExit, ChildRestart, RestartIntensity, Strategy, SupervisorConfig};

// Note: RestartManager is pub(crate), so we test via integration tests
// that exercise the supervisor behavior. These tests verify the decision logic
// indirectly through supervisor behavior.

// For unit tests, we'd need to either:
// 1. Make RestartManager pub (not recommended)
// 2. Add a test module inside the crate
// 3. Test via supervisor integration tests

// Let's add internal unit tests in the restart_manager module instead.
```

Actually, since `RestartManager` is `pub(crate)`, we should add tests inside the module.

- [ ] **Step 2: Add tests inside restart_manager.rs**

Add at the bottom of `src/supervisor/restart_manager.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::supervisor::config::RestartIntensity;

    fn test_config(strategy: Strategy) -> SupervisorConfig {
        SupervisorConfig {
            strategy,
            intensity: RestartIntensity {
                max_restarts: 3,
                window: Duration::from_secs(60),
            },
            hang_check_interval: Duration::from_secs(1),
            event_capacity: 64,
        }
    }

    fn test_restart_policy() -> RestartPolicy {
        RestartPolicy {
            max_restarts: 2,
            window: Duration::from_secs(60),
            backoff_base: Duration::from_millis(100),
            backoff_max: Duration::from_secs(1),
        }
    }

    #[test]
    fn decide_no_restart_for_temporary() {
        let config = test_config(Strategy::OneForOne);
        let mut mgr = RestartManager::new(&config);
        mgr.register("temp", ChildRestart::Temporary, &test_restart_policy());

        let decision = mgr.decide("temp", &ChildExit::Failed("err".into()), false, Instant::now());
        assert!(matches!(decision, RestartDecision::NoRestart));
    }

    #[test]
    fn decide_no_restart_for_transient_normal_exit() {
        let config = test_config(Strategy::OneForOne);
        let mut mgr = RestartManager::new(&config);
        mgr.register("trans", ChildRestart::Transient, &test_restart_policy());

        let decision = mgr.decide("trans", &ChildExit::Normal, false, Instant::now());
        assert!(matches!(decision, RestartDecision::NoRestart));
    }

    #[test]
    fn decide_restart_for_transient_failure() {
        let config = test_config(Strategy::OneForOne);
        let mut mgr = RestartManager::new(&config);
        mgr.register("trans", ChildRestart::Transient, &test_restart_policy());

        let decision = mgr.decide("trans", &ChildExit::Failed("err".into()), false, Instant::now());
        assert!(matches!(decision, RestartDecision::RestartAfter { .. }));
    }

    #[test]
    fn decide_restart_for_permanent_normal_exit() {
        let config = test_config(Strategy::OneForOne);
        let mut mgr = RestartManager::new(&config);
        mgr.register("perm", ChildRestart::Permanent, &test_restart_policy());

        let decision = mgr.decide("perm", &ChildExit::Normal, false, Instant::now());
        assert!(matches!(decision, RestartDecision::RestartAfter { .. }));
    }

    #[test]
    fn decide_hung_child_treated_as_failure() {
        let config = test_config(Strategy::OneForOne);
        let mut mgr = RestartManager::new(&config);
        mgr.register("trans", ChildRestart::Transient, &test_restart_policy());

        // Shutdown exit is normally not a failure, but hung=true overrides
        let decision = mgr.decide("trans", &ChildExit::Shutdown, true, Instant::now());
        assert!(matches!(decision, RestartDecision::RestartAfter { .. }));
    }

    #[test]
    fn decide_child_limit_exceeded() {
        let config = test_config(Strategy::OneForOne);
        let mut mgr = RestartManager::new(&config);
        let policy = RestartPolicy {
            max_restarts: 1, // Exceed after 2 restarts
            window: Duration::from_secs(60),
            backoff_base: Duration::from_millis(10),
            backoff_max: Duration::from_millis(100),
        };
        mgr.register("child", ChildRestart::Permanent, &policy);

        let now = Instant::now();
        // First restart
        let d1 = mgr.decide("child", &ChildExit::Failed("".into()), false, now);
        assert!(matches!(d1, RestartDecision::RestartAfter { .. }));

        // Second restart - should exceed (max_restarts=1 means >1 triggers exceeded)
        let d2 = mgr.decide("child", &ChildExit::Failed("".into()), false, now + Duration::from_millis(1));
        assert!(matches!(d2, RestartDecision::ChildLimitExceeded { .. }));
    }

    #[test]
    fn decide_intensity_exceeded() {
        let config = SupervisorConfig {
            strategy: Strategy::OneForOne,
            intensity: RestartIntensity {
                max_restarts: 1, // Exceed after 2 restarts
                window: Duration::from_secs(60),
            },
            hang_check_interval: Duration::from_secs(1),
            event_capacity: 64,
        };
        let mut mgr = RestartManager::new(&config);
        let policy = RestartPolicy {
            max_restarts: 10, // High limit so per-child doesn't trigger
            window: Duration::from_secs(60),
            backoff_base: Duration::from_millis(10),
            backoff_max: Duration::from_millis(100),
        };
        mgr.register("a", ChildRestart::Permanent, &policy);
        mgr.register("b", ChildRestart::Permanent, &policy);

        let now = Instant::now();
        // First restart (a)
        let d1 = mgr.decide("a", &ChildExit::Failed("".into()), false, now);
        assert!(matches!(d1, RestartDecision::RestartAfter { .. }));

        // Second restart (b) - should exceed intensity
        let d2 = mgr.decide("b", &ChildExit::Failed("".into()), false, now + Duration::from_millis(1));
        assert!(matches!(d2, RestartDecision::IntensityExceeded { .. }));
    }

    #[test]
    fn decide_one_for_all_returns_restart_all() {
        let config = test_config(Strategy::OneForAll);
        let mut mgr = RestartManager::new(&config);
        mgr.register("child", ChildRestart::Permanent, &test_restart_policy());

        let decision = mgr.decide("child", &ChildExit::Failed("".into()), false, Instant::now());
        assert!(matches!(decision, RestartDecision::RestartAll));
    }

    #[test]
    fn decide_unknown_child_returns_no_restart() {
        let config = test_config(Strategy::OneForOne);
        let mgr = RestartManager::new(&config);

        let decision = mgr.decide("unknown", &ChildExit::Failed("".into()), false, Instant::now());
        assert!(matches!(decision, RestartDecision::NoRestart));
    }

    #[test]
    fn backoff_delay_increases_exponentially() {
        let config = test_config(Strategy::OneForOne);
        let mut mgr = RestartManager::new(&config);
        let policy = RestartPolicy {
            max_restarts: 10,
            window: Duration::from_secs(60),
            backoff_base: Duration::from_millis(100),
            backoff_max: Duration::from_secs(10),
        };
        mgr.register("child", ChildRestart::Permanent, &policy);

        let now = Instant::now();

        // First restart: 100ms * 2^0 = 100ms
        if let RestartDecision::RestartAfter { delay } = mgr.decide("child", &ChildExit::Failed("".into()), false, now) {
            assert_eq!(delay, Duration::from_millis(100));
        } else {
            panic!("expected RestartAfter");
        }

        // Second restart: 100ms * 2^1 = 200ms
        if let RestartDecision::RestartAfter { delay } = mgr.decide("child", &ChildExit::Failed("".into()), false, now + Duration::from_millis(1)) {
            assert_eq!(delay, Duration::from_millis(200));
        } else {
            panic!("expected RestartAfter");
        }

        // Third restart: 100ms * 2^2 = 400ms
        if let RestartDecision::RestartAfter { delay } = mgr.decide("child", &ChildExit::Failed("".into()), false, now + Duration::from_millis(2)) {
            assert_eq!(delay, Duration::from_millis(400));
        } else {
            panic!("expected RestartAfter");
        }
    }
}
```

- [ ] **Step 3: Run tests**

Run: `cargo test -p mra restart_manager 2>&1`
Expected: All tests pass

- [ ] **Step 4: Commit**

```bash
git add src/supervisor/restart_manager.rs
git commit -m "test(supervisor): add unit tests for RestartManager"
```

---

### Task 13: Run full test suite and fix any regressions

**Files:**
- Various

- [ ] **Step 1: Run all tests**

Run: `cargo test 2>&1`
Expected: All tests pass

- [ ] **Step 2: Fix any failures**

If tests fail, analyze and fix. Common issues:
- Import errors
- Field access changes in ChildState
- Event emission timing

- [ ] **Step 3: Run clippy**

Run: `cargo clippy --all-targets 2>&1 | head -40`
Fix any warnings.

- [ ] **Step 4: Commit fixes if any**

```bash
git add -A
git commit -m "fix(supervisor): address test regressions after RestartManager refactor"
```

---

### Task 14: Final cleanup and documentation

**Files:**
- Modify: `src/supervisor/restart_manager.rs`

- [ ] **Step 1: Add module-level documentation**

Add at the top of `src/supervisor/restart_manager.rs`:

```rust
//! Restart decision manager for the supervisor.
//!
//! This module consolidates all restart-related logic into a single struct:
//! - Per-child restart tracking (backoff, limits)
//! - Global restart intensity tracking
//! - Policy evaluation (Permanent/Transient/Temporary)
//! - Strategy dispatch (OneForOne/OneForAll)
//!
//! The `RestartManager::decide()` method is the single entry point for
//! restart decisions. It returns a `RestartDecision` enum that tells
//! the supervisor what to do — without blocking on backoff delays.
```

- [ ] **Step 2: Verify documentation builds**

Run: `cargo doc -p mra --no-deps 2>&1 | head -20`
Expected: No warnings

- [ ] **Step 3: Final commit**

```bash
git add -A
git commit -m "docs(supervisor): add RestartManager module documentation"
```

---

## Summary

After completing all tasks:

1. **~150 lines removed from runner.rs** — inline restart logic replaced with `decide()` call
2. **New restart_manager.rs (~200 lines)** — consolidated restart logic with clean interface
3. **Non-blocking backoff** — supervisor loop stays responsive during restart delays
4. **11 new unit tests** — covering all `RestartDecision` variants
5. **No breaking changes** — external API unchanged
