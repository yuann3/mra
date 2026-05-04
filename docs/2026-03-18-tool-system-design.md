# Tool System Design

## Summary

Add an extensible tool system to mra so agents can invoke tools (shell commands, file reads, etc.) during LLM-driven workflows. The system uses a unified `Tool` trait with untyped JSON boundaries (`serde_json::Value` in/out), a `ToolRegistry` for name-based lookup, and integration with the existing `AgentCtx` and LLM layer. Native Rust tools ship first; the trait is designed so WASM and MCP backends plug in later without changing agent code.

## Core Types

### `ToolSpec`

What the LLM sees. Sent as part of the LLM request so the model knows which tools are available and how to call them.

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolSpec {
    pub name: String,
    pub description: String,
    pub parameters: Value, // JSON Schema object
}
```

### `ToolOutput`

What comes back from a tool invocation. Agents feed this back to the LLM as a `Role::Tool` message.

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolOutput {
    pub content: String,
    pub is_error: bool,
}
```

### `Tool` trait

The unified interface. Every tool backend (native, WASM, MCP) implements this.

```rust
pub trait Tool: Send + Sync + 'static {
    fn spec(&self) -> &ToolSpec;
    fn invoke(&self, args: Value)
        -> Pin<Box<dyn Future<Output = Result<ToolOutput, ToolError>> + Send + '_>>;
}
```

Uses `Pin<Box<dyn Future>>` for dyn-safety, matching the existing `LlmProvider` pattern. No `async_trait` dependency.

### `ToolRegistry`

Holds `Arc<dyn Tool>` keyed by name. Built once, then cloned per-agent (cheap — just clones the `HashMap` of `Arc`s). Stored as an owned field in `AgentCtx`.

```rust
#[derive(Clone)]
pub struct ToolRegistry {
    tools: HashMap<String, Arc<dyn Tool>>,
}

impl ToolRegistry {
    pub fn new() -> Self;
    pub fn register(&mut self, tool: Arc<dyn Tool>);
    pub fn specs(&self) -> Vec<&ToolSpec>;
    pub fn get(&self, name: &str) -> Option<&Arc<dyn Tool>>;
    pub async fn invoke(&self, name: &str, args: Value) -> Result<ToolOutput, ToolError>;
}
```

`invoke` looks up the tool by name and calls it. Returns `ToolError::NotFound` if the name doesn't match.

Implements `Clone` so the supervisor can clone the registry into each child on spawn/restart.

## AgentCtx Integration

`AgentCtx` gains a `tools` field and a `call_tool` convenience method:

```rust
pub struct AgentCtx {
    // ... existing fields ...
    pub tools: ToolRegistry,
}

impl AgentCtx {
    pub async fn call_tool(&self, name: &str, args: Value) -> Result<ToolOutput, ToolError> {
        self.report_progress(); // reset hang detection timer
        self.tools.invoke(name, args).await
    }
}
```

`call_tool` mirrors `chat()` — wraps the raw call with progress reporting so tool invocations don't trigger hang detection.

Note: `report_progress()` is called once before the tool runs. For long-running tools (e.g., a 30s shell command), the agent's hang timeout should be configured to exceed the tool's timeout. This matches the existing `chat()` pattern.

## LLM Layer Changes

### LlmRequest

Add an optional `tools` field:

```rust
pub struct LlmRequest {
    pub model: Option<String>,
    pub messages: Vec<ChatMessage>,
    pub temperature: Option<f32>,
    pub max_tokens: Option<u32>,
    pub tools: Option<Vec<ToolSpec>>,  // NEW
}
```

### LlmResponse

Add a `tool_calls` field:

```rust
pub struct LlmResponse {
    pub content: String,
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    pub tool_calls: Vec<ToolCall>,  // NEW — empty if no tool use
}
```

### New types

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCall {
    pub id: String,       // provider-assigned call ID
    pub name: String,     // which tool to invoke
    pub arguments: Value, // parsed JSON arguments
}
```

### Role and ChatMessage

```rust
pub enum Role {
    System,
    User,
    Assistant,
    Tool,  // NEW
}

