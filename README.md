# mra

> **WIP.** Not stable release, API will change 

I wanted something lightweight that makes me happy.

MRA is a framework for building headless, programmable AI agents that run as Tokio actors with built-in supervision, sessions, sandboxing, and tool use. write them together, and let them talk to LLMs.

## What??

Each agent runs as its own async task with a bounded `mpsc` mailbox. Agents talk to each other through handles, call LLMs, and get restarted by a supervisor if they crash or stop responding. No shared mutable state.

What you get out of the box:
- `Runtime::builder()` -- one entry point, handles CLI and HTTP dispatch
- Session persistence -- conversation history saved and replayed automatically
- Per-agent model selection -- different agents can use different LLMs
- Sandbox workspaces -- agents run in isolated temp directories with mounted host paths
- Tools -- shell, file read, file edit, scoped to the sandbox
- Roles -- load system prompts from `.mra/roles/*.md` files
- SSE streaming -- `Accept: text/event-stream` on any agent endpoint
- Supervision -- restarts, hang detection, budget enforcement

## Quick look

A three-agent pipeline in ~40 lines of actual code:

```rust
use mra::agent::{AgentBehavior, AgentCtx, AgentReply, Task};
use mra::config::MraConfig;
use mra::error::AgentError;
use mra::llm::{LlmRequest, OpenRouterClient};
use mra::runtime::{AgentEntry, Runtime};

struct Researcher;
impl AgentBehavior for Researcher {
    async fn handle(&mut self, ctx: &mut AgentCtx, input: Task) -> Result<AgentReply, AgentError> {
        let resp = ctx.chat(&LlmRequest::builder()
            .system("You are a research assistant. Produce concise notes.")
            .user(&input.instruction)
            .build()).await?;
        let next = ctx.peers["writer"].execute(Task::new(&resp.content)).await?;
        Ok(AgentReply { task_id: input.id, output: next.output,
            self_tokens: resp.total_tokens(),
            total_tokens: resp.total_tokens() + next.total_tokens })
    }
}

// Writer and Editor follow the same pattern...

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let config = MraConfig::load()?;
    Runtime::builder()
        .agent(AgentEntry::new("editor", Editor).model("anthropic/claude-haiku-4-5"))
        .agent(AgentEntry::new("writer", Writer).model("anthropic/claude-sonnet-4"))
        .agent(AgentEntry::new("researcher", Researcher).model("anthropic/claude-haiku-4-5"))
        .llm(OpenRouterClient::builder()
            .api_key(&config.llm.api_key)
            .base_url(&config.llm.base_url)
            .build())
        .build().await?
        .run().await?;
    Ok(())
}
```

`Runtime::builder()` registers agents, wires the LLM, starts the supervisor. `run()` looks at argv -- if you pass an agent name and a prompt, it dispatches directly. If you pass `serve`, it starts an HTTP server.

```bash
cargo run --example pipeline researcher "the history of Rust"
```

Each agent picks its own model. The researcher and editor use Haiku (fast, cheap), the writer uses Sonnet (better at creative work). The supervisor injects peer handles automatically, so `ctx.peers["writer"]` just works.

## Install

Run the following Cargo command in your project directory:

```bash
cargo add mra
```

Or add the following line to your Cargo.toml:

```toml
mra = "0.1.0"
```

## Runtime and triggers

`Runtime::builder()` is the entry point.

```rust
Runtime::builder()
    .agent(AgentEntry::new("my-agent", MyBehavior).model("anthropic/claude-sonnet-4"))
    .llm(my_llm_provider)
    .tools(my_tool_registry)          // shared tools for all agents
    .session_store(my_store)          // optional, defaults to memory/file
    .roles_dir(".mra/roles")          // load system prompts from markdown files
    .budget(my_budget_tracker)        // optional token budget
    .port(3000)                       // HTTP port (only used with "serve")
    .build().await?
    .run().await?;
```

`run()` inspects argv to decide what to do:

| `argv[1]` | What happens |
|---|---|
| `serve` | Starts an Axum HTTP server (needs `features = ["http"]`) |
| `<agent-name> <prompt>` | One-shot CLI dispatch, prints result to stdout |
| nothing | Prints usage and exits |

## Sessions

Conversation history is handled for you. When you call `ctx.chat()`, the framework prepends all previous turns before sending to the LLM, then appends the new exchange and saves it. Your behavior code stays stateless.

Two built-in stores:
- `MemorySessionStore` -- default for CLI mode. Lost when the process exits.
- `FileSessionStore` -- default for HTTP mode. One JSON file per session in `.mra/sessions/`.

Implement the `SessionStore` trait if you need Redis, Postgres, whatever.

## HTTP API

Gated behind `features = ["http"]` so you don't pull in Axum if you don't need it.

```bash
# Start the server
cargo run --example pipeline -- serve

# New session
curl -X POST localhost:3000/agents/researcher \
  -H "Content-Type: application/json" \
  -d '{"prompt": "explain ownership in Rust"}'

# Continue a session
curl -X POST localhost:3000/agents/researcher/SESSION_ID \
  -d '{"prompt": "now explain lifetimes", "role": "teacher"}'

# Get history
curl localhost:3000/agents/researcher/SESSION_ID

# Delete session
curl -X DELETE localhost:3000/agents/researcher/SESSION_ID
```

