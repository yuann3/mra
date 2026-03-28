# PRD: Agent Spawn Builder

## Problem

`AgentHandle::spawn` and `AgentHandle::spawn_child` take 8 positional parameters,
5 of which are usually defaulted to empty/None values. Every `ChildFactory` closure
requires ~20 lines of boilerplate including a `Pin<Box<dyn Future<...>>>` type cast
and manual `ChildContext` field forwarding. This ceremony is repeated identically
across all tests and user code.

Specific pain points:

1. **Positional args are error-prone** — `budget` and `llm` are both
   `Option<Arc<...>>` and easy to swap silently.
2. **Factory boilerplate** — every supervised agent needs a 20-line closure that
   destructures `ChildContext`, forwards 6 fields into `spawn_child`, and casts
   to `Pin<Box<dyn Future<...>>>`.
3. **Name duplication** — `ChildSpec.name` and `AgentConfig.name` must match but
   nothing enforces this. Budget registration uses `spec.name`, budget charging
   uses `ctx.name` (from config). Drift causes silent bugs.
4. **Extension fragility** — adding a 9th parameter breaks every call site and
   every factory closure.

## Proposed Solution

### Part A: `AgentSpawn<B>` builder

A typed builder for standalone agent spawns. Only `name` + `behavior` are required;
everything else defaults to empty/None.

```rust
#[must_use]
pub struct AgentSpawn<B> { /* 8 fields with defaults */ }

impl<B: AgentBehavior> AgentSpawn<B> {
    // Convenience: auto-creates AgentConfig from name
    pub fn new(name: impl Into<String>, behavior: B) -> Self;

    // Explicit: caller provides pre-built config
    pub fn from_config(config: AgentConfig, behavior: B) -> Self;

    // Internal: absorbs all ChildContext fields for use in from_behavior
    pub(crate) fn with_child_ctx(self, ctx: ChildContext) -> Self;

    // Optional setters
    pub fn id(self, id: AgentId) -> Self;
    pub fn peers(self, peers: HashMap<String, AgentHandle>) -> Self;
    pub fn llm(self, llm: Arc<dyn LlmProvider>) -> Self;
    pub fn cancel(self, cancel: CancellationToken) -> Self;
    pub fn budget(self, budget: Arc<BudgetTracker>) -> Self;
    pub fn tools(self, tools: ToolRegistry) -> Self;

    // Terminal methods
    pub fn spawn(self) -> SpawnedAgent;
    pub fn spawn_child(self) -> SpawnedChild;
}
```

### Part B: `ChildSpec::from_behavior`

Eliminates factory boilerplate for the 90% case where the factory just returns
a fresh behavior value.

```rust
impl ChildSpec {
    pub fn from_behavior<B, F>(config: AgentConfig, make_behavior: F) -> Self
    where
        B: AgentBehavior,
        F: Fn(&ChildContext) -> B + Send + Sync + 'static;
}
```

- Derives `ChildSpec.name` from `config.name` — single source of truth.
- Closure receives `&ChildContext` so behaviors can inspect `generation`, `peers`, etc.
- Hides the `Pin<Box<dyn Future<...>>>` annotation entirely.
- `ChildSpec::new` remains as the escape hatch for async/fallible factories.

## Non-Goals

- Not changing `SpawnedAgent` or `SpawnedChild` return types.
- Not changing `AgentBehavior` trait.
- Not changing supervisor internals (runner.rs restart logic).
- Not removing `ChildSpec::new` — it remains for advanced use cases.

## Success Criteria

1. All 32 existing tests pass after migration.
2. No test requires `HashMap::new()`, `None`, or `ToolRegistry::new()` as positional args.
3. No test contains the `Pin<Box<dyn Future<...>>>` type cast (except escape-hatch tests).
4. `ChildSpec.name` is always derived from `config.name` when using `from_behavior`.
5. Adding a hypothetical 9th parameter to the builder does not break any call site.

---

# Implementation Plan

## Phase 1: Add `AgentSpawn<B>` builder (additive, no breakage)

Create `src/agent/spawn.rs` with:

- [ ] `AgentSpawn<B>` struct with all 8 fields
- [ ] `new(name, behavior)` constructor with defaults
- [ ] `from_config(config, behavior)` constructor
- [ ] `with_child_ctx(self, ctx)` method (pub(crate))
- [ ] All optional setters: `.id()`, `.peers()`, `.llm()`, `.cancel()`, `.budget()`, `.tools()`
- [ ] Terminal methods: `.spawn()` → `SpawnedAgent`, `.spawn_child()` → `SpawnedChild`
- [ ] `#[must_use]` on the struct
- [ ] Re-export from `agent/mod.rs`

**Verify**: `cargo test` — all existing tests still pass, new code compiles.

## Phase 2: Add `ChildSpec::from_behavior` (additive, no breakage)

In `src/supervisor/child.rs`:

- [ ] Add `from_behavior<B, F>(config, make_behavior)` method
- [ ] Internally uses `AgentSpawn::from_config(config, make_behavior(&ctx)).with_child_ctx(ctx).spawn_child()`
- [ ] Derives `name` from `config.name`
- [ ] Inherits default `restart`, `shutdown_policy`, `hang_timeout`, `token_budget`
- [ ] Builder methods (`.with_restart()`, etc.) chain after `from_behavior`

**Verify**: `cargo test` — all existing tests still pass.

## Phase 3: Migrate tests to new API

- [ ] `tests/agent.rs` — replace all `AgentHandle::spawn(...)` with `AgentSpawn::new(...).spawn()`
- [ ] `tests/supervisor.rs` — replace `echo_spec()` and inline factories with `ChildSpec::from_behavior`
- [ ] `tests/runtime.rs` — replace `echo_spec()` with `ChildSpec::from_behavior`
- [ ] `tests/budget_integration.rs` — replace `test_spec()` with `ChildSpec::from_behavior`
- [ ] `tests/tool.rs` — replace `spawn_tool_agent()` with `AgentSpawn::new(...)`
- [ ] Keep raw `ChildSpec::new` only for generation-dependent tests (OneForAll, hang detection)

**Verify**: `cargo test` — all 32 tests pass. No test contains `HashMap::new()` or
`Pin<Box<dyn Future<...>>>` cast (except intentional escape-hatch tests).

## Phase 4: Deprecate old API

- [ ] Add `#[deprecated]` to `AgentHandle::spawn` and `AgentHandle::spawn_child`
- [ ] Update `src/supervisor/runner.rs` internal calls to use `AgentSpawn` (if applicable)
- [ ] Update doc comments and examples in `agent/mod.rs`

**Verify**: `cargo test` — no warnings except the intentional deprecation notices
on escape-hatch call sites (suppressed with `#[allow(deprecated)]`).
