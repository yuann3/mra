//! WASM sandboxed tool execution.
//!
//! Compiles and runs untrusted `.wasm` tool binaries inside Wasmtime with
//! deny-by-default permissions, bounded CPU and memory, and a JSON-in/JSON-out
//! interface that integrates with the existing [`Tool`](crate::tool::Tool) trait.

mod limits;
mod manifest;
mod tool;

pub use self::limits::*;
pub use self::tool::WasmTool;

use wasmtime::Engine;

/// Owns the Wasmtime engine and dedicated thread pool for WASM execution.
pub struct WasmRuntime {
    engine: Engine,
    pool: rayon::ThreadPool,
}

impl WasmRuntime {
    /// Creates a new WASM runtime with default settings.
    pub fn new() -> Result<Self, anyhow::Error> {
        let mut config = wasmtime::Config::new();
        config.cranelift_opt_level(wasmtime::OptLevel::Speed);

        let engine = Engine::new(&config)?;

        let pool = rayon::ThreadPoolBuilder::new()
            .num_threads(num_cpus::get())
            .build()?;

        Ok(Self { engine, pool })
    }

    /// Returns a reference to the Wasmtime engine.
    pub fn engine(&self) -> &Engine {
        &self.engine
    }

    /// Returns a reference to the thread pool.
    pub(crate) fn pool(&self) -> &rayon::ThreadPool {
        &self.pool
    }
}
