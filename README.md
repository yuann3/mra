# mra

> **⚠️ Super early stage / WIP.** Everything here works but the API will change. Don't build anything serious on this yet.

A multi-agent runtime for Rust. Spawn AI agents as lightweight Tokio actors, wire them together, and let them talk to LLMs — all under an Erlang-style supervisor that keeps things running.

## What is this?

mra gives you an actor-based system where each agent gets its own async task, a bounded mailbox, and can call other agents or LLMs. Think of it as "what if Erlang/OTP supervisors, but for LLM pipelines, and in Rust."

Agents communicate through typed channels with backpressure. No shared mutable state, no locks. The supervisor watches over your agents, restarts them when they crash, detects hangs, and auto-wires peer connections.

## Quick look

Here's a 3-agent research pipeline. The researcher calls an LLM, passes results to a writer, who passes to an editor:

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

        // Delegate to the next agent — peers are auto-injected by the supervisor
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

The supervisor is modeled after Erlang/OTP. It manages agent lifecycles with:

- **Restart strategies**: `OneForOne` (restart just the crashed agent) or `OneForAll` (restart every agent when one crashes)
- **Restart policies**: `Permanent` (always restart), `Transient` (restart only on failure), `Temporary` (never restart)
- **Exponential backoff**: configurable base delay and max cap between restarts
- **Restart intensity**: global rate limit — if too many restarts happen in a time window, the supervisor gives up
- **Hang detection**: polls agent progress state and kills unresponsive agents
- **Peer injection**: agents automatically receive handles to their siblings via `ctx.peers`
- **Hot-swap mailbox**: `ArcSwap`-backed mailbox slot lets existing handles survive restarts — senders never need updating

```rust
use mra::runtime::SwarmRuntime;
use mra::supervisor::{ChildSpec, ChildRestart, SupervisorConfig};

let runtime = SwarmRuntime::new(SupervisorConfig::default());

// Spawn agents in dependency order — supervisor auto-wires peers
runtime.spawn(
    agent_spec("editor", llm.clone(), || Editor)
).await?;
runtime.spawn(
    agent_spec("writer", llm.clone(), || Writer)
).await?;
```

Subscribe to lifecycle events:

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

## Running the demo

Copy the example config and add your OpenRouter API key:

```bash
cp mra.example.toml mra.toml
# edit mra.toml with your API key
```

Or just set the env var:

```bash
export MRA_LLM__API_KEY="your-key-here"
```

Then run:

```bash
cargo run --bin demo "the invention of the transistor"
```

This fires up three supervised agents (researcher → writer → editor) that each call the LLM and pass results down the chain. The supervisor logs lifecycle events as they happen.

## Config

mra uses [figment](https://github.com/SergioBenitez/Figment) for layered config. It loads in this order (later wins):

1. Hardcoded defaults
2. `mra.toml` in the working directory
3. Environment variables prefixed with `MRA_` (nested with `__`, so `MRA_LLM__API_KEY` sets `llm.api_key`)

```toml
[llm]
api_key = "your-openrouter-api-key"
model = "anthropic/claude-sonnet-4"
base_url = "https://openrouter.ai/api/v1"

[runtime]
max_agents = 100
shutdown_timeout_secs = 30
```

## Architecture

```
┌──────────────────────────────────────────────────────┐
│                   SwarmRuntime                        │
│  ┌────────────────────────────────────────────────┐  │
│  │              SupervisorRunner                   │  │
│  │  select! loop:                                  │  │
│  │    • JoinSet — child exits (crash/normal/hang)  │  │
│  │    • mpsc    — commands (start/stop/get/shutdown)│  │
│  │    • interval — hang check tick                  │  │
│  │                                                  │  │
│  │  ┌──────────┐  ┌──────────┐  ┌──────────┐      │  │
│  │  │ Agent A  │  │ Agent B  │  │ Agent C  │      │  │
│  │  │ (Handle) │──│ (Handle) │──│ (Handle) │      │  │
│  │  └──────────┘  └──────────┘  └──────────┘      │  │
│  │       ↕ ArcSwap      ↕ ArcSwap     ↕ ArcSwap   │  │
│  │  ┌──────────┐  ┌──────────┐  ┌──────────┐      │  │
│  │  │ Mailbox  │  │ Mailbox  │  │ Mailbox  │      │  │
│  │  │ (mpsc)   │  │ (mpsc)   │  │ (mpsc)   │      │  │
│  │  └──────────┘  └──────────┘  └──────────┘      │  │
│  └────────────────────────────────────────────────┘  │
│              event_tx (broadcast)                     │
└──────────────────────────────────────────────────────┘
```

Each agent is two pieces:

- **AgentHandle** — cloneable, Send + Sync. This is what you use to send tasks and get replies. Backed by a bounded `mpsc` channel routed through an `ArcSwap` mailbox slot.
- **AgentRunner** — the internal loop that owns mutable state and calls your `AgentBehavior::handle`. Runs inside the supervisor's `JoinSet`.

The mailbox slot (`ArcSwap<mpsc::Sender>`) is the key trick: when an agent restarts, the supervisor swaps in a new sender pointing to the fresh agent task. Existing handles (held by peers or external code) keep working without any rewiring.

## What's here

- Actor system with bounded channels and backpressure
- Erlang/OTP-style supervisor with OneForOne and OneForAll strategies
- Automatic peer injection — agents discover siblings by name
- Hot-swap mailbox slots that survive restarts
- Hang detection via progress-state polling
- Exponential backoff with per-child and global restart intensity limits
- Lifecycle events via broadcast channel
- Cancellation via `CancellationToken` (graceful shutdown and hard cancel)
- LLM provider abstraction with OpenRouter client
- Figment config with env var overrides
- Error classification system (transient/permanent/overload/cancelled/budget) for retry decisions
- 70+ tests including supervisor integration tests

## Requirements

- Rust 1.91+ (edition 2024)
- An OpenRouter API key (or any OpenAI-compatible endpoint)

## License

MIT
