//! Supervision demo — crash recovery, budget enforcement, runtime observability.
//!
//! Three agents under Erlang-style supervision with a mock LLM (no API key needed):
//! - "reliable" — always succeeds
//! - "flaky" — fails every 3rd request (transient errors, auto-restarted)
//! - "expensive" — high token usage, hits per-agent budget
//!
//! ```text
//! cargo run --bin demo_supervisor
//! ```

use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;
use std::time::Duration;

use mra::agent::{AgentBehavior, AgentCtx, AgentReply, AgentSpawn, Task};
use mra::budget::BudgetTracker;
use mra::config::AgentConfig;
use mra::error::AgentError;
use mra::llm::{ChatMessage, LlmProvider, LlmRequest, LlmResponse, Role};
use mra::supervisor::{
    ChildContext, ChildRestart, ChildSpec, SpawnedChild, SupervisorConfig, SupervisorEvent,
    SupervisorHandle,
};

struct MockLlm(u64);
impl LlmProvider for MockLlm {
    fn chat<'a>(
        &'a self,
        _req: &'a LlmRequest,
    ) -> Pin<Box<dyn Future<Output = Result<LlmResponse, mra::error::LlmError>> + Send + 'a>>
    {
        let t = self.0;
        Box::pin(async move {
            Ok(LlmResponse {
                content: "processed".into(),
                prompt_tokens: t / 2,
                completion_tokens: t / 2,
                tool_calls: vec![],
            })
        })
    }
}

fn user_msg(text: &str) -> ChatMessage {
    ChatMessage { role: Role::User, content: text.into(), tool_calls: vec![], tool_call_id: None }
}

async fn chat_task(ctx: &mut AgentCtx, input: &Task) -> Result<(String, u64), AgentError> {
    let req = LlmRequest::builder().message(user_msg(&input.instruction)).build();
    let r = ctx.chat(&req).await?;
    let tokens = r.total_tokens();
    Ok((r.content, tokens))
}

// --- Behaviors ---

struct Reliable;
impl AgentBehavior for Reliable {
    async fn handle(&mut self, ctx: &mut AgentCtx, input: Task) -> Result<AgentReply, AgentError> {
        let (content, tokens) = chat_task(ctx, &input).await?;
        Ok(AgentReply { task_id: input.id, output: content, self_tokens: tokens, total_tokens: tokens })
    }
}

struct Flaky(Arc<AtomicU32>);
impl AgentBehavior for Flaky {
    async fn handle(&mut self, ctx: &mut AgentCtx, input: Task) -> Result<AgentReply, AgentError> {
        if (self.0.fetch_add(1, Ordering::SeqCst) + 1).is_multiple_of(3) {
            return Err(AgentError::HandlerFailed("transient failure".into()));
        }
        let (content, tokens) = chat_task(ctx, &input).await?;
        Ok(AgentReply { task_id: input.id, output: content, self_tokens: tokens, total_tokens: tokens })
    }
}

struct Expensive;
impl AgentBehavior for Expensive {
    async fn handle(&mut self, ctx: &mut AgentCtx, input: Task) -> Result<AgentReply, AgentError> {
        let (content, tokens) = chat_task(ctx, &input).await?;
        Ok(AgentReply { task_id: input.id, output: content, self_tokens: tokens, total_tokens: tokens })
    }
}

fn make_spec<B: AgentBehavior>(
    name: &str, llm: Arc<dyn LlmProvider>,
    make: impl Fn() -> B + Send + Sync + 'static,
) -> ChildSpec {
    let cfg = AgentConfig::new(name);
    ChildSpec::new(name, cfg.clone(), Arc::new(move |ctx: ChildContext| {
        let (llm, cfg, b) = (llm.clone(), cfg.clone(), make());
        Box::pin(async move {
            Ok(AgentSpawn::from_config(cfg, b)
                .id(ctx.id).cancel(ctx.cancel).peers(ctx.peers)
                .llm(llm).budget_opt(ctx.budget).tools(ctx.tools)
                .spawn_child())
        }) as Pin<Box<dyn Future<Output = Result<SpawnedChild, mra::error::SupervisorError>> + Send>>
    }))
}

