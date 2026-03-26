# Plan: Tool Demo — Single-Agent Coding Assistant

> Source PRD: https://github.com/yuann3/mra/issues/5

## Architectural decisions

Durable decisions that apply across all phases:

- **Base branch**: Build on top of `feat/tool-system` (all tool infrastructure already exists there)
- **Tool trait pattern**: Every tool implements `Tool` with `spec() -> &ToolSpec` and `invoke(Value) -> Pin<Box<dyn Future<Output = Result<ToolOutput, ToolError>> + Send + '_>>` — dyn-safe async
- **Tool arg structs**: `#[derive(Deserialize, JsonSchema)] #[serde(deny_unknown_fields)]` — reject unknown fields, derive `schemars::JsonSchema` for auto-generated parameter schema via `schema_for!()`, consistent with `ShellTool` and `ReadFileTool`
- **ToolLoopResult**: New struct defined in `src/agent/ctx.rs`, re-exported from `src/agent/mod.rs` — `{ response: LlmResponse, iterations: usize, total_prompt_tokens: u64, total_completion_tokens: u64 }`
- **Max iterations semantics**: `max_iterations` counts total LLM calls (not tool-execution rounds). If the LLM returns tool calls on the final iteration, the loop stops and returns that response without executing the tools — the caller gets the last response as-is.
- **Message history in tool loop**: `chat_with_tools` accumulates `Vec<ChatMessage>` across iterations: system -> user -> assistant(tool_calls) -> tool results -> assistant(tool_calls) -> tool results -> ... -> assistant(text)
- **Error propagation**: Two distinct error paths:
  - **Domain errors** (file not found, text not found, ambiguous match): returned as `Ok(ToolOutput { is_error: true, content: "..." })` — sent back to the LLM so it can recover. Matches the `ReadFileTool` pattern.
  - **Unexpected errors** (JSON deserialization failure): returned as `Err(ToolError::InvalidArgs(...))`. These are programming errors, not recoverable by the LLM.
  - **Budget exceeded**: `AgentError::BudgetExceeded` from `chat()` is fatal and breaks the tool loop immediately.
- **Mock LlmProvider**: Phase 2 requires a new mock `LlmProvider` implementation for testing. No mock exists in the codebase today — this is net-new test infrastructure. Existing tests in `tests/llm.rs` are structural/serde tests only.
- **EditFileTool semantics**: Search-and-replace only. Args: `{ "path", "old_text", "new_text" }`. Fails if old_text not found or ambiguous (multiple matches). Does not support full-file overwrite.

---

## Phase 1: EditFileTool

**User stories**: 2, 11

### What to build

A new `EditFileTool` that implements the `Tool` trait with search-and-replace semantics. The tool reads a file, verifies the `old_text` exists exactly once, replaces it with `new_text`, and writes the file back. Errors (file not found, text not found, ambiguous match) are returned as `ToolOutput { is_error: true }` with a descriptive message — not as Rust errors — so the LLM can recover gracefully.

Register the tool in the tool module's public exports alongside `ShellTool` and `ReadFileTool`.

Comprehensive tests covering: successful replacement, file not found, old_text not found, ambiguous match (old_text appears multiple times), edge cases (empty strings, unicode content). Tests use real temp files, not mocks.

### Acceptance criteria

- [ ] `EditFileTool` implements `Tool` trait with correct `ToolSpec` (name, description, JSON Schema parameters)
- [ ] Successful search-and-replace: reads file, replaces exactly one occurrence, writes back, returns success message
- [ ] File not found: returns `ToolOutput { is_error: true }` with descriptive message
- [ ] `old_text` not found in file: returns `ToolOutput { is_error: true }` with descriptive message
- [ ] Ambiguous match (old_text appears multiple times): returns `ToolOutput { is_error: true }` with descriptive message
- [ ] `deny_unknown_fields` on arg struct rejects extra JSON fields
- [ ] Re-exported from `src/tool/mod.rs` as `pub use edit_file::EditFileTool`
- [ ] All tests pass: `cargo test` with no failures
- [ ] `cargo clippy` clean, `cargo fmt` clean

