#![warn(missing_docs)]

//! # mra — Multi-agent Runtime Architecture
//!
//! A Tokio-native framework for running concurrent AI agents as lightweight
//! actors with supervision, session persistence, and budget enforcement.
//!
//! ## Key features
//!
//! - **Actor model**: each agent is a handle/task pair communicating via bounded
//!   `mpsc` channels with `oneshot` request/reply.
//! - **Supervision**: Erlang-style one-for-one restart with heartbeat-based hang
//!   detection and exponential backoff.
//! - **Session persistence**: multi-turn conversation history stored via the
//!   [`SessionStore`](session::SessionStore) trait.
//! - **Budget enforcement**: per-run and per-agent token limits. Once a
//!   limit is crossed, further LLM calls fail immediately.
//! - **HTTP trigger**: optional Axum-backed REST API (requires `http` feature).

pub mod agent;
pub mod budget;
pub mod config;
pub mod error;
pub mod ids;
pub mod llm;
pub mod runtime;
pub mod session;
pub mod supervisor;
pub mod sandbox;
pub mod tool;
