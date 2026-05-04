//! HTTP trigger for the agent runtime (requires `http` feature).
//!
//! Provides an Axum-backed REST API so agents can be invoked over HTTP.
//!
//! # Routes
//!
//! | Method | Path | Description |
//! |--------|------|-------------|
//! | `POST` | `/agents/:name` | New session (runtime generates UUID v4) |
//! | `POST` | `/agents/:name/:session_id` | Continue existing session |
//! | `GET` | `/agents/:name/:session_id` | Fetch session history |
//! | `DELETE` | `/agents/:name/:session_id` | Delete session |
//!
//! # Request body
//!
//! ```json
//! { "prompt": "summarize the state of Rust async in 2025" }
//! ```
//!
//! # Response
//!
//! ```json
//! {
//!   "session_id": "abc-123",
//!   "response": "Rust async has matured...",
//!   "usage": { "self_tokens": 142, "total_tokens": 89 }
//! }
//! ```

use std::sync::Arc;

use axum::Router;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::response::sse::{Event, Sse};
use axum::routing::{delete, get, post};
use futures_util::stream;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::session::{FileSessionStore, SessionStore};

use super::{Runtime, RuntimeError};

// ── Shared state ─────────────────────────────────────────────────────────────

/// Shared state threaded through all Axum handlers.
#[derive(Clone)]
pub(crate) struct HttpState {
    runtime: Arc<Runtime>,
    store: Arc<dyn SessionStore>,
}

// ── Request / response types ──────────────────────────────────────────────────

#[derive(Deserialize)]
struct PromptBody {
    prompt: Option<String>,
}

#[derive(Serialize)]
struct AgentResponse {
    session_id: String,
    response: String,
    usage: UsageInfo,
}

#[derive(Serialize)]
struct UsageInfo {
    /// Direct LLM token spend for this agent (does not include nested agents).
    self_tokens: u64,
    /// End-to-end total including all nested agent calls.
    total_tokens: u64,
}

#[derive(Serialize)]
struct HistoryResponse {
    session_id: String,
    messages: Vec<HistoryMessage>,
}

#[derive(Serialize)]
struct HistoryMessage {
    role: String,
    content: String,
}

#[derive(Serialize)]
struct ErrorBody {
    error: String,
}

fn err(status: StatusCode, msg: impl Into<String>) -> impl IntoResponse {
    (status, axum::Json(ErrorBody { error: msg.into() }))
}

fn wants_sse(headers: &axum::http::HeaderMap) -> bool {
    headers
        .get(axum::http::header::ACCEPT)
        .and_then(|v| v.to_str().ok())
        .map(|s| s.contains("text/event-stream"))
        .unwrap_or(false)
}

// ── Handlers ─────────────────────────────────────────────────────────────────

/// POST /agents/:name — new session
async fn post_new_session(
    Path(name): Path<String>,
    State(state): State<HttpState>,
    headers: axum::http::HeaderMap,
    axum::Json(body): axum::Json<PromptBody>,
) -> impl IntoResponse {
    let prompt = match body.prompt {
        Some(p) if !p.trim().is_empty() => p,
        _ => return err(StatusCode::BAD_REQUEST, "missing or empty `prompt`").into_response(),
    };

    let session_id = Uuid::new_v4().to_string();
    dispatch_and_respond(&state, &name, &prompt, Some(session_id), wants_sse(&headers)).await
}

/// POST /agents/:name/:session_id — continue session
async fn post_continue_session(
    Path((name, session_id)): Path<(String, String)>,
    State(state): State<HttpState>,
    headers: axum::http::HeaderMap,
    axum::Json(body): axum::Json<PromptBody>,
) -> impl IntoResponse {
    let prompt = match body.prompt {
        Some(p) if !p.trim().is_empty() => p,
        _ => return err(StatusCode::BAD_REQUEST, "missing or empty `prompt`").into_response(),
    };

    dispatch_and_respond(&state, &name, &prompt, Some(session_id), wants_sse(&headers)).await
}

/// GET /agents/:name/:session_id — fetch history
async fn get_history(
    Path((_name, session_id)): Path<(String, String)>,
    State(state): State<HttpState>,
) -> impl IntoResponse {
    match state.store.load(&session_id).await {
        Ok(messages) => {
            let body = HistoryResponse {
                session_id: session_id.clone(),
                messages: messages
                    .into_iter()
                    .map(|m| HistoryMessage {
                        role: match m.role {
                            crate::session::Role::User => "user",
                            crate::session::Role::Assistant => "assistant",
                            crate::session::Role::System => "system",
                        }.to_string(),
                        content: m.content,
                    })
                    .collect(),
            };
            axum::Json(body).into_response()
        }
        Err(e) => {
            err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response()
        }
    }
}