async fn print_status(supervisor: &SupervisorHandle, budget: &BudgetTracker) {
    println!("\n--- Agent Status ---");
    for c in supervisor.list_children().await {
        let s = if c.alive { "alive" } else { "dead " };
        println!("  {:<12}| {s} | gen {} | restarts: {}", c.name, c.generation, c.restart_count);
    }
    println!();
    let _ = budget; // available for callers who need it
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let low: Arc<dyn LlmProvider> = Arc::new(MockLlm(100));
    let high: Arc<dyn LlmProvider> = Arc::new(MockLlm(500));
    let budget = Arc::new(BudgetTracker::builder().global_limit(5000).build_unconnected());

    let (supervisor, _join) = SupervisorHandle::start_with_budget(
        SupervisorConfig::builder()
            .hang_check_interval(Duration::from_millis(100))
            .budget(budget.clone())
            .build(),
        Some(budget.clone()),
    );

    // Event subscriber
    let mut events = supervisor.subscribe();
    tokio::spawn(async move {
        while let Ok(ev) = events.recv().await {
            match ev {
                SupervisorEvent::SupervisorStarted => println!("[+] Supervisor started"),
                SupervisorEvent::ChildStarted { name, generation } =>
                    println!("  [ok] Agent '{name}' started (gen {generation})"),
                SupervisorEvent::ChildExited { name, generation, .. } =>
                    println!("  [x] Agent '{name}' exited (gen {generation})"),
                SupervisorEvent::ChildRestarted { name, old_gen, new_gen, delay } =>
                    println!("  [~] Agent '{name}' restarted: gen {old_gen} -> {new_gen} (after {delay:?})"),
                SupervisorEvent::BudgetExceeded { name, used, limit } =>
                    println!("  [$] Budget exceeded: '{name}' ({used}/{limit} tokens)"),
                SupervisorEvent::SupervisorStopping => println!("[!] Supervisor stopping..."),
                _ => {}
            }
        }
    });

    // Spawn agents
    let counter = Arc::new(AtomicU32::new(0));
    let c = counter.clone();
    supervisor.start_child(make_spec("reliable", low.clone(), || Reliable)).await?;
    supervisor.start_child(make_spec("flaky", low.clone(), move || Flaky(c.clone())).with_restart(ChildRestart::Transient)).await?;
    supervisor.start_child(make_spec("expensive", high, || Expensive).with_token_budget(1000)).await?;

    tokio::time::sleep(Duration::from_millis(50)).await;
    print_status(&supervisor, &budget).await;

    println!("[>] Sending tasks...\n");
    let reliable = supervisor.child("reliable").await.unwrap();
    let flaky = supervisor.child("flaky").await.unwrap();
    let expensive = supervisor.child("expensive").await.unwrap();

    // Reliable
    let r = reliable.execute(Task::new("task 1")).await?;
    println!("  [ok] reliable: {}", r.output);

    // Flaky — 3rd call fails, agent gets restarted
    for i in 1..=3 {
        match flaky.execute(Task::new(format!("task {i}"))).await {
            Ok(r) => println!("  [ok] flaky: {}", r.output),
            Err(e) => println!("  [!] flaky task {i}: {e}"),
        }
    }

    // Expensive — burns through per-agent budget
    for i in 1..=5 {
        match expensive.execute(Task::new(format!("task {i}"))).await {
            Ok(r) => println!("  [ok] expensive: {}", r.output),
            Err(e) => { println!("  [!] expensive task {i}: {e}"); break; }
        }
    }

    tokio::time::sleep(Duration::from_millis(100)).await;
    print_status(&supervisor, &budget).await;

    // Runtime budget adjustment
    println!("[>] Raising global budget to 10000...");
    budget.set_global_limit(10000);
    println!("  [ok] Budget raised\n");

    // execute_with_timeout
    println!("[>] Testing execute_with_timeout...");
    match reliable.execute_with_timeout(Task::new("quick"), Duration::from_secs(5)).await {
        Ok(r) => println!("  [ok] reliable: {}", r.output),
        Err(e) => println!("  [!] timeout: {e}"),
    }

    print_status(&supervisor, &budget).await;

    println!("[#] Final token usage:");
    let u = budget.run_usage();
    println!("  Global: {} / {} tokens", u.used, u.limit.map_or("∞".into(), |l| l.to_string()));
    for c in supervisor.list_children().await {
        if let Some(u) = budget.agent_usage(&c.name) {
            println!("  {}: {} tokens", c.name, u.used);
        }
    }

    supervisor.shutdown().await;
    Ok(())
}
