//! Research pipeline demo: researcher -> writer -> editor.
//!
//! Three agents form a sequential pipeline under an Erlang-style
//! supervisor. Each calls the LLM via OpenRouter, then delegates to
//! the next agent in the chain.
//!
//! The supervisor automatically:
//! - Injects peer handles so agents can find each other by name
//! - Monitors agents for hangs and restarts them if needed
//! - Emits lifecycle events (started, exited, restarted, hang detected)
//!
//! Usage:
//!   cargo run --bin demo "your topic here"
//!
//! Requires `mra.toml` or `MRA_LLM__API_KEY` env var.

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

const RESEARCHER_SYSTEM: &str = "\
You are a research assistant. Given a topic, produce concise research \
notes with key facts and findings. Be factual and thorough.";

const WRITER_SYSTEM: &str = "\
You are a writer. Given research notes, write a clear and engaging \
article. Use the notes as source material.";

const EDITOR_SYSTEM: &str = "\
You are an editor. Polish the given article for clarity, grammar, and \
flow. Return the improved version.";

/// First stage of the pipeline. Calls the LLM to produce research notes
/// from a user-supplied topic, then forwards those notes to the Writer.
struct Researcher;

/// Researcher calls the LLM once, then delegates to the Writer peer.
/// The peer handle comes from supervisor injection — no manual wiring.
impl AgentBehavior for Researcher {
    async fn handle(&mut self, ctx: &mut AgentCtx, input: Task) -> Result<AgentReply, AgentError> {
        let task_id = input.id;

        let request = LlmRequest {
            model: None,
            messages: vec![
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
            ],
            temperature: Some(0.3),
            max_tokens: Some(1024),
            tools: None,
        };

        let response = ctx.chat(&request).await?;
        let tokens = response.total_tokens();

        // Peers are auto-injected by the supervisor — no manual wiring needed
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

/// Second stage. Takes research notes from the Researcher and produces
/// a draft article, then hands it off to the Editor for polishing.
struct Writer;

impl AgentBehavior for Writer {
    async fn handle(&mut self, ctx: &mut AgentCtx, input: Task) -> Result<AgentReply, AgentError> {
        let task_id = input.id;

        let request = LlmRequest {
            model: None,
            messages: vec![
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
            ],
            temperature: Some(0.7),
            max_tokens: Some(2048),
            tools: None,
        };

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
/// This is the terminal node — it returns directly instead of delegating.
struct Editor;

impl AgentBehavior for Editor {
    async fn handle(&mut self, ctx: &mut AgentCtx, input: Task) -> Result<AgentReply, AgentError> {
        let task_id = input.id;

        let request = LlmRequest {
            model: None,
            messages: vec![
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
            ],
            temperature: Some(0.3),
            max_tokens: Some(2048),
            tools: None,
        };

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

/// Helper to build a ChildSpec that uses supervisor-injected peers and LLM.
fn agent_spec<B: AgentBehavior>(
    name: &str,
    llm: Arc<dyn LlmProvider>,
    behavior_fn: fn() -> B,
) -> ChildSpec {
    let config = AgentConfig::new(name);
    ChildSpec::new(
        name,
        config.clone(),
        Arc::new(move |ctx: ChildContext| {
            let llm = llm.clone();
            let config = config.clone();
            let behavior = behavior_fn();
            Box::pin(async move {
                Ok(AgentSpawn::from_config(config, behavior)
                    .id(ctx.id)
                    .cancel(ctx.cancel)
                    .peers(ctx.peers)
                    .llm(llm)
                    .budget_opt(ctx.budget)
                    .tools(ctx.tools)
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
    .with_hang_timeout(Duration::from_secs(120))
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

    // The supervisor checks every 5s for hung agents and restarts them.
    // A global token budget of 50k caps total LLM spend across all agents.
    let sup_config = SupervisorConfig::builder()
        .hang_check_interval(Duration::from_secs(5))
        .build();
    let runtime = SwarmRuntime::with_budget(sup_config, 50_000);

    // Subscribe to supervisor lifecycle events so we can print them.
    // Events fire on start, exit, restart, hang detection, and budget limits.
    let mut events = runtime.subscribe();
    tokio::spawn(async move {
        while let Ok(event) = events.recv().await {
            match event {
                SupervisorEvent::SupervisorStarted => {
                    println!("[+] Supervisor started");
                }
                SupervisorEvent::ChildStarted { name, generation } => {
                    println!("  [ok] Agent '{name}' started (gen {generation})");
                }
                SupervisorEvent::ChildExited {
                    name, generation, ..
                } => {
                    println!("  [x] Agent '{name}' exited (gen {generation})");
                }
                SupervisorEvent::ChildRestarted {
                    name,
                    old_gen,
                    new_gen,
                    delay,
                } => {
                    println!(
                        "  [~] Agent '{name}' restarted: gen {old_gen} -> {new_gen} (after {delay:?})"
                    );
                }
                SupervisorEvent::HangDetected {
                    name,
                    generation,
                    elapsed,
                } => {
                    println!(
                        "  [?] Hang detected: '{name}' gen {generation} unresponsive for {elapsed:?}"
                    );
                }
                SupervisorEvent::SupervisorStopping => {
                    println!("[!] Supervisor stopping...");
                }
                SupervisorEvent::ChildRestartLimitExceeded { name, restarts } => {
                    println!("  [ERR] Agent '{name}' restart limit exceeded ({restarts} restarts)");
                }
                SupervisorEvent::RestartIntensityExceeded { total_restarts } => {
                    println!("  [ERR] Global restart intensity exceeded ({total_restarts} restarts)");
                }
                SupervisorEvent::BudgetExceeded { name, used, limit } => {
                    println!("  [$] Budget exceeded: '{name}' ({used}/{limit} tokens)");
                }
            }
        }
    });

    // Spawn agents in dependency order: editor first, then writer, then researcher.
    // The supervisor auto-injects peer handles so each agent can look up
    // its already-started siblings by name via ctx.peers.
    runtime
        .spawn(agent_spec("editor", llm.clone(), || Editor))
        .await?;
    runtime
        .spawn(agent_spec("writer", llm.clone(), || Writer))
        .await?;
    runtime
        .spawn(agent_spec("researcher", llm.clone(), || Researcher))
        .await?;

    let topic = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "the history of the Rust programming language".into());

    println!("\n[>] Submitting topic: {topic}");
    println!("[~] Pipeline: researcher -> writer -> editor\n");

    let researcher = runtime.get_handle_by_name("researcher").await.unwrap();
    let reply = researcher.execute(Task::new(&topic)).await?;

    println!(
        "\n[ok] Final output ({} total tokens across pipeline):\n",
        reply.total_tokens
    );
    println!("{}", reply.output);

    // Print per-agent token breakdown
    println!("\n[#] Token usage breakdown:");
    if let Some(usage) = runtime.token_usage() {
        println!(
            "   Global: {} / {} tokens",
            usage.used,
            usage.limit.map_or("∞".to_string(), |l| l.to_string())
        );
    }
    for name in &["researcher", "writer", "editor"] {
        if let Some(usage) = runtime.agent_token_usage(name) {
            println!("   {name}: {} tokens (direct LLM usage)", usage.used);
        }
    }

    runtime.shutdown().await;
    Ok(())
}
