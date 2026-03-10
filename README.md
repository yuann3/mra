# mra

A multi-agent runtime for Rust. Spawn AI agents as lightweight Tokio actors, wire them together, and let them talk to LLMs.

## What is this?

mra gives you an actor-based system where each agent gets its own async task, a bounded mailbox, and can call other agents or LLMs. Think of it as "what if Erlang actors, but for LLM pipelines, and in Rust."

Agents communicate through typed channels with backpressure. No shared mutable state, no locks. You wire up peers at spawn time, and agents call each other through handles.

## Quick look

Here's a 3-agent research pipeline. The researcher calls an LLM, passes results to a writer, who passes to an editor:

```rust
use std::collections::HashMap;
use std::sync::Arc;

use mra::agent::{AgentBehavior, AgentCtx, AgentReply, Task};
use mra::config::{AgentConfig, MraConfig};
use mra::error::AgentError;
use mra::llm::{ChatMessage, LlmProvider, LlmRequest, OpenRouterClient, Role};
use mra::runtime::SwarmRuntime;

struct Editor;

impl AgentBehavior for Editor {
    async fn handle(
        &mut self,
        ctx: &mut AgentCtx,
        input: Task,
    ) -> Result<AgentReply, AgentError> {
        let llm = ctx.llm.as_ref().expect("no llm configured");
        let task_id = input.id;

        let request = LlmRequest {
            model: None,
            messages: vec![
                ChatMessage { role: Role::System, content: "You are an editor.".into() },
                ChatMessage { role: Role::User, content: input.instruction },
            ],
            temperature: Some(0.3),
            max_tokens: Some(2048),
        };

        let response = llm.chat(&request).await.map_err(AgentError::Llm)?;
        let tokens = response.total_tokens();

        Ok(AgentReply { task_id, output: response.content, tokens_used: tokens })
    }
}
```

Agents delegate to peers through `ctx.peers`:

```rust
let writer = ctx.peers.get("writer").expect("writer peer not found");
let reply = writer.execute(Task::new(response.content)).await?;
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

This fires up three agents (researcher -> writer -> editor) that each call the LLM and pass results down the chain.

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

## How it works

Each agent is two pieces:

- **AgentHandle** -- cloneable, Send + Sync. This is what you use to send tasks and get replies. Backed by a bounded `mpsc` channel.
- **AgentRunner** -- the internal loop that owns mutable state and calls your `AgentBehavior::handle`. Runs as a spawned Tokio task.

You implement `AgentBehavior` and the runtime does the rest. The trait uses native async fn (RPITIT), so there's no boxing overhead in the actor loop. The `LlmProvider` trait does use `Pin<Box<dyn Future>>` for dyn-safety since providers are shared as `Arc<dyn LlmProvider>`, but the boxing cost is nothing compared to a network round-trip.

`SwarmRuntime` manages spawning, lookup by name/id, and coordinated shutdown (cancels all agents, joins with timeout).

## What's here so far

- Actor system with bounded channels and backpressure
- Cancellation via `CancellationToken` (both graceful shutdown and hard cancel)
- Progress tracking (busy/idle state via watch channels)
- Supervisor restart policy config (exponential backoff, rolling window)
- LLM provider abstraction with OpenRouter client
- Peer-to-peer agent delegation
- Figment config with env var overrides
- Error classification system (transient/permanent/overload/cancelled/budget) for retry decisions
- Research pipeline demo

## Requirements

- Rust 1.91+ (edition 2024)
- An OpenRouter API key (or any OpenAI-compatible endpoint)

## License

MIT
