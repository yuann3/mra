//! WASM sandboxed tool execution.
//!
//! Compiles and runs untrusted `.wasm` tool binaries inside Wasmtime with
//! deny-by-default permissions, bounded CPU and memory, and a JSON-in/JSON-out
//! interface that integrates with the existing [`Tool`](crate::tool::Tool) trait.

mod limits;
mod manifest;
mod tool;

pub use self::limits::*;
pub use self::manifest::{WasmToolConfig, WasmToolLimits, WasmToolManifest};
pub use self::tool::WasmTool;

use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::Duration;

use serde_json::json;
use thiserror::Error;
use wasmtime::{Engine, Module};

use crate::error::ToolError;
use crate::tool::ToolSpec;

/// Errors while initializing the WASM runtime or discovering WASM tools.
#[derive(Debug, Error)]
pub enum WasmError {
    /// Failed to initialize the Wasmtime engine or worker pool.
    #[error("failed to initialize wasm runtime: {0}")]
    Initialization(String),
    /// Failed to access the filesystem while scanning for tools.
    #[error(transparent)]
    Io(#[from] std::io::Error),
    /// Failed to parse the tool manifest TOML.
    #[error("invalid wasm manifest: {0}")]
    ManifestParse(#[from] toml::de::Error),
    /// The manifest failed semantic validation.
    #[error("invalid wasm manifest: {0}")]
    InvalidManifest(String),
    /// The manifest points to a missing `.wasm` binary.
    #[error("WASM binary not found: {wasm_path} (referenced by {manifest_path})")]
    MissingBinary {
        /// Path to the missing `.wasm` binary.
        wasm_path: std::path::PathBuf,
        /// Path to the manifest that referenced it.
        manifest_path: std::path::PathBuf,
    },
    /// Failed to compile a `.wasm` module.
    #[error("failed to compile wasm module: {0}")]
    Compilation(String),
    /// Failed to register a discovered tool in the runtime registry.
    #[error(transparent)]
    Registry(#[from] ToolError),
}

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
    pub fn new() -> Result<Self, WasmError> {
        Self::with_options(num_cpus::get(), EPOCH_TICK_INTERVAL_MS)
    }

    /// Creates a new WASM runtime with custom thread pool size and tick interval.
    pub fn with_options(thread_pool_size: usize, epoch_tick_ms: u64) -> Result<Self, WasmError> {
        let mut config = wasmtime::Config::new();
        config.cranelift_opt_level(wasmtime::OptLevel::Speed);
        config.epoch_interruption(true);

        let engine = Engine::new(&config).map_err(|e| WasmError::Initialization(e.to_string()))?;

        let pool = rayon::ThreadPoolBuilder::new()
            .num_threads(thread_pool_size)
            .build()
            .map_err(|e| WasmError::Initialization(e.to_string()))?;

        let ticker_stop = Arc::new(AtomicBool::new(false));
        let ticker_engine = engine.clone();
        let stop = ticker_stop.clone();
        let ticker_thread = thread::spawn(move || {
            while !stop.load(Ordering::Relaxed) {
                thread::sleep(Duration::from_millis(epoch_tick_ms));
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

    /// Scans a directory for WASM tool subdirectories and loads them.
    ///
    /// Each subdirectory must contain a `tool.toml` manifest and the `.wasm`
    /// binary it references. Modules are eagerly compiled at load time.
    pub fn load_tools(self: &Arc<Self>, dir: &Path) -> Result<Vec<WasmTool>, WasmError> {
        let mut tools = Vec::new();

        if !dir.exists() {
            return Ok(tools);
        }

        let entries = std::fs::read_dir(dir)?;
        for entry in entries {
            let entry = entry?;
            let path = entry.path();
            if !path.is_dir() {
                continue;
            }

            let manifest_path = path.join("tool.toml");
            if !manifest_path.exists() {
                continue;
            }

            let toml_str = std::fs::read_to_string(&manifest_path)?;
            let manifest = WasmToolManifest::parse(&toml_str)?;

            let wasm_path = path.join(&manifest.wasm);
            if !wasm_path.exists() {
                return Err(WasmError::MissingBinary {
                    wasm_path,
                    manifest_path,
                });
            }

            let module = Module::from_file(&self.engine, &wasm_path)
                .map_err(|e| WasmError::Compilation(e.to_string()))?;

            let parameters = manifest
                .parameters
                .clone()
                .unwrap_or_else(|| json!({"type": "object"}));

            let spec = ToolSpec {
                name: manifest.name.clone(),
                description: manifest.description.clone(),
                parameters,
            };

            let tool = WasmTool::new(spec, module, Arc::clone(self))
                .with_max_memory(manifest.limits.max_memory_bytes)
                .with_epoch_deadline(manifest.limits.epoch_deadline_ticks);

            tools.push(tool);
        }

        Ok(tools)
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
