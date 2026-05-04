//! # Research Pipeline Demo
//!
//! Three agents form a chain: **researcher → writer → editor**.
//! Each agent calls the LLM, then delegates to the next peer.
//!
//! Showcases:
//! - `Runtime::builder()` — single entry point, no boilerplate
//! - Per-agent model selection — researcher uses a fast model, writer uses a creative one
//! - Peer delegation — agents discover each other by name via `ctx.peers`
//! - CLI trigger — `Runtime::run()` dispatches based on argv
//!
//! ## Usage
//!
//! ```text
//! # Run the researcher (which chains writer → editor automatically)
//! cargo run --bin demo researcher "the history of the Rust language"
//! ```
//!
//! Requires `MRA_LLM__API_KEY` env var or `mra.toml`.

use mra::agent::{AgentBehavior, AgentCtx, AgentReply, Task};
use mra::config::MraConfig;
use mra::error::AgentError;
use mra::llm::{ChatMessage, LlmRequest, OpenRouterClient, Role};
use mra::runtime::{AgentEntry, Runtime};

// ── Behaviors ───────────────────────────────────────────────────────────────

/// Researches a topic, then delegates to the writer.
struct Researcher;

impl AgentBehavior for Researcher {
    async fn handle(&mut self, ctx: &mut AgentCtx, input: Task) -> Result<AgentReply, AgentError> {
        let task_id = input.id;

        let resp = ctx
            .chat(
                &LlmRequest::builder()
                    .message(ChatMessage { role: Role::System, content: "You are a research assistant. Produce concise notes with key facts.".into(), tool_calls: vec![], tool_call_id: None })
                    .message(ChatMessage { role: Role::User, content: input.instruction, tool_calls: vec![], tool_call_id: None })
                    .temperature(0.3)
                    .max_tokens(1024)
                    .build(),
            )
            .await?;

        let self_tokens = resp.total_tokens();
        let writer_reply = ctx.peers["writer"].execute(Task::new(resp.content)).await?;

        Ok(AgentReply {
            task_id,
            output: writer_reply.output,
            self_tokens,
            total_tokens: self_tokens + writer_reply.total_tokens,
        })
    }
}

/// Turns research notes into a polished article, then delegates to the editor.
struct Writer;

impl AgentBehavior for Writer {
    async fn handle(&mut self, ctx: &mut AgentCtx, input: Task) -> Result<AgentReply, AgentError> {
        let task_id = input.id;

        let resp = ctx
            .chat(
                &LlmRequest::builder()
                    .message(ChatMessage { role: Role::System, content: "You are a writer. Turn research notes into a clear, engaging article.".into(), tool_calls: vec![], tool_call_id: None })
                    .message(ChatMessage { role: Role::User, content: input.instruction, tool_calls: vec![], tool_call_id: None })
                    .temperature(0.7)
                    .max_tokens(2048)
                    .build(),
            )
            .await?;

        let self_tokens = resp.total_tokens();
        let editor_reply = ctx.peers["editor"].execute(Task::new(resp.content)).await?;

        Ok(AgentReply {
            task_id,
            output: editor_reply.output,
            self_tokens,
            total_tokens: self_tokens + editor_reply.total_tokens,
        })
    }
}

/// Polishes a draft for clarity and grammar.
struct Editor;

impl AgentBehavior for Editor {
    async fn handle(&mut self, ctx: &mut AgentCtx, input: Task) -> Result<AgentReply, AgentError> {
        let task_id = input.id;

        let resp = ctx
            .chat(
                &LlmRequest::builder()
                    .message(ChatMessage { role: Role::System, content: "You are an editor. Polish the article for clarity, grammar, and flow.".into(), tool_calls: vec![], tool_call_id: None })
                    .message(ChatMessage { role: Role::User, content: input.instruction, tool_calls: vec![], tool_call_id: None })
                    .temperature(0.3)
                    .max_tokens(2048)
                    .build(),
            )
            .await?;

        let tokens = resp.total_tokens();
        Ok(AgentReply {
            task_id,
            output: resp.content,
            self_tokens: tokens,
            total_tokens: tokens,
        })
    }
}

// ── Main ────────────────────────────────────────────────────────────────────

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();

    let config = MraConfig::load()?;

    Runtime::builder()
        // Agents registered in dependency order (editor first, researcher last).
        .agent(AgentEntry::new("editor", Editor))
        .agent(AgentEntry::new("writer", Writer))
        .agent(AgentEntry::new("researcher", Researcher))
        // Per-agent model selection: the writer gets a creative model.
        .model(&config.llm.model)
        .llm(
            OpenRouterClient::builder()
                .api_key(&config.llm.api_key)
                .base_url(&config.llm.base_url)
                .default_model(&config.llm.model)
                .build(),
        )
        .build()
        .await?
        .run()
        .await?;

    Ok(())
}
