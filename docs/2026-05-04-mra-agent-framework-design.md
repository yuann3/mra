# MRA Agent Framework Design

**Date:** 2026-05-04  
**Status:** Approved  
**Branch:** demos/update-and-supervisor  

---

## Problem Statement

MRA is currently a library — users must write a full `main()`, wire up supervisors manually, and there is no concept of deploying an agent as a persistent service. Inspired by Flue, this design adds triggers (HTTP + CLI) and session persistence (conversation history) to make MRA a proper agent framework.

**Core gap being filled:** Agents cannot currently be invoked via HTTP or CLI without bespoke plumbing, and there is no mechanism for conversation continuity across invocations.

**Out of scope (backlog):** Skills (Markdown-based reusable prompts), cron triggers, distributed supervision.

---

## Architecture Overview

Three new layers added on top of the existing supervisor/agent core:

```
┌─────────────────────────────────────────┐
│              Runtime                     │  ← replaces SwarmRuntime
│  ┌─────────────┐  ┌──────────────────┐  │
│  │ HTTP Trigger │  │   CLI Trigger    │  │
│  │  (Axum)     │  │  (argv parsing)  │  │
│  └──────┬──────┘  └────────┬─────────┘  │
│         └────────┬─────────┘            │
│          ┌───────▼────────┐             │
│          │  SessionStore  │             │
│          │  (load/save)   │             │
│          └───────┬────────┘             │
│          ┌───────▼────────┐             │
│          │   Supervisor   │             │  ← unchanged
│          └───────┬────────┘             │
│          ┌───────▼────────┐             │
│          │   AgentCtx     │             │  ← gains history + store ref
│          │ + Vec<Message> │             │
│          └────────────────┘             │
└─────────────────────────────────────────┘
```

**Key invariants:**
- `Runtime` is the single public entry point for all new users
- Supervisor, agent core, tools, and budget tracking are unchanged
- HTTP is behind `features = ["http"]` — zero cost if unused
- Existing `AgentBehavior` implementations require no changes to gain session support

---

## Component 1: `SessionStore` Trait

**Location:** `src/session/mod.rs`

```rust
#[async_trait]
pub trait SessionStore: Send + Sync + 'static {
    async fn load(&self, session_id: &str) -> Result<Vec<Message>, SessionError>;
    async fn save(&self, session_id: &str, history: &[Message]) -> Result<(), SessionError>;
    async fn delete(&self, session_id: &str) -> Result<(), SessionError>;
}
```

**Built-in implementations:**

| Type | Backing | Default for |
|------|---------|-------------|
| `MemorySessionStore` | `Arc<Mutex<HashMap<String, Vec<Message>>>>` | CLI mode, tests |
| `FileSessionStore` | `{dir}/{session_id}.json` | HTTP server mode |

Users can implement `SessionStore` themselves for Redis, Postgres, etc.

**`Message` type** (new, shared, provider-agnostic):

```rust
pub struct Message {
    pub role: Role,      // User | Assistant | System
    pub content: String,
}
```

Maps 1:1 to the `messages` array already sent by `OpenRouterClient`.

**Store selection:** If the user does not call `.session_store()` on the builder, `Runtime` selects automatically:
- CLI mode → `MemorySessionStore`
- HTTP mode → `FileSessionStore::new(".mra/sessions")`

---

## Component 2: `AgentCtx` Changes

**Location:** `src/agent/ctx.rs`

Three new fields injected by `Runtime` before each dispatch. The injection mechanism: `Runtime` loads history from `SessionStore`, then passes `session_id` and `history` as fields on `Task`. `AgentRunner` reads them from `Task` when constructing `AgentCtx`. This keeps `AgentRunner` decoupled from the store — it only sees the already-loaded data.

```rust
pub struct AgentCtx {
    // existing fields unchanged
    pub id: AgentId,
    pub name: String,
    pub peers: HashMap<String, AgentHandle>,
    pub tools: ToolRegistry,

    // new fields
    pub(crate) history: Vec<Message>,
    pub(crate) session_id: Option<String>,
    pub(crate) session_store: Arc<dyn SessionStore>,
}
```

**`ctx.chat()` updated behavior:**

