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

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::Duration;

use wasmtime::Engine;

/// Owns the Wasmtime engine and dedicated thread pool for WASM execution.
///
/// Spawns a background epoch ticker thread that increments the engine's
/// epoch counter every [`EPOCH_TICK_INTERVAL_MS`] milliseconds. This drives
/// epoch-based interruption for CPU-bound WASM tools.
pub struct WasmRuntime {
    engine: Engine,
    pool: rayon::ThreadPool,
    ticker_stop: Arc<AtomicBool>,
    ticker_thread: Option<thread::JoinHandle<()>>,
}

impl WasmRuntime {
    /// Creates a new WASM runtime with default settings.
    ///
    /// Enables epoch interruption on the engine and spawns the ticker thread.
    pub fn new() -> Result<Self, anyhow::Error> {
        let mut config = wasmtime::Config::new();
        config.cranelift_opt_level(wasmtime::OptLevel::Speed);
        config.epoch_interruption(true);

        let engine = Engine::new(&config)?;

        let pool = rayon::ThreadPoolBuilder::new()
            .num_threads(num_cpus::get())
            .build()?;

        let ticker_stop = Arc::new(AtomicBool::new(false));
        let ticker_engine = engine.clone();
        let stop = ticker_stop.clone();
        let ticker_thread = thread::spawn(move || {
            while !stop.load(Ordering::Relaxed) {
                thread::sleep(Duration::from_millis(EPOCH_TICK_INTERVAL_MS));
                ticker_engine.increment_epoch();
            }
        });

        Ok(Self {
            engine,
            pool,
            ticker_stop,
            ticker_thread: Some(ticker_thread),
        })
    }

    /// Returns a reference to the Wasmtime engine.
    pub fn engine(&self) -> &Engine {
        &self.engine
    }

    /// Returns a reference to the thread pool.
    pub(crate) fn pool(&self) -> &rayon::ThreadPool {
        &self.pool
    }

    /// Stops the epoch ticker thread and shuts down the thread pool.
    pub fn shutdown(&mut self) {
        self.ticker_stop.store(true, Ordering::Relaxed);
        if let Some(handle) = self.ticker_thread.take() {
            let _ = handle.join();
        }
    }
}

impl Drop for WasmRuntime {
    fn drop(&mut self) {
        self.shutdown();
    }
}
