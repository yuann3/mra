# mra

> **WIP.** Don't depend on this yet.

You know you can just make things right, and i just want something lightweight and make me happy. So here we go, a multi-agent runtime for Rust. Spawn AI agents as lightweight Tokio actors, wire them together, and let them talk to LLMs

## What?

Each agent runs as its own async task with a bounded `mpsc` mailbox. Agents talk to each other through handles, call LLMs, and get restarted by a supervisor if they crash or stop responding. No shared mutable state.

If you've used Erlang/OTP, this is that idea applied to LLM pipelines, in Rust.

## Quick look

A writer agent that calls an LLM, then passes the result to an editor agent:

```rust
use mra::agent::{AgentBehavior, AgentCtx, AgentReply, Task};
use mra::error::AgentError;
use mra::llm::{ChatMessage, LlmRequest, Role};

struct Writer;

impl AgentBehavior for Writer {
    async fn handle(
        &mut self,
        ctx: &mut AgentCtx,
        input: Task,
    ) -> Result<AgentReply, AgentError> {
        let llm = ctx.llm.as_ref().expect("no llm");
        let response = llm.chat(&LlmRequest {
            model: None,
            messages: vec![
                ChatMessage { role: Role::System, content: "You are a writer.".into() },
                ChatMessage { role: Role::User, content: input.instruction },
            ],
            temperature: Some(0.7),
            max_tokens: Some(2048),
        }).await.map_err(AgentError::Llm)?;

        // The supervisor injects peer handles automatically
        let editor = ctx.peers.get("editor").expect("editor peer");
        let reply = editor.execute(Task::new(response.content)).await?;

        Ok(AgentReply {
            task_id: input.id,
            output: reply.output,
            tokens_used: response.total_tokens() + reply.tokens_used,
        })
    }
}
```

## Supervision

The supervisor sits in a `select!` loop watching for three things: child exits, incoming commands, and hang-check ticks. When something goes wrong, it decides what to do based on the restart policy.

Restart strategies:
- `OneForOne` -- restart the crashed agent only
- `OneForAll` -- restart every agent when one crashes

Restart policies per child:
- `Permanent` -- always restart, regardless of how it exited
- `Transient` -- restart only on failure (not normal exit)
- `Temporary` -- never restart

Other stuff the supervisor handles:
- Exponential backoff between restarts (configurable base and cap)
- A global restart intensity limit -- too many restarts in a window and it gives up
- Hang detection by polling each agent's last-activity timestamp
- Peer injection -- agents get handles to their siblings through `ctx.peers`
- Hot-swap mailbox via `ArcSwap` -- when an agent restarts, existing handles keep working because the supervisor swaps the new channel sender into the same stable slot

```rust
use mra::runtime::SwarmRuntime;
use mra::supervisor::{ChildSpec, ChildRestart, SupervisorConfig};

let runtime = SwarmRuntime::new(SupervisorConfig::default());

// Spawn in dependency order. The supervisor populates ctx.peers
// with whatever siblings are already alive.
runtime.spawn(agent_spec("editor", llm.clone(), || Editor)).await?;
runtime.spawn(agent_spec("writer", llm.clone(), || Writer)).await?;
```

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

## Tools

Agents can call tools during their `handle` loop. A tool is anything that implements the `Tool` trait -- an async function that takes JSON arguments and returns text output. The LLM sees the tool's name, description, and JSON Schema parameters, then decides when to call it.

Two built-in tools ship out of the box:

- **`ShellTool`** -- runs a shell command via `/bin/sh -c`. Configurable timeout (default 30s), `kill_on_drop`, output capped at 32 KB.
- **`ReadFileTool`** -- reads a file and returns its contents, capped at 64 KB.

Register tools in a `ToolRegistry` and pass it when spawning the agent:

```rust
use std::sync::Arc;
use mra::tool::{ToolRegistry, ShellTool, ReadFileTool};

let mut tools = ToolRegistry::new();
tools.register(Arc::new(ShellTool::new())).unwrap();
tools.register(Arc::new(ReadFileTool::new())).unwrap();
```

Inside a behavior handler, call tools through `ctx.call_tool()`. This sends periodic heartbeats to the supervisor while the tool runs, so long commands don't trigger hang detection:

```rust
let output = ctx.call_tool("shell", serde_json::json!({
    "command": "ls -la"
})).await?;
```

The tool specs are available via `ctx.tools.specs()` for forwarding to the LLM in an `LlmRequest`.

## Running the demo

Set up your OpenRouter API key:

```bash
cp mra.example.toml mra.toml
# add your API key to mra.toml
```

Or use an env var:

```bash
export MRA_LLM__API_KEY="your-key-here"
```

Run it:

```bash
cargo run --bin demo "the invention of the transistor"
```

Three agents (researcher, writer, editor) form a pipeline. Each calls the LLM and hands the result to the next one. The supervisor prints lifecycle events as it goes.

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

## Soo

Each agent is two pieces:

- `AgentHandle` -- the external API. Cloneable, `Send + Sync`. Sends tasks through a bounded `mpsc` channel routed through an `ArcSwap` mailbox slot.
- `AgentRunner` -- the internal loop. Owns mutable state, receives messages, calls your `AgentBehavior::handle`. Runs inside the supervisor's `JoinSet`.

The `ArcSwap` mailbox slot is what makes restarts transparent. When an agent dies and the supervisor respawns it, the new `mpsc::Sender` gets swapped into the same slot. Anyone holding an `AgentHandle` -- peers, external code, whoever -- keeps sending to the same stable address. They never know the agent restarted.