1. Build full message list: `history + req.messages`
2. Call LLM with full context
3. Append both turns (user + assistant) to `self.history`
4. Flush updated history to `session_store` (only if `session_id` is `Some`)
5. Return `LlmResponse` unchanged

`ctx.chat()` signature is unchanged from the caller's perspective. `&mut self` was already required.

**`AgentBehavior` trait:** No changes required. Existing behaviors gain multi-turn support automatically.

---

## Component 3: `Runtime` (replaces `SwarmRuntime`)

**Location:** `src/runtime/mod.rs`

```rust
pub struct Runtime {
    supervisor: Supervisor,
    session_store: Arc<dyn SessionStore>,
    #[cfg(feature = "http")]
    http_port: u16,
}

impl Runtime {
    pub fn builder() -> RuntimeBuilder { ... }

    pub async fn run(self) -> Result<(), RuntimeError> {
        let args: Vec<String> = std::env::args().collect();
        match args.get(1).map(String::as_str) {
            Some("serve") => self.run_http().await,
            Some(name)    => {
                let prompt = args.get(2).map(String::as_str).unwrap_or("");
                self.run_cli(name, prompt).await
            }
            None => Err(RuntimeError::Usage("usage: <binary> serve | <binary> <agent-name> <prompt>".into())),
        }
    }
}
```

**Builder API:**

```rust
Runtime::builder()
    .agent(AgentEntry::new("researcher", ResearcherBehavior::new())
        .model("anthropic/claude-opus-4-6"))    // per-agent override
    .agent(AgentEntry::new("writer", WriterBehavior::new()))  // uses global default
    .model("anthropic/claude-sonnet-4-6")        // global default model
    .llm(OpenRouterClient::builder().api_key("...").build())
    .session_store(FileSessionStore::new(".mra/sessions"))  // optional
    .budget(BudgetTracker::builder().global_tokens(1_000_000).build())  // optional
    .port(3000)  // optional, HTTP only
    .build().await?
    .run().await
```

**`run()` dispatch logic:**

| `argv[1]` | Mode | Session store default |
|-----------|------|-----------------------|
| `"serve"` | HTTP server on default port (3000) | `FileSessionStore` |
| `"serve --port N"` | HTTP server on port N | `FileSessionStore` |
| `"<agent-name>" <prompt>` | CLI one-shot | `MemorySessionStore` |
| absent or unrecognized | Print usage error, exit 1 | — |

**`SwarmRuntime` is removed.** It provided a thin wrapper over `Supervisor` with no additional value. All functionality migrates into `Runtime`.

---

## Component 4: HTTP Trigger (`features = ["http"]`)

**Location:** `src/runtime/http.rs`

**Routes:**

```
POST   /agents/:name                → new session (Runtime generates UUID v4)
POST   /agents/:name/:session_id    → continue existing session
GET    /agents/:name/:session_id    → fetch session history
DELETE /agents/:name/:session_id    → delete session
```

**Request body:**
```json
{ "prompt": "summarize the state of Rust async in 2025" }
```

**Response:**
```json
{
  "session_id": "abc-123",
  "response": "Rust async has matured significantly...",
  "usage": { "prompt_tokens": 142, "completion_tokens": 89 }
}
```

**Error responses:**

| Code | Condition |
|------|-----------|
| `404` | Unknown agent name |
| `400` | Missing or malformed prompt |
| `500` | Agent error (message included) |

**Axum state:** `Arc<RuntimeState>` shared across handlers, containing agent handle map and session store reference.

---

## Component 5: CLI Trigger

**Location:** `src/runtime/cli.rs`

Invocation: `cargo run --bin myagent researcher "summarize X"`

- `argv[1]` = agent name
- `argv[2]` = prompt
- Runtime creates a `MemorySessionStore`, loads empty history, dispatches `Task`, prints result to stdout, exits

No session continuity across CLI invocations by design (one-shot). Users who need CLI sessions can pass `--session <id>` — this is a backlog item.

---

## Component 6: Model Selection

**Location:** `src/runtime/mod.rs`, `src/agent/ctx.rs`

Each agent can specify its own model ID. A global default on `RuntimeBuilder` is used for any agent that doesn't override it.

