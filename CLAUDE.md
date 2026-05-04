# MRA — Multi-agent Runtime Architecture

Tokio-native Rust framework for building resilient, multi-agent LLM applications. Inspired by Erlang/OTP supervision and Flue's harness-first philosophy.

## Build & Test

```bash
cargo build
cargo test
cargo run --bin demo "your topic"          # research pipeline demo (needs MRA_LLM__API_KEY)
cargo run --bin demo_supervisor            # supervision + budget demo (mock LLM, no key needed)
cargo run --bin demo_tools                 # tool invocation demo
cargo build --features wasm               # enable WASM-sandboxed tools
```

Set `MRA_LLM__API_KEY=<openrouter-key>` or add to `mra.toml` (see `mra.example.toml`).

## Architecture

```
Runtime (entry point)
  └── Supervisor (Erlang-style supervision tree)
        └── AgentRunner × N (owns behavior, holds AgentCtx)
              ├── AgentCtx  (tools, peers, LLM, budget, session history)
              ├── ToolRegistry
              └── BudgetTracker
```

**Key design invariant:** No shared mutable state. Agents communicate through `AgentHandle` (cloneable, `Send + Sync`) over bounded `mpsc` channels with `oneshot` reply.

## Module Map

| Module | Purpose |
|--------|---------|
| `agent/` | Actor model: `AgentBehavior`, `AgentHandle`, `AgentCtx`, `AgentSpawn` |
| `supervisor/` | Supervision tree, restart policies, hang detection, event streaming |
| `runtime.rs` | `SwarmRuntime` — top-level entry point (being replaced by `Runtime` builder) |
| `llm/` | `LlmProvider` trait, `OpenRouterClient` |
| `tool/` | `Tool` trait, `ToolRegistry`, built-in tools (`ShellTool`, `ReadFileTool`, `EditFileTool`) |
| `budget.rs` | `BudgetTracker` — atomic per-agent and global token limits |
| `config.rs` | `AgentConfig`, `SupervisorConfig` via Figment (toml + env) |
| `error.rs` | `ErrorClass` enum for retry/restart decisions |
| `sandbox/` | `Sandbox` trait, `Workspace` (TempDir + symlink mounts), `VirtualSandbox` — replaces `wasm/` |

## Implementing an Agent

```rust
use mra::agent::{AgentBehavior, AgentCtx, AgentReply, AgentError};
use mra::llm::LlmRequest;

pub struct MyAgent;

impl AgentBehavior for MyAgent {
    async fn handle(&mut self, ctx: &mut AgentCtx, input: Task) -> Result<AgentReply, AgentError> {
        let response = ctx.chat(&LlmRequest::builder()
            .user(input.content)
            .build()).await?;
        Ok(AgentReply::text(response.content))
    }
}
```

## Key Conventions

- **Error classification:** every error has `fn classification() -> ErrorClass`. The supervisor uses this to decide whether to retry, restart, or give up.
- **Progress reporting:** long-running agents call `ctx.report_progress()` to reset the hang detector.
- **Peer discovery:** agents access siblings by name via `ctx.peers["other-agent"]`. The supervisor injects peers automatically.
- **Budget enforcement:** `ctx.chat()` pre-checks the budget before calling the LLM. Once exceeded, it stays exceeded for that run.
- **Tool output limits:** `ShellTool` caps at 32 KB, `ReadFileTool` at 64 KB. Configure via builders.

## In-Progress Design

MRA is being evolved into a proper agent framework. See the active design doc:

- [`docs/2026-05-04-mra-agent-framework-design.md`](docs/2026-05-04-mra-agent-framework-design.md) — **Triggers + Sessions + Model Selection** (approved, not yet implemented)

**Planned additions:**
- `Runtime` builder API (replaces `SwarmRuntime`)
- HTTP trigger via Axum (`features = ["http"]`) with SSE streaming (`Accept: text/event-stream`)
- CLI trigger (`cargo run --bin myapp <agent-name> "prompt"`)
- `SessionStore` trait with `MemorySessionStore` + `FileSessionStore`
- Per-agent model selection (`AgentEntry::new(...).model("anthropic/claude-sonnet-4-6")`)
- Conversation history in `AgentCtx` (auto-prepended to LLM calls)
- `Workspace` + `VirtualSandbox` — `TempDir`-backed isolated workspace with symlink mounts (replaces WASM)
- Roles — `.mra/roles/<name>.md` system prompt overlays injected per call

## Docs

| File | Description |
|------|-------------|
| `docs/2026-05-04-mra-agent-framework-design.md` | Framework evolution design (triggers, sessions, model selection) |
| `docs/2026-03-18-tool-system-design.md` | Tool system design |
| `docs/plans/` | Historical implementation plans |