Send `Accept: text/event-stream` and you get SSE instead of buffered JSON:

```
data: {"type":"token","content":"Ownership is"}
data: {"type":"token","content":" Rust's way of"}
data: {"type":"done","session_id":"...","usage":{...}}
```

## Sandbox

Agents can work inside an isolated temp directory. Mount real host paths in via symlinks. When the workspace drops, the temp dir gets cleaned up.

```rust
use mra::sandbox::{Sandbox, VirtualSandbox};

let mut sandbox = VirtualSandbox::with_mount("workspace", project_dir)?;
let result = sandbox.exec("ls workspace/src/", Default::default()).await?;
println!("{}", result.stdout);
```

Path traversal outside the root is rejected.

## Tools

Three built-in tools. Register them in a `ToolRegistry` and pass to the runtime:

```rust
use std::sync::Arc;
use mra::tool::{ToolRegistry, ShellTool, ReadFileTool, EditFileTool};

let tools = ToolRegistry::new();
tools.register(Arc::new(ShellTool::builder().timeout(Duration::from_secs(60)).build()))?;
tools.register(Arc::new(ReadFileTool::new()))?;
tools.register(Arc::new(EditFileTool::new()))?;

Runtime::builder()
    .tools(tools)
    // ...
```

Inside a behavior, use `ctx.chat_with_tools()` for an autonomous tool loop -- the LLM picks tools, you execute them, feed results back, repeat until it's done:

```rust
let result = ctx.chat_with_tools(
    &LlmRequest::builder()
        .system("You are a code reviewer with shell and read_file tools.")
        .user("review src/lib.rs")
        .tools(ctx.tools.specs())
        .build()
    15,  // max iterations
).await?;
```

Or call tools manually with `ctx.call_tool("shell", json!({"command": "ls"}))`. This sends heartbeats to the supervisor while the tool runs, so long commands don't trigger hang detection.

## Roles

Drop markdown files in `.mra/roles/` and they become named system prompts. The filename stem is the role name.

```bash
echo "You are an expert Rust developer. Read before you edit." > .mra/roles/coder.md
```

Pass a role through the HTTP API with `"role": "coder"` in the request body. The system message gets injected for that one call -- it's never saved to session history, so it doesn't pollute the transcript.

## Supervision

The supervisor sits in a `select!` loop watching for child exits, incoming commands, and hang-check ticks. When something goes wrong, it decides what to do based on the restart policy.

Restart strategies:
- `OneForOne` -- restart the crashed agent only
- `OneForAll` -- restart every agent when one crashes

Restart policies per child:
- `Permanent` -- always restart
- `Transient` -- restart only on failure
- `Temporary` -- never restart

Other stuff:
- Exponential backoff between restarts (configurable base and cap)
- Global restart intensity limit -- too many restarts in a window and it gives up
- Hang detection by polling each agent's last-activity timestamp
- Peer injection -- agents get handles to their siblings through `ctx.peers`
- Hot-swap mailbox via `ArcSwap` -- when an agent restarts, existing handles keep working

You can subscribe to lifecycle events:

```rust
let mut events = runtime.subscribe();
while let Ok(event) = events.recv().await {
    match event {
        SupervisorEvent::ChildStarted { name, .. } => println!("{name} started"),
        SupervisorEvent::ChildRestarted { name, .. } => println!("{name} restarted"),
        SupervisorEvent::HangDetected { name, .. } => println!("{name} hung!"),
        _ => {}
    }
}
```

## Running the examples

Set up your OpenRouter API key:

```bash
cp mra.example.toml mra.toml
# add your API key to mra.toml
```

Or use an env var:

```bash
export MRA_LLM__API_KEY="your-key-here"
```

Two examples:

```bash
# Pipeline: researcher -> writer -> editor
cargo run --example pipeline researcher "the invention of the transistor"

# Coding agent: reads files, runs commands, fixes code
cargo run --example coding_agent coder "review src/lib.rs and fix any issues"
```

The pipeline example uses three agents with different models. The coding agent uses shell, read_file, and edit_file tools, and runs an autonomous tool loop.

## Config

[Figment](https://github.com/SergioBenitez/Figment)-based, layered. Later sources override earlier ones:

1. Hardcoded defaults
2. `mra.toml` in the working directory
3. Env vars prefixed with `MRA_` (nested with `__`, e.g. `MRA_LLM__API_KEY`)

```toml
[llm]
api_key = "your-openrouter-api-key"
model = "anthropic/claude-sonnet-4"
base_url = "https://openrouter.ai/api/v1"

[runtime]
max_agents = 100
shutdown_timeout_secs = 30
```

## Wire them together

Each agent is two pieces:

- `AgentHandle` -- the external API. Cloneable, `Send + Sync`. Sends tasks through a bounded `mpsc` channel routed through an `ArcSwap` mailbox slot.
- `AgentRunner` -- the internal loop. Owns mutable state, receives messages, calls your `AgentBehavior::handle`. Runs inside the supervisor's `JoinSet`.

The `ArcSwap` mailbox slot is what makes restarts transparent. When an agent dies and the supervisor respawns it, the new `mpsc::Sender` gets swapped into the same slot. Anyone holding an `AgentHandle` -- peers, external code, whoever -- keeps sending to the same stable address. They never know the agent restarted.
