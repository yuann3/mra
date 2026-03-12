//! Research pipeline demo: researcher → writer → editor.
//!
//! Three agents form a sequential pipeline. Each calls the LLM via
//! OpenRouter, then delegates to the next agent in the chain.
//!
//! Usage:
//!   cargo run --bin demo "your topic here"
//!
//! Requires `mra.toml` or `MRA_LLM__API_KEY` env var.

use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use mra::agent::{AgentBehavior, AgentCtx, AgentHandle, AgentReply, Task};
use mra::config::{AgentConfig, MraConfig};
use mra::error::AgentError;
use mra::llm::{ChatMessage, LlmProvider, LlmRequest, OpenRouterClient, Role};
use mra::runtime::SwarmRuntime;
use mra::supervisor::{ChildContext, ChildSpec, SpawnedChild, SupervisorConfig};

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

    let runtime = SwarmRuntime::new(SupervisorConfig::default());

    // Editor (no peers)
    let llm_c = llm.clone();
    let editor_spec = ChildSpec::new(
        "editor",
        AgentConfig::new("editor"),
        Arc::new(move |ctx: ChildContext| {
            let llm = llm_c.clone();
            Box::pin(async move {
                Ok(AgentHandle::spawn_child(
                    ctx.id,
                    AgentConfig::new("editor"),
                    Editor,
                    HashMap::new(),
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
    );
    let editor_handle = runtime.spawn(editor_spec).await?;

    // Writer (peers: editor)
    let llm_c = llm.clone();
    let eh = editor_handle.clone();
    let writer_spec = ChildSpec::new(
        "writer",
        AgentConfig::new("writer"),
        Arc::new(move |ctx: ChildContext| {
            let llm = llm_c.clone();
            let editor = eh.clone();
            Box::pin(async move {
                let mut peers = HashMap::new();
                peers.insert("editor".into(), editor);
                Ok(AgentHandle::spawn_child(
                    ctx.id,
                    AgentConfig::new("writer"),
                    Writer,
                    peers,
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
    );
    let writer_handle = runtime.spawn(writer_spec).await?;

    // Researcher (peers: writer)
    let llm_c = llm.clone();
    let wh = writer_handle.clone();
    let researcher_spec = ChildSpec::new(
        "researcher",
        AgentConfig::new("researcher"),
        Arc::new(move |ctx: ChildContext| {
            let llm = llm_c.clone();
            let writer = wh.clone();
            Box::pin(async move {
                let mut peers = HashMap::new();
                peers.insert("writer".into(), writer);
                Ok(AgentHandle::spawn_child(
                    ctx.id,
                    AgentConfig::new("researcher"),
                    Researcher,
                    peers,
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
    );
    runtime.spawn(researcher_spec).await?;

    let topic = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "the history of the Rust programming language".into());

    println!("📚 Submitting topic: {topic}");
    println!("🔄 Pipeline: researcher → writer → editor\n");

    let researcher = runtime.get_handle_by_name("researcher").await.unwrap();
    let reply = researcher.execute(Task::new(&topic)).await?;

    println!("✅ Final output ({} tokens used):\n", reply.tokens_used);
    println!("{}", reply.output);

    runtime.shutdown().await;
    Ok(())
}
