//! Tool demo -- a single coding agent that can run shell commands,
//! read files, and edit them.
//!
//! The agent runs a multi-turn loop: it calls the LLM, the LLM
//! picks which tools to invoke, we execute them, feed the results
//! back, and repeat until the LLM has nothing left to do (or we
//! hit the iteration cap).
//!
//! By default the agent runs `cargo clippy` and fixes any warnings.
//! Pass a different task as the first argument if you want.
//!
//! ```text
//! cargo run --bin demo_tools
//! cargo run --bin demo_tools "add doc comments to src/lib.rs"
//! ```
//!
//! Needs `mra.toml` with an API key, or set `MRA_LLM__API_KEY`.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use mra::agent::{AgentBehavior, AgentCtx, AgentReply, AgentSpawn, Task};
use mra::config::{AgentConfig, MraConfig};
use mra::error::AgentError;
use mra::llm::{ChatMessage, LlmProvider, LlmRequest, OpenRouterClient, Role};
use mra::runtime::SwarmRuntime;
use mra::supervisor::{
    ChildContext, ChildRestart, ChildSpec, SpawnedChild, SupervisorConfig, SupervisorEvent,
};
use mra::tool::{EditFileTool, ReadFileTool, ShellTool, ToolOutput, ToolRegistry};

const MAX_ITERATIONS: usize = 10;

const SYSTEM_PROMPT: &str = "\
You are a code quality agent. You have access to shell, read_file, and edit_file tools. \
Run cargo clippy, then fix any warnings you find. \
Read files to understand the code before making changes. \
Use edit_file with exact old_text matches to make surgical edits.";

/// The agent behavior. Implements its own tool loop (rather than using
/// `ctx.chat_with_tools()`) so it can print each tool call as it happens.
struct Coder;

/// Cuts a string to `max` bytes on a char boundary, appending "..." if truncated.
fn truncate_display(s: &str, max: usize) -> String {
    if s.len() <= max {
        s.to_string()
    } else {
        let mut end = max;
        while end > 0 && !s.is_char_boundary(end) {
            end -= 1;
        }
        format!("{}...", &s[..end])
    }
}

impl AgentBehavior for Coder {
    async fn handle(&mut self, ctx: &mut AgentCtx, input: Task) -> Result<AgentReply, AgentError> {
        let task_id = input.id;
        let tool_specs: Vec<_> = ctx.tools.specs().into_iter().cloned().collect();

        let mut messages = vec![
            ChatMessage {
                role: Role::System,
                content: SYSTEM_PROMPT.into(),
                tool_calls: vec![],
                tool_call_id: None,
            },
            ChatMessage {
                role: Role::User,
                content: input.instruction,
                tool_calls: vec![],
                tool_call_id: None,
            },
        ];

        let mut total_prompt = 0u64;
        let mut total_completion = 0u64;

        for iteration in 1..=MAX_ITERATIONS {
            let request = LlmRequest {
                model: None,
                messages: messages.clone(),
                temperature: Some(0.2),
                max_tokens: Some(4096),
                tools: Some(tool_specs.clone()),
            };

            let response = ctx.chat(&request).await?;
            total_prompt += response.prompt_tokens;
            total_completion += response.completion_tokens;

            if response.tool_calls.is_empty() {
                println!("  📝 Iteration {iteration}: agent responded with text");
                return Ok(AgentReply {
                    task_id,
                    output: response.content,
                    self_tokens: total_prompt + total_completion,
                    total_tokens: total_prompt + total_completion,
                });
            }

            println!(
                "  🔄 Iteration {iteration}: {} tool call(s)",
                response.tool_calls.len()
            );

            // Append assistant message with tool calls
            messages.push(ChatMessage {
                role: Role::Assistant,
                content: response.content.clone(),
                tool_calls: response.tool_calls.clone(),
                tool_call_id: None,
            });

            // Execute each tool call
            for call in &response.tool_calls {
                let args_display = truncate_display(&call.arguments.to_string(), 120);
                println!("    🔧 {}: {}", call.name, args_display);

                let tool_result = match ctx.call_tool(&call.name, call.arguments.clone()).await {
                    Ok(output) => output,
                    Err(err) => ToolOutput {
                        content: format!("Tool error: {err}"),
                        is_error: true,
                    },
                };

                let status = if tool_result.is_error { "❌" } else { "✅" };
                let content_display = truncate_display(&tool_result.content, 200);
                println!("    {status} {content_display}");

                messages.push(ChatMessage {
                    role: Role::Tool,
                    content: tool_result.content,
                    tool_calls: vec![],
                    tool_call_id: Some(call.id.clone()),
                });
            }

            ctx.report_progress();
        }

        println!("  ⚠️  Max iterations ({MAX_ITERATIONS}) reached");
        Ok(AgentReply {
            task_id,
            output: "Max iterations reached".into(),
            self_tokens: total_prompt + total_completion,
            total_tokens: total_prompt + total_completion,
        })
    }
}