**Builder API:**

```rust
Runtime::builder()
    .agent(AgentEntry::new("researcher", ResearcherBehavior::new())
        .model("anthropic/claude-opus-4-6"))      // per-agent override
    .agent(AgentEntry::new("writer", WriterBehavior::new()))  // uses global default
    .model("anthropic/claude-sonnet-4-6")          // global default
    .llm(OpenRouterClient::builder().api_key("...").build())
    .build().await?
    .run().await
```

**`AgentEntry`** — new thin wrapper replacing the raw `(name, behavior)` tuple in the builder:

```rust
pub struct AgentEntry {
    pub name: String,
    pub behavior: Box<dyn AgentBehavior>,
    pub model: Option<String>,  // None → use RuntimeBuilder global default
}

impl AgentEntry {
    pub fn new(name: &str, behavior: impl AgentBehavior) -> Self { ... }
    pub fn model(mut self, model_id: &str) -> Self { ... }
}
```

**How it flows into `AgentCtx`:**
- `Runtime` resolves each agent's effective model at build time: `agent.model.unwrap_or(global_default)`
- The resolved model string is stored in `AgentCtx` as `pub(crate) model: String`
- `ctx.chat()` sets `req.model = self.model.clone()` before calling the LLM provider

**Model ID format:** OpenRouter-style `provider/model-name` (e.g., `anthropic/claude-sonnet-4-6`, `openai/gpt-4o`). MRA does not validate the string — OpenRouter returns a clear error for unknown models.

**`LlmRequest` change:** The existing `model` field on `LlmRequest` (if present) is overwritten by `AgentCtx`'s resolved model. If `LlmRequest` doesn't yet have a `model` field, one is added.

---

## Component 7: Workspace + VirtualSandbox (replaces WASM)

**Location:** `src/sandbox/mod.rs`

The WASM sandbox (`features = ["wasm"]`, Wasmtime) is **removed**. Replaced by a `Sandbox` trait backed by a `TempDir`-based `Workspace`.

### `Sandbox` trait

```rust
pub trait Sandbox: Send + 'static {
    async fn exec(&mut self, cmd: &str, opts: ExecOptions) -> Result<ExecResult>;
    async fn read_file(&self, path: &str) -> Result<String>;
    async fn write_file(&self, path: &str, content: &str) -> Result<()>;
    fn root(&self) -> &Path;
}

pub struct ExecOptions {
    pub env: HashMap<String, String>,
    pub stdin: Option<String>,
}

pub struct ExecResult {
    pub stdout: String,
    pub stderr: String,
    pub exit_code: i32,
}
```

### `Workspace`

```rust
pub struct Workspace {
    dir: TempDir,           // auto-deleted on drop
    mounts: Vec<Mount>,
}

struct Mount {
    mount_point: PathBuf,   // path inside workspace
    real_path: PathBuf,     // real host path (symlinked in)
}

impl Workspace {
    pub fn new() -> Result<Self>
    pub fn mount(&mut self, at: &str, real_path: PathBuf) -> Result<()>  // creates symlink
    pub fn path(&self) -> &Path
}
```

**Mount mechanics:** `workspace.mount("/workspace", "/home/user/project")` creates a symlink at `{tempdir}/workspace → /home/user/project`. The agent sees it as a real directory; writes go directly to the host path.

**Flue mapping:**
- `new Workspace()` ≡ `new InMemoryFs()` — ephemeral scratch, nothing persists past drop
- `.mount("/workspace", real_path)` ≡ `new ReadWriteFs({ root })` — live host access
- Multiple mounts ≡ `new MountableFs(...)` — composed view

### `VirtualSandbox`

```rust
pub struct VirtualSandbox {
    workspace: Workspace,
}

impl VirtualSandbox {
    pub fn new() -> Result<Self>                              // empty scratch
    pub fn with_mount(at: &str, path: PathBuf) -> Result<Self>
}
```

Shell commands run via `tokio::process::Command` with `current_dir = workspace.root()`. `ReadFileTool` and `EditFileTool` resolve paths relative to the workspace root and reject `..` traversal.

### Integration with `AgentCtx`

