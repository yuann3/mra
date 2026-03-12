//! Research pipeline demo: researcher → writer → editor.
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

use mra::agent::{AgentBehavior, AgentCtx, AgentHandle, AgentReply, Task};
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

struct Researcher;

impl AgentBehavior for Researcher {
    async fn handle(&mut self, ctx: &mut AgentCtx, input: Task) -> Result<AgentReply, AgentError> {
        let llm = ctx.llm.as_ref().expect("no llm configured");
        let task_id = input.id;

        let request = LlmRequest {
            model: None,
            messages: vec![
                ChatMessage {
                    role: Role::System,
                    content: RESEARCHER_SYSTEM.into(),
                },
                ChatMessage {
                    role: Role::User,
                    content: input.instruction,
                },
            ],
            temperature: Some(0.3),
            max_tokens: Some(1024),
        };

        let response = llm.chat(&request).await.map_err(AgentError::Llm)?;
        let tokens = response.total_tokens();

        // Peers are auto-injected by the supervisor — no manual wiring needed
        let writer = ctx.peers.get("writer").expect("writer peer not found");
        let writer_reply = writer.execute(Task::new(response.content)).await?;

        Ok(AgentReply {
            task_id,
            output: writer_reply.output,
            tokens_used: tokens + writer_reply.tokens_used,
        })
    }
}

struct Writer;

impl AgentBehavior for Writer {
    async fn handle(&mut self, ctx: &mut AgentCtx, input: Task) -> Result<AgentReply, AgentError> {
        let llm = ctx.llm.as_ref().expect("no llm configured");
        let task_id = input.id;

        let request = LlmRequest {
            model: None,
            messages: vec![
                ChatMessage {
                    role: Role::System,
                    content: WRITER_SYSTEM.into(),
                },
                ChatMessage {
                    role: Role::User,
                    content: input.instruction,
                },
            ],
            temperature: Some(0.7),
            max_tokens: Some(2048),
        };

        let response = llm.chat(&request).await.map_err(AgentError::Llm)?;
        let tokens = response.total_tokens();

        let editor = ctx.peers.get("editor").expect("editor peer not found");
        let editor_reply = editor.execute(Task::new(response.content)).await?;

        Ok(AgentReply {
            task_id,
            output: editor_reply.output,
            tokens_used: tokens + editor_reply.tokens_used,
        })
    }
}

struct Editor;

impl AgentBehavior for Editor {
    async fn handle(&mut self, ctx: &mut AgentCtx, input: Task) -> Result<AgentReply, AgentError> {
        let llm = ctx.llm.as_ref().expect("no llm configured");
        let task_id = input.id;

        let request = LlmRequest {
            model: None,
            messages: vec![
                ChatMessage {
                    role: Role::System,
                    content: EDITOR_SYSTEM.into(),
                },
                ChatMessage {
                    role: Role::User,
                    content: input.instruction,
                },
            ],
            temperature: Some(0.3),
            max_tokens: Some(2048),
        };

        let response = llm.chat(&request).await.map_err(AgentError::Llm)?;
        let tokens = response.total_tokens();

        Ok(AgentReply {
            task_id,
            output: response.content,
            tokens_used: tokens,
        })
    }
}

/// Helper to build a ChildSpec that uses supervisor-injected peers and LLM.
fn agent_spec<B: AgentBehavior>(
    name: &str,
    llm: Arc<dyn LlmProvider>,
    behavior_fn: fn() -> B,
) -> ChildSpec {
    let agent_name = name.to_string();
    ChildSpec::new(
        name,
        AgentConfig::new(name),
        Arc::new(move |ctx: ChildContext| {
            let llm = llm.clone();
            let agent_name = agent_name.clone();
            let behavior = behavior_fn();
            Box::pin(async move {
                Ok(AgentHandle::spawn_child(
                    ctx.id,
                    AgentConfig::new(&agent_name),
                    behavior,
                    ctx.peers, // Supervisor populates this with alive siblings
                    Some(llm),
                    ctx.cancel,
                ))
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

    let sup_config = SupervisorConfig {
        hang_check_interval: Duration::from_secs(5),
        ..Default::default()
    };
    let runtime = SwarmRuntime::new(sup_config);

    // Subscribe to supervisor events and log them in the background
    let mut events = runtime.subscribe();
    tokio::spawn(async move {
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
            }
        }
    });

    // Spawn agents in dependency order (editor first, then writer, then researcher).
    // The supervisor auto-injects peer handles — each agent can see its
    // already-started siblings via ctx.peers.
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

    println!("\n📚 Submitting topic: {topic}");
    println!("🔄 Pipeline: researcher → writer → editor\n");

    let researcher = runtime.get_handle_by_name("researcher").await.unwrap();
    let reply = researcher.execute(Task::new(&topic)).await?;

    println!("\n✅ Final output ({} tokens used):\n", reply.tokens_used);
    println!("{}", reply.output);

    runtime.shutdown().await;
    Ok(())
}