/// DELETE /agents/:name/:session_id — delete session
async fn delete_session(
    Path((_name, session_id)): Path<(String, String)>,
    State(state): State<HttpState>,
) -> impl IntoResponse {
    match state.store.delete(&session_id).await {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(e) => err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

async fn dispatch_and_respond(
    state: &HttpState,
    agent_name: &str,
    prompt: &str,
    session_id: Option<String>,
    sse: bool,
) -> axum::response::Response {
    match state
        .runtime
        .dispatch(agent_name, prompt, session_id.clone(), Arc::clone(&state.store))
        .await
    {
        Ok(reply) => {
            let sid = session_id.unwrap_or_default();
            if sse {
                let token_data = serde_json::json!({
                    "type": "token",
                    "content": reply.output,
                });
                let done_data = serde_json::json!({
                    "type": "done",
                    "session_id": sid,
                    "usage": {
                        "self_tokens": reply.self_tokens,
                        "total_tokens": reply.total_tokens,
                    },
                });
                let events = vec![
                    Ok::<Event, std::convert::Infallible>(
                        Event::default().data(token_data.to_string()),
                    ),
                    Ok::<Event, std::convert::Infallible>(
                        Event::default().data(done_data.to_string()),
                    ),
                ];
                Sse::new(stream::iter(events)).into_response()
            } else {
                let body = AgentResponse {
                    session_id: sid,
                    response: reply.output,
                    usage: UsageInfo {
                        self_tokens: reply.self_tokens,
                        total_tokens: reply.total_tokens,
                    },
                };
                axum::Json(body).into_response()
            }
        }
        Err(RuntimeError::UnknownAgent(_)) => {
            err(StatusCode::NOT_FOUND, format!("unknown agent: {agent_name}")).into_response()
        }
        Err(e) => err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

// ── Public entry point ────────────────────────────────────────────────────────

/// Starts the Axum HTTP server for the given runtime.
///
/// Uses the runtime's configured session store if set, otherwise defaults
/// to a `FileSessionStore` at `.mra/sessions`.
pub(crate) async fn run_http(runtime: Runtime, port: u16) -> Result<(), RuntimeError> {
    let store: Arc<dyn SessionStore> = runtime
        .session_store
        .clone()
        .unwrap_or_else(|| Arc::new(FileSessionStore::new(".mra/sessions")));

    let state = HttpState {
        runtime: Arc::new(runtime),
        store,
    };

    let app = Router::new()
        .route("/agents/{name}", post(post_new_session))
        .route("/agents/{name}/{session_id}", post(post_continue_session))
        .route("/agents/{name}/{session_id}", get(get_history))
        .route("/agents/{name}/{session_id}", delete(delete_session))
        .with_state(state);

    let addr = std::net::SocketAddr::from(([0, 0, 0, 0], port));
    tracing::info!("HTTP server listening on {addr}");

    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .map_err(|e| RuntimeError::Supervisor(crate::error::SupervisorError::SpawnFailed(
            format!("failed to bind {addr}: {e}")
        )))?;

    axum::serve(listener, app)
        .await
        .map_err(|e| RuntimeError::Supervisor(crate::error::SupervisorError::SpawnFailed(
            format!("axum serve error: {e}")
        )))?;

    Ok(())
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use std::future::Future;
    use std::pin::Pin;
    use std::sync::Arc;

    use axum::body::Body;
    use axum::http::{Method, Request, StatusCode};
    use tower::ServiceExt;

    use crate::agent::{AgentBehavior, AgentCtx, AgentReply, Task};
    use crate::error::{AgentError, LlmError};
    use crate::llm::{ChatMessage, LlmProvider, LlmRequest, LlmResponse, Role};
    use crate::runtime::{AgentEntry, Runtime};
    use crate::session::MemorySessionStore;

    use super::*;

    struct MockLlm;

    impl LlmProvider for MockLlm {
        fn chat<'a>(
            &'a self,
            req: &'a LlmRequest,
        ) -> Pin<Box<dyn Future<Output = Result<LlmResponse, LlmError>> + Send + 'a>> {
            let last = req
                .messages
                .iter()
                .rev()
                .find(|m| matches!(m.role, Role::User))
                .map(|m| m.content.clone())
                .unwrap_or_default();
            Box::pin(async move {
                Ok(LlmResponse {
                    content: format!("echo:{last}"),
                    prompt_tokens: 1,
                    completion_tokens: 1,
                    tool_calls: vec![],
                })
            })
        }
    }

    struct Echo;
    impl AgentBehavior for Echo {
        async fn handle(&mut self, ctx: &mut AgentCtx, input: Task) -> Result<AgentReply, AgentError> {
            let req = LlmRequest::builder()
                .message(ChatMessage { role: Role::User, content: input.instruction.clone(), tool_calls: vec![], tool_call_id: None })
                .build();
            let resp = ctx.chat(&req).await?;
            let tokens = resp.total_tokens();
            Ok(AgentReply { task_id: input.id, output: resp.content, self_tokens: tokens, total_tokens: tokens })
        }
    }

    async fn make_app() -> (axum::Router, Arc<MemorySessionStore>) {
        let store = Arc::new(MemorySessionStore::new());
        let runtime = Runtime::builder()
            .agent(AgentEntry::new("echo", Echo))
            .llm(MockLlm)
            .build()
            .await
            .unwrap();

        let state = HttpState {
            runtime: Arc::new(runtime),
            store: Arc::clone(&store) as Arc<dyn SessionStore>,
        };

        let app = Router::new()
            .route("/agents/{name}", post(post_new_session))
            .route("/agents/{name}/{session_id}", post(post_continue_session))
            .route("/agents/{name}/{session_id}", get(get_history))
            .route("/agents/{name}/{session_id}", delete(delete_session))
            .with_state(state);

        (app, store)
    }

    #[tokio::test]
    async fn post_new_session_returns_200_with_session_id() {
        let (app, _store) = make_app().await;
        let req = Request::builder()
            .method(Method::POST)
            .uri("/agents/echo")
            .header("content-type", "application/json")
            .body(Body::from(r#"{"prompt":"hello"}"#))
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        let body = axum::body::to_bytes(resp.into_body(), 1024 * 1024).await.unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert!(!json["session_id"].as_str().unwrap_or("").is_empty());
        assert!(json["response"].as_str().unwrap_or("").contains("hello"));
    }

    #[tokio::test]
    async fn post_unknown_agent_returns_404() {
        let (app, _store) = make_app().await;
        let req = Request::builder()
            .method(Method::POST)
            .uri("/agents/nonexistent")
            .header("content-type", "application/json")
            .body(Body::from(r#"{"prompt":"hi"}"#))
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn post_missing_prompt_returns_400() {
        let (app, _store) = make_app().await;
        let req = Request::builder()
            .method(Method::POST)
            .uri("/agents/echo")
            .header("content-type", "application/json")
            .body(Body::from(r#"{}"#))
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn get_history_returns_messages() {
        let (app, store) = make_app().await;

        // Pre-seed a session
        use crate::session::{Message, Role as SRole};
        store.save("s1", &[
            Message { role: SRole::User, content: "hi".into() },
            Message { role: SRole::Assistant, content: "hello".into() },
        ]).await.unwrap();

        let req = Request::builder()
            .method(Method::GET)
            .uri("/agents/echo/s1")
            .body(Body::empty())
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = axum::body::to_bytes(resp.into_body(), 1024 * 1024).await.unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["messages"].as_array().unwrap().len(), 2);
    }

    #[tokio::test]
    async fn delete_session_returns_204() {
        let (app, store) = make_app().await;

        use crate::session::{Message, Role as SRole};
        store.save("s2", &[Message { role: SRole::User, content: "x".into() }]).await.unwrap();

        let req = Request::builder()
            .method(Method::DELETE)
            .uri("/agents/echo/s2")
            .body(Body::empty())
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::NO_CONTENT);
    }

    #[tokio::test]
    async fn sse_post_new_session_returns_sse_stream() {
        let (app, _store) = make_app().await;
        let req = Request::builder()
            .method(Method::POST)
            .uri("/agents/echo")
            .header("content-type", "application/json")
            .header("accept", "text/event-stream")
            .body(Body::from(r#"{"prompt":"hello"}"#))
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        let body = axum::body::to_bytes(resp.into_body(), 1024 * 1024).await.unwrap();
        let body_str = std::str::from_utf8(&body).unwrap();
        assert!(body_str.contains("\"type\":\"token\""), "body should contain token event");
        assert!(body_str.contains("\"type\":\"done\""), "body should contain done event");
    }

    #[tokio::test]
    async fn sse_content_type_is_event_stream() {
        let (app, _store) = make_app().await;
        let req = Request::builder()
            .method(Method::POST)
            .uri("/agents/echo")
            .header("content-type", "application/json")
            .header("accept", "text/event-stream")
            .body(Body::from(r#"{"prompt":"hello"}"#))
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        let content_type = resp
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");
        assert!(
            content_type.contains("text/event-stream"),
            "content-type should be text/event-stream, got: {content_type}"
        );
    }
}