`AgentCtx` gains a `sandbox: Box<dyn Sandbox>` field. Built-in tools (`ShellTool`, `ReadFileTool`, `EditFileTool`) use `ctx.sandbox` instead of accessing the host directly. `Runtime` creates a fresh `VirtualSandbox` per session and passes it through `Task` → `AgentRunner` → `AgentCtx`.

**`AgentEntry` gains optional sandbox factory:**
```rust
AgentEntry::new("coder", CoderBehavior::new())
    .sandbox(|| VirtualSandbox::with_mount("/workspace", project_dir))
```

---

## Component 8: SSE Streaming

**Location:** `src/runtime/http.rs`

The same Axum routes handle both buffered and streaming responses. The client's `Accept` header determines the format — no API surface change.

**Routing logic:**
```
Accept: application/json      → buffered JSON (existing behavior)
Accept: text/event-stream     → SSE token stream
```

**SSE event format:**
```
data: {"type":"token","content":"Hello"}
data: {"type":"token","content":" world"}
data: {"type":"done","session_id":"abc-123","usage":{"prompt_tokens":42,"completion_tokens":17}}
```

**Implementation:** `LlmProvider` gains an optional `chat_stream()` method returning `impl Stream<Item = Result<String>>`. `OpenRouterClient` implements it via chunked HTTP. If the provider doesn't implement streaming, the response is buffered and sent as a single `token` event followed by `done`.

**`LlmProvider` addition:**
```rust
pub trait LlmProvider: Send + Sync + 'static {
    async fn chat(&self, req: &LlmRequest) -> Result<LlmResponse, LlmError>;
    // Optional — default impl buffers chat() and yields one chunk
    async fn chat_stream(&self, req: &LlmRequest)
        -> Result<impl Stream<Item = Result<String>> + Send, LlmError> { ... }
}
```

---

## Component 9: Roles

**Location:** `src/runtime/roles.rs`

Roles are named system prompt overlays loaded from `.mra/roles/<name>.md` at `Runtime` startup. They are injected as a system message for the duration of one `ctx.chat()` call — not stored in session history.

**File convention:** `.mra/roles/data-analyst.md` defines the `"data-analyst"` role. If `.mra/roles/` doesn't exist, roles are unavailable (no error).

**Precedence:** call-level role > session-level role > no role.

**HTTP payload addition:**
```json
{ "prompt": "analyze this CSV", "role": "data-analyst" }
```

**`AgentCtx` addition:**
```rust
ctx.with_role("data-analyst")  // returns a scoped ctx for one chat() call
```

Internally, `with_role()` prepends the role's Markdown content as a `System` message before the history and user message, then removes it after the call returns.

**`RuntimeBuilder` loads roles at build time:**
```rust
Runtime::builder()
    .roles_dir(".mra/roles")   // optional, defaults to ".mra/roles"
    ...
```

---

## Feature Flags

| Flag | Adds | Default |
|------|------|---------|
| `http` | Axum dependency, HTTP trigger + SSE | off |

**Removed:** `wasm` feature flag and Wasmtime/wasmtime-wasi dependencies. The `vfs` crate (pure Rust, no deps) replaces them for in-process file operations.

---

## Migration from `SwarmRuntime`

`SwarmRuntime` is removed. Users currently using it directly (only the demo binaries) update their `main.rs` to use `Runtime::builder()`. The supervisor API below `Runtime` is unchanged.

Existing `AgentBehavior` implementations: **no changes required**.

---

## What Is Not Changing

- `Supervisor`, `SupervisorConfig`, `ChildRestart`, `SupervisorEvent` — unchanged
- `AgentBehavior` trait — unchanged
- `BudgetTracker` — unchanged
- `LlmProvider`, `OpenRouterClient` — unchanged (streaming method is additive)
- `AgentHandle`, `AgentSpawn` — unchanged

**Changing:**
- `Tool`, `ToolRegistry`, built-in tools — tools now use `ctx.sandbox` instead of host directly
- `wasm` feature removed, `http` feature added

---

## Backlog (not in this design)

- Skills: `.mra/skills/*.md` loaded at startup, `ctx.skill("name")` returns contents
- Cron trigger
- CLI session continuity (`--session <id>` flag)
- `GET /agents` to list registered agents
