//! Research pipeline demo: researcher -> writer -> editor.
//!
//! Three agents form a sequential pipeline. Each calls the LLM via OpenRouter,
//! then delegates to the next agent in the chain. The pipeline is registered
//! with `Runtime::builder()` and dispatched via the CLI trigger.
//!
//! Usage:
//!   cargo run --bin demo researcher "your topic here"
//!
//! Requires `mra.toml` or `MRA_LLM__API_KEY` env var.

use mra::agent::{AgentBehavior, AgentCtx, AgentReply, Task};
use mra::config::MraConfig;
use mra::error::AgentError;
use mra::llm::{ChatMessage, LlmRequest, OpenRouterClient, Role};
use mra::runtime::{AgentEntry, Runtime};

const RESEARCHER_SYSTEM: &str = "\
You are a research assistant. Given a topic, produce concise research \
notes with key facts and findings. Be factual and thorough.";

const WRITER_SYSTEM: &str = "\
You are a writer. Given research notes, write a clear and engaging \
article. Use the notes as source material.";

const EDITOR_SYSTEM: &str = "\
You are an editor. Polish the given article for clarity, grammar, and \
flow. Return the improved version.";

/// First stage of the pipeline. Calls the LLM to produce research notes,
/// then forwards those notes to the Writer.
struct Researcher;

impl AgentBehavior for Researcher {
    async fn handle(&mut self, ctx: &mut AgentCtx, input: Task) -> Result<AgentReply, AgentError> {
        let task_id = input.id;

        let request = LlmRequest::builder()
            .messages(vec![
                ChatMessage {
                    role: Role::System,
                    content: RESEARCHER_SYSTEM.into(),
                    tool_calls: vec![],
                    tool_call_id: None,
                },
                ChatMessage {
                    role: Role::User,
                    content: input.instruction,
                    tool_calls: vec![],
                    tool_call_id: None,
                },
            ])
            .temperature(0.3)
            .max_tokens(1024)
            .build();

        let response = ctx.chat(&request).await?;
        let tokens = response.total_tokens();

        let writer = ctx.peers.get("writer").expect("writer peer not found");
        let writer_reply = writer.execute(Task::new(response.content)).await?;

        Ok(AgentReply {
            task_id,
            output: writer_reply.output,
            self_tokens: tokens,
            total_tokens: tokens + writer_reply.total_tokens,
        })
    }
}

/// Second stage. Takes research notes and produces a draft article.
struct Writer;

impl AgentBehavior for Writer {
    async fn handle(&mut self, ctx: &mut AgentCtx, input: Task) -> Result<AgentReply, AgentError> {
        let task_id = input.id;

        let request = LlmRequest::builder()
            .messages(vec![
                ChatMessage {
                    role: Role::System,
                    content: WRITER_SYSTEM.into(),
                    tool_calls: vec![],
                    tool_call_id: None,
                },
                ChatMessage {
                    role: Role::User,
                    content: input.instruction,
                    tool_calls: vec![],
                    tool_call_id: None,
                },
            ])
            .temperature(0.7)
            .max_tokens(2048)
            .build();

        let response = ctx.chat(&request).await?;
        let tokens = response.total_tokens();

        let editor = ctx.peers.get("editor").expect("editor peer not found");
        let editor_reply = editor.execute(Task::new(response.content)).await?;

        Ok(AgentReply {
            task_id,
            output: editor_reply.output,
            self_tokens: tokens,
            total_tokens: tokens + editor_reply.total_tokens,
        })
    }
}

/// Final stage. Polishes the Writer's draft for clarity and grammar.
struct Editor;

impl AgentBehavior for Editor {
    async fn handle(&mut self, ctx: &mut AgentCtx, input: Task) -> Result<AgentReply, AgentError> {
        let task_id = input.id;

        let request = LlmRequest::builder()
            .messages(vec![
                ChatMessage {
                    role: Role::System,
                    content: EDITOR_SYSTEM.into(),
                    tool_calls: vec![],
                    tool_call_id: None,
                },
                ChatMessage {
                    role: Role::User,
                    content: input.instruction,
                    tool_calls: vec![],
                    tool_call_id: None,
                },
            ])
            .temperature(0.3)
            .max_tokens(2048)
            .build();

        let response = ctx.chat(&request).await?;
        let tokens = response.total_tokens();

        Ok(AgentReply {
            task_id,
            output: response.content,
            self_tokens: tokens,
            total_tokens: tokens,
        })
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();

    let config = MraConfig::load()?;
    let llm = OpenRouterClient::builder()
        .api_key(&config.llm.api_key)
        .base_url(&config.llm.base_url)
        .default_model(&config.llm.model)
        .build();

    // Register all three agents. Spawn in dependency order so peers are
    // available: editor first, then writer (which needs editor), then
    // researcher (which needs writer).
    Runtime::builder()
        .agent(AgentEntry::new("editor", Editor))
        .agent(AgentEntry::new("writer", Writer))
        .agent(AgentEntry::new("researcher", Researcher))
        .model(&config.llm.model)
        .llm(llm)
        .build()
        .await?
        .run()
        .await?;

    Ok(())
}