---

## Phase 2: `chat_with_tools()` loop

**User stories**: 3, 5, 6, 9, 10

### What to build

A reusable `chat_with_tools()` method on `AgentCtx` that implements the multi-turn tool-use loop. Given an initial `LlmRequest` and a max iteration count, the method:

1. Calls `self.chat()` (which handles budget enforcement)
2. If the response contains `tool_calls`, executes each tool via `self.call_tool()`, appends tool result messages to the conversation history
3. Re-calls the LLM with the updated history
4. Repeats until the LLM returns a plain text response (no tool calls) or max iterations is reached
5. Sends heartbeats via `self.report_progress()` between iterations

Returns `ToolLoopResult` with the final response, iteration count, and aggregated token counts across all LLM calls.

Tests use a mock LLM that returns scripted sequences of tool calls and plain text responses, plus real tool instances where applicable.

### Acceptance criteria

- [ ] `ToolLoopResult` struct defined with `response`, `iterations`, `total_prompt_tokens`, `total_completion_tokens`
- [ ] Normal loop: mock LLM returns tool calls on iteration 1, plain text on iteration 2 — verify 2 iterations, correct token aggregation
- [ ] Immediate text: mock LLM returns plain text on first call — verify 1 iteration, no tools executed
- [ ] Max iterations: mock LLM returns tool calls every time — verify loop stops at `max_iterations`, returns last response
- [ ] Tool error handling: tool returns `ToolOutput { is_error: true }` — verify it's sent back to LLM as a tool message, loop continues
- [ ] Budget exceeded mid-loop: mock LLM + budget tracker — verify `AgentError::BudgetExceeded` propagates out
- [ ] Heartbeat: `report_progress()` called between iterations (verified via progress watch channel)
- [ ] Token aggregation is correct across multiple iterations (sum of all prompt + completion tokens)
- [ ] All tests pass: `cargo test` with no failures
- [ ] `cargo clippy` clean, `cargo fmt` clean

---

## Phase 3: `demo_tools.rs` binary

**User stories**: 1, 4, 7, 8, 12

### What to build

A new demo binary showcasing the tool system end-to-end. A single "coder" agent is spawned under the supervisor with `ShellTool`, `ReadFileTool`, and `EditFileTool` registered. The agent uses `ctx.chat_with_tools()` to run a multi-turn tool loop.

System prompt: "You are a code quality agent. Run cargo clippy, then fix any warnings you find."

Default user task (argv fallback): "Run cargo clippy on this project, read any files with warnings, and fix the issues"

The demo prints each tool call as it happens (tool name, arguments, truncated result) so the user can observe the agent working in real time. Uses the same supervisor/runtime/budget scaffolding as the existing pipeline demo: event subscription loop, token usage breakdown, graceful shutdown.

### Acceptance criteria

- [ ] New binary at `src/bin/demo_tools.rs`
- [ ] Single "coder" agent with `ShellTool`, `ReadFileTool`, `EditFileTool` registered
- [ ] Uses `ctx.chat_with_tools()` with max 10 iterations
- [ ] System prompt and default user task match the spec
- [ ] Accepts user prompt via `argv[1]` with sensible default
- [ ] Prints each tool call: tool name, arguments (truncated if long), result summary
- [ ] Prints iteration count as loop progresses
- [ ] Full supervisor event logging in background task (same pattern as `demo.rs`)
- [ ] Token usage breakdown at the end (global + per-agent)
- [ ] Graceful shutdown via `runtime.shutdown()`
- [ ] `[[bin]]` entry added to `Cargo.toml` for `demo_tools`
- [ ] `cargo build --bin demo_tools` compiles without warnings
- [ ] `cargo clippy` clean, `cargo fmt` clean