pub struct ChatMessage {
    pub role: Role,
    pub content: String,
    pub tool_calls: Vec<ToolCall>,     // NEW — populated on assistant messages with tool use
    pub tool_call_id: Option<String>,  // NEW — set for Role::Tool messages
}
```

The `tool_calls` field on assistant messages is required by the OpenAI API: when feeding tool results back to the LLM, the preceding assistant message must include the `tool_calls` that triggered them. Without this, the API rejects the conversation as malformed.

`ChatMessage` construction helpers (e.g., `ToolOutput::to_chat_message(call_id)`) are deferred — agents build these manually for now.

### OpenRouter client changes

The request serializer adds the `tools` array in OpenAI function-calling format:

```json
{
  "tools": [{
    "type": "function",
    "function": {
      "name": "shell",
      "description": "Run a shell command",
      "parameters": { ... }
    }
  }]
}
```

The response parser extracts `tool_calls` from the assistant message:

```json
{
  "choices": [{
    "message": {
      "tool_calls": [{
        "id": "call_abc123",
        "type": "function",
        "function": { "name": "shell", "arguments": "{\"command\": \"ls\"}" }
      }]
    }
  }]
}
```

### OpenRouter client changes (additional)

The `ApiMessage` response struct's `content` field must become `Option<String>` (or use `#[serde(default)]`). When the LLM responds with tool calls, `content` is often `null` in the API response. The current `content: String` would fail deserialization.

## Error Changes

Add one variant to the existing `ToolError` enum in `src/error.rs`:

```rust
pub enum ToolError {
    WasmTrap(String),        // existing
    FuelExhausted,           // existing
    ExecutionFailed(String), // existing
    NotFound(String),        // existing
    InvalidArgs(String),     // NEW
}
```

Classification match arm:

```rust
Self::InvalidArgs(_) => ErrorClass::Permanent,
```

Native tool implementations should wrap I/O errors in `ExecutionFailed` (classified as `Transient`) rather than introducing new variants.

## Native Tools

### `ShellTool`

Runs a shell command and returns stdout/stderr.

```rust
#[derive(Deserialize, JsonSchema)]
struct ShellArgs {
    /// The shell command to execute
    command: String,
}
```

- Uses `tokio::process::Command` with `/bin/sh -c`
- Returns stdout as `content`, sets `is_error: true` if exit code != 0 (with stderr as content)
- Timeout: configurable, default 30s
- No sandboxing in v1 (native trust boundary)

### `ReadFileTool`

Reads a file and returns its contents.

```rust
#[derive(Deserialize, JsonSchema)]
struct ReadFileArgs {
    /// The file path to read
    path: String,
}
```

- Uses `tokio::fs::read_to_string`
- Returns file contents as `content`
- Sets `is_error: true` with error message if file doesn't exist or can't be read

Both tools use `schemars::JsonSchema` on their args struct to auto-generate the JSON Schema at construction time, then `serde_json::from_value` inside `invoke()` for validation.

## Spawn Path Changes

`AgentHandle::spawn` and `AgentHandle::spawn_child` gain a `tools: ToolRegistry` parameter, threaded through to `AgentCtx`.

`ChildContext` (in `src/supervisor/child.rs`) gains a new field:

```rust
pub struct ChildContext {
    pub id: AgentId,
    pub generation: u64,
    pub cancel: CancellationToken,
    pub peers: HashMap<String, AgentHandle>,
    pub llm: Option<Arc<dyn LlmProvider>>,
    pub budget: Option<Arc<BudgetTracker>>,
    pub tools: ToolRegistry,  // NEW
}
```

The supervisor clones the `ToolRegistry` (cheap — just `Arc` clones) into `ChildContext` on each spawn/restart, so restarted agents retain their tool set.

## File Layout

| File | Change |
|------|--------|
| `src/tool/mod.rs` | New — `Tool`, `ToolSpec`, `ToolOutput`, `ToolRegistry` |
| `src/tool/shell.rs` | New — `ShellTool` |
| `src/tool/read_file.rs` | New — `ReadFileTool` |
| `src/agent/ctx.rs` | Add `tools: ToolRegistry`, `call_tool()` |
| `src/agent/runner.rs` | Thread `ToolRegistry` through spawn functions |
| `src/llm/mod.rs` | Add `tools`, `tool_calls`, `ToolCall`, `Role::Tool`, `tool_call_id` |
| `src/llm/openrouter.rs` | Serialize tools in request, parse tool_calls in response |
| `src/error.rs` | Add `ToolError::InvalidArgs` |
| `src/lib.rs` | Add `pub mod tool;` |
| `Cargo.toml` | Add `schemars = "1"` |

## What This Does NOT Include

- WASM sandboxed tools (future: `WasmTool` implements `Tool` trait)
- MCP remote tools (future: `McpTool` implements `Tool` trait)
- Tool-use budget tracking (tools don't consume LLM tokens directly)
- Agentic tool-use loop (agent calls LLM, LLM returns tool_calls, agent invokes tools, feeds results back — this is agent behavior logic, not framework)
- `#[tool]` derive macro for reducing boilerplate
- `inventory`-based auto-registration
