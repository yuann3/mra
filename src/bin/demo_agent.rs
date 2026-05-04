//! # Coding Agent Demo
//!
//! A single autonomous agent that can review code, fix issues, and write
//! reports — all within an isolated sandbox workspace.
//!
//! Showcases:
//! - `Runtime::builder()` — framework entry point, zero boilerplate
//! - **Sandbox** — agent runs in a `TempDir` workspace with a mounted project dir
//! - **Tools** — `ShellTool`, `ReadFileTool`, `EditFileTool` scoped to workspace
//! - **Roles** — `.mra/roles/coder.md` system prompt loaded from disk
//! - **Sessions** — conversation history persisted across `ctx.chat()` calls
//! - **Model selection** — per-agent model override
//! - **Tool loop** — `ctx.chat_with_tools()` for autonomous multi-step execution
//!
//! ## Usage
//!
//! ```text
//! # Default task: run clippy, fix warnings, write a summary
//! cargo run --bin demo_agent coder
//!
//! # Custom task
//! cargo run --bin demo_agent coder "review src/lib.rs for code quality issues"
//! ```
//!
//! Requires `MRA_LLM__API_KEY` env var or `mra.toml`.

use std::sync::Arc;
use std::time::Duration;

use mra::agent::{AgentBehavior, AgentCtx, AgentReply, Task};
use mra::config::MraConfig;
use mra::error::AgentError;
use mra::llm::{ChatMessage, LlmRequest, OpenRouterClient, Role};
use mra::runtime::{AgentEntry, Runtime};
use mra::sandbox::{ExecOptions, Sandbox, VirtualSandbox};
use mra::tool::{EditFileTool, ReadFileTool, ShellTool, ToolRegistry};

const DEFAULT_TASK: &str = "\
Run `cargo clippy` on this project. Read any files with warnings. \
Fix the issues, then write a brief summary of what you changed to summary.md.";

const SYSTEM_PROMPT: &str = "\
You are an expert Rust developer. You have access to shell, read_file, \
and edit_file tools inside an isolated workspace. \
\n\
- Use `shell` to run commands (cargo, git, grep, etc.)\n\
- Use `read_file` to examine source code before making changes\n\
- Use `edit_file` with exact `old_text` matches for surgical edits\n\
\n\
Think step by step. Read before you edit. Verify your changes compile.";

const MAX_TOOL_ITERATIONS: usize = 15;

// ── Agent ───────────────────────────────────────────────────────────────────

struct Coder;

impl AgentBehavior for Coder {
    async fn handle(&mut self, ctx: &mut AgentCtx, input: Task) -> Result<AgentReply, AgentError> {
        let task_id = input.id;

        println!("  [*] Task: {}", input.instruction);
        println!("  [*] Tools: {}", ctx.tools.specs().iter().map(|s| s.name.as_str()).collect::<Vec<_>>().join(", "));
        println!("  [*] Max iterations: {MAX_TOOL_ITERATIONS}\n");

        let request = LlmRequest::builder()
            .message(ChatMessage {
                role: Role::System,
                content: SYSTEM_PROMPT.into(),
                tool_calls: vec![],
                tool_call_id: None,
            })
            .message(ChatMessage {
                role: Role::User,
                content: input.instruction,
                tool_calls: vec![],
                tool_call_id: None,
            })
            .temperature(0.2)
            .max_tokens(4096)
            .tools(ctx.tools.specs())
            .build();

        let result = ctx.chat_with_tools(&request, MAX_TOOL_ITERATIONS).await?;

        println!("\n  [ok] Done in {} iteration(s)", result.iterations);
        println!("  [#] Tokens: {} prompt + {} completion",
            result.total_prompt_tokens, result.total_completion_tokens);

        let total = result.total_prompt_tokens + result.total_completion_tokens;
        Ok(AgentReply {
            task_id,
            output: result.response.content,
            self_tokens: total,
            total_tokens: total,
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
    let _task = std::env::args().nth(2).unwrap_or_else(|| DEFAULT_TASK.into());

    // Set up the sandbox: mount the current project directory at /workspace
    // so the agent can read and modify files in an isolated environment.
    let project_dir = std::env::current_dir()?;
    let sandbox = VirtualSandbox::with_mount("workspace", project_dir.clone())?;

    println!("[*] MRA Coding Agent");
    println!("[*] Workspace: {} (mounted at sandbox/workspace)", project_dir.display());

    // Verify the sandbox works
    let mut sandbox = sandbox;
    let ls = sandbox.exec("ls workspace/src/", ExecOptions::default()).await?;
    println!("[*] Project files: {}\n", ls.stdout.trim().replace('\n', ", "));

    // Register tools — all scoped to the sandbox workspace
    let tools = ToolRegistry::new();
    tools.register(Arc::new(
        ShellTool::builder()
            .timeout(Duration::from_secs(60))
            .build(),
    ))?;
    tools.register(Arc::new(ReadFileTool::new()))?;
    tools.register(Arc::new(EditFileTool::new()))?;

    // Build the runtime with the coding agent
    Runtime::builder()
        .agent(
            AgentEntry::new("coder", Coder)
                .model(&config.llm.model),
        )
        .llm(
            OpenRouterClient::builder()
                .api_key(&config.llm.api_key)
                .base_url(&config.llm.base_url)
                .default_model(&config.llm.model)
                .build(),
        )
        .roles_dir(".mra/roles")
        .build()
        .await?
        .run()
        .await?;

    Ok(())
}
