//! Minimal pipeline demo: researcher → writer → editor in ~40 lines.
//!
//!     cargo run --bin demo researcher "the history of Rust"

use mra::agent::{AgentBehavior, AgentCtx, AgentReply, Task};
use mra::config::MraConfig;
use mra::error::AgentError;
use mra::llm::{LlmRequest, OpenRouterClient};
use mra::runtime::{AgentEntry, Runtime};

struct Researcher;
impl AgentBehavior for Researcher {
    async fn handle(&mut self, ctx: &mut AgentCtx, input: Task) -> Result<AgentReply, AgentError> {
        let resp = ctx
            .chat(
                &LlmRequest::builder()
                    .system("You are a research assistant. Produce concise notes with key facts.")
                    .user(&input.instruction)
                    .build(),
            )
            .await?;
        let next = ctx.peers["writer"]
            .execute(Task::new(&resp.content))
            .await?;
        Ok(AgentReply {
            task_id: input.id,
            output: next.output,
            self_tokens: resp.total_tokens(),
            total_tokens: resp.total_tokens() + next.total_tokens,
        })
    }
}

struct Writer;
impl AgentBehavior for Writer {
    async fn handle(&mut self, ctx: &mut AgentCtx, input: Task) -> Result<AgentReply, AgentError> {
        let resp = ctx
            .chat(
                &LlmRequest::builder()
                    .system("You are a writer. Turn research notes into a clear, engaging article.")
                    .user(&input.instruction)
                    .build(),
            )
            .await?;
        let next = ctx.peers["editor"]
            .execute(Task::new(&resp.content))
            .await?;
        Ok(AgentReply {
            task_id: input.id,
            output: next.output,
            self_tokens: resp.total_tokens(),
            total_tokens: resp.total_tokens() + next.total_tokens,
        })
    }
}

struct Editor;
impl AgentBehavior for Editor {
    async fn handle(&mut self, ctx: &mut AgentCtx, input: Task) -> Result<AgentReply, AgentError> {
        let resp = ctx
            .chat(
                &LlmRequest::builder()
                    .system("You are an editor. Polish the article for clarity, grammar, and flow.")
                    .user(&input.instruction)
                    .build(),
            )
            .await?;
        Ok(AgentReply::from_response(&input, &resp))
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let config = MraConfig::load()?;
    Runtime::builder()
        // Each agent can use a different model — the writer gets a creative one.
        .agent(AgentEntry::new("editor", Editor).model("anthropic/claude-haiku-4-5"))
        .agent(AgentEntry::new("writer", Writer).model("anthropic/claude-sonnet-4"))
        .agent(AgentEntry::new("researcher", Researcher).model("anthropic/claude-haiku-4-5"))
        .llm(
            OpenRouterClient::builder()
                .api_key(&config.llm.api_key)
                .base_url(&config.llm.base_url)
                .build(),
        )
        .build()
        .await?
        .run()
        .await?;
    Ok(())
}
