//! Coding agent demo: review, fix, and report — in a sandbox workspace.
//!
//!     cargo run --bin demo_agent coder "review src/lib.rs and fix any issues"
//!     cargo run --bin demo_agent coder  # default: run clippy + fix + write summary

use std::sync::Arc;
use std::time::Duration;

use mra::agent::{AgentBehavior, AgentCtx, AgentReply, Task};
use mra::config::MraConfig;
use mra::error::AgentError;
use mra::llm::{LlmRequest, OpenRouterClient};
use mra::runtime::{AgentEntry, Runtime};
use mra::sandbox::{ExecOptions, Sandbox, VirtualSandbox};
use mra::tool::{EditFileTool, ReadFileTool, ShellTool, ToolRegistry};

struct Coder;

impl AgentBehavior for Coder {
    async fn handle(&mut self, ctx: &mut AgentCtx, input: Task) -> Result<AgentReply, AgentError> {
        let result = ctx.chat_with_tools(
            &LlmRequest::builder()
                .system("You are an expert Rust developer with shell, read_file, and edit_file tools. \
                         Think step by step. Read before you edit. Verify changes compile.")
                .user(&input.instruction)
                .temperature(0.2)
                .max_tokens(4096)
                .tools(ctx.tools.specs())
                .build(),
            15,
        ).await?;

        println!("[ok] Done in {} iteration(s), {} tokens",
            result.iterations, result.total_prompt_tokens + result.total_completion_tokens);

        let total = result.total_prompt_tokens + result.total_completion_tokens;
        Ok(AgentReply { task_id: input.id, output: result.response.content, self_tokens: total, total_tokens: total })
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let config = MraConfig::load()?;

    // Sandbox: mount the project at /workspace so the agent can read/edit files
    let project = std::env::current_dir()?;
    let mut sandbox = VirtualSandbox::with_mount("workspace", project.clone())?;
    let ls = sandbox.exec("ls workspace/src/", ExecOptions::default()).await?;
    println!("[*] Workspace: {} -> {}", project.display(), ls.stdout.trim().replace('\n', ", "));

    // Tools: shell (60s timeout for cargo), read_file, edit_file
    let tools = ToolRegistry::new();
    tools.register(Arc::new(ShellTool::builder().timeout(Duration::from_secs(60)).build()))?;
    tools.register(Arc::new(ReadFileTool::new()))?;
    tools.register(Arc::new(EditFileTool::new()))?;

    Runtime::builder()
        .agent(AgentEntry::new("coder", Coder).model(&config.llm.model))
        .llm(OpenRouterClient::builder()
            .api_key(&config.llm.api_key)
            .base_url(&config.llm.base_url)
            .default_model(&config.llm.model)
            .build())
        .roles_dir(".mra/roles")
        .build().await?
        .run().await?;
    Ok(())
}