/// Registers the three tools the coder agent has access to.
/// Shell gets a longer timeout (60s) since `cargo clippy` can be slow.
fn build_tool_registry() -> ToolRegistry {
    let mut registry = ToolRegistry::new();
    registry
        .register(Arc::new(ShellTool::with_timeout(Duration::from_secs(60))))
        .unwrap();
    registry.register(Arc::new(ReadFileTool::new())).unwrap();
    registry.register(Arc::new(EditFileTool::new())).unwrap();
    registry
}

/// Builds a [`ChildSpec`] for the coder agent. The tools are passed
/// into the factory closure so they survive supervisor restarts.
fn coder_spec(llm: Arc<dyn LlmProvider>, tools: ToolRegistry) -> ChildSpec {
    let config = AgentConfig::new("coder");
    ChildSpec::new(
        "coder",
        config.clone(),
        Arc::new(move |ctx: ChildContext| {
            let llm = llm.clone();
            let tools = tools.clone();
            let config = config.clone();
            Box::pin(async move {
                Ok(AgentSpawn::from_config(config, Coder)
                    .id(ctx.id)
                    .cancel(ctx.cancel)
                    .peers(ctx.peers)
                    .llm(llm)
                    .budget_opt(ctx.budget)
                    .tools(tools)
                    .spawn_child())
            })
                as Pin<
                    Box<
                        dyn Future<Output = Result<SpawnedChild, mra::error::SupervisorError>>
                            + Send,
                    >,
                >
        }),
    )
    .with_restart(ChildRestart::Permanent)
    .with_hang_timeout(Duration::from_secs(300))
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();

    let config = MraConfig::load()?;
    let llm: Arc<dyn LlmProvider> = Arc::new(OpenRouterClient::new(
        config.llm.base_url.clone(),
        config.llm.api_key.clone(),
        config.llm.model.clone(),
    ));

    let sup_config = SupervisorConfig::builder()
        .hang_check_interval(Duration::from_secs(5))
        .build();
    let runtime = SwarmRuntime::with_budget(sup_config, 100_000);

    // Subscribe to supervisor events
    let mut events = runtime.subscribe();
    let event_task = tokio::spawn(async move {
        while let Ok(event) = events.recv().await {
            match event {
                SupervisorEvent::SupervisorStarted => {
                    println!("🟢 Supervisor started");
                }
                SupervisorEvent::ChildStarted { name, generation } => {
                    println!("  ✅ Agent '{name}' started (gen {generation})");
                }
                SupervisorEvent::ChildExited {
                    name, generation, ..
                } => {
                    println!("  ⛔ Agent '{name}' exited (gen {generation})");
                }
                SupervisorEvent::ChildRestarted {
                    name,
                    old_gen,
                    new_gen,
                    delay,
                } => {
                    println!(
                        "  🔄 Agent '{name}' restarted: gen {old_gen} → {new_gen} (after {delay:?})"
                    );
                }
                SupervisorEvent::HangDetected {
                    name,
                    generation,
                    elapsed,
                } => {
                    println!(
                        "  ⏳ Hang detected: '{name}' gen {generation} unresponsive for {elapsed:?}"
                    );
                }
                SupervisorEvent::SupervisorStopping => {
                    println!("🔴 Supervisor stopping...");
                }
                SupervisorEvent::ChildRestartLimitExceeded { name, restarts } => {
                    println!("  ❌ Agent '{name}' restart limit exceeded ({restarts} restarts)");
                }
                SupervisorEvent::RestartIntensityExceeded { total_restarts } => {
                    println!("  ❌ Global restart intensity exceeded ({total_restarts} restarts)");
                }
                SupervisorEvent::BudgetExceeded { name, used, limit } => {
                    println!("  💰 Budget exceeded: '{name}' ({used}/{limit} tokens)");
                }
            }
        }
    });

    let tools = build_tool_registry();
    runtime.spawn(coder_spec(llm.clone(), tools)).await?;

    let task = std::env::args().nth(1).unwrap_or_else(|| {
        "Run cargo clippy on this project, read any files with warnings, and fix the issues".into()
    });

    println!("\n🔧 Task: {task}");
    println!("🛠️  Tools: shell, read_file, edit_file");
    println!("🔄 Max iterations: {MAX_ITERATIONS}\n");

    let coder = runtime.get_handle_by_name("coder").await.unwrap();
    let reply = coder.execute(Task::new(&task)).await?;

    println!("\n✅ Done ({} total tokens):\n", reply.total_tokens);
    println!("{}", reply.output);

    // Token breakdown
    println!("\n📊 Token usage:");
    if let Some(usage) = runtime.token_usage() {
        println!(
            "   Global: {} / {} tokens",
            usage.used,
            usage.limit.map_or("∞".to_string(), |l| l.to_string())
        );
    }
    if let Some(usage) = runtime.agent_token_usage("coder") {
        println!("   coder: {} tokens (direct LLM usage)", usage.used);
    }

    runtime.shutdown().await;
    let _ = event_task.await;
    Ok(())
}
