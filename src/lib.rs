#![warn(missing_docs)]

//! # mra — Multi-agent Runtime Architecture
//!
//! A Tokio-native framework for running concurrent AI agents as lightweight
//! actors with supervision, WASM-sandboxed tools, and budget enforcement.
//!
//! ## Key features
//!
//! - **Actor model**: each agent is a handle/task pair communicating via bounded
//!   `mpsc` channels with `oneshot` request/reply.
//! - **Supervision**: Erlang-style one-for-one restart with heartbeat-based hang
//!   detection and exponential backoff.
//! - **WASM sandboxing**: tools run in Wasmtime with deny-by-default permissions
//!   and fuel limits, on `spawn_blocking` to avoid starving the Tokio runtime.
//! - **Budget enforcement**: per-run and per-agent token limits. Once a
//!   limit is crossed, further LLM calls fail immediately.

pub mod agent;
pub mod budget;
pub mod config;
pub mod error;
pub mod ids;
pub mod llm;
pub mod runtime;
pub mod supervisor;
pub mod tool;
#[cfg(feature = "wasm")]
pub mod wasm;
