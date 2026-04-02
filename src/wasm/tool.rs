//! `WasmTool` — implements `Tool` trait for WASM guest modules.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use serde_json::Value;
use wasmtime::{Engine, Linker, Module, Store, StoreLimitsBuilder};
use wasmtime_wasi::preview1::WasiP1Ctx;
use wasmtime_wasi::WasiCtxBuilder;

use crate::error::ToolError;
use crate::tool::{Tool, ToolOutput, ToolSpec};

use super::{DEFAULT_EPOCH_DEADLINE_TICKS, DEFAULT_MAX_MEMORY_BYTES, WasmRuntime};

struct StoreState {
    wasi: WasiP1Ctx,
    limits: wasmtime::StoreLimits,
}

/// A tool backed by a compiled WASM module.
///
/// Each invocation creates a fresh `Store` and instance, so there is no
/// state leakage between calls.
pub struct WasmTool {
    spec: ToolSpec,
    module: Module,
    runtime: Arc<WasmRuntime>,
    max_memory_bytes: usize,
    epoch_deadline_ticks: u64,
}

impl WasmTool {
    /// Creates a new WASM tool from a precompiled module.
    pub fn new(spec: ToolSpec, module: Module, runtime: Arc<WasmRuntime>) -> Self {
        Self {
            spec,
            module,
            runtime,
            max_memory_bytes: DEFAULT_MAX_MEMORY_BYTES,
            epoch_deadline_ticks: DEFAULT_EPOCH_DEADLINE_TICKS,
        }
    }

    /// Creates a WASM tool by compiling a `.wasm` file.
    pub fn from_file(
        spec: ToolSpec,
        path: &std::path::Path,
        runtime: Arc<WasmRuntime>,
    ) -> Result<Self, anyhow::Error> {
        let module = Module::from_file(runtime.engine(), path)?;
        Ok(Self::new(spec, module, runtime))
    }

    /// Sets the maximum memory in bytes for this tool.
    pub fn with_max_memory(mut self, bytes: usize) -> Self {
        self.max_memory_bytes = bytes;
        self
    }

    /// Sets the epoch deadline in ticks for this tool.
    pub fn with_epoch_deadline(mut self, ticks: u64) -> Self {
        self.epoch_deadline_ticks = ticks;
        self
    }
}

impl Tool for WasmTool {
    fn spec(&self) -> &ToolSpec {
        &self.spec
    }

    fn invoke(
        &self,
        args: Value,
    ) -> Pin<Box<dyn Future<Output = Result<ToolOutput, ToolError>> + Send + '_>> {
        Box::pin(async move {
            let (tx, rx) = tokio::sync::oneshot::channel();

            let json_bytes = serde_json::to_vec(&args)
                .map_err(|e| ToolError::InvalidArgs(e.to_string()))?;

            let engine = self.runtime.engine().clone();
            let module = self.module.clone();
            let max_memory = self.max_memory_bytes;
            let epoch_deadline = self.epoch_deadline_ticks;

            self.runtime.pool().spawn(move || {
                let result =
                    invoke_in_store(&engine, &module, &json_bytes, max_memory, epoch_deadline);
                let _ = tx.send(result);
            });

            rx.await
                .map_err(|_| ToolError::ExecutionFailed("wasm execution cancelled".into()))?
        })
    }
}

fn map_trap(e: anyhow::Error) -> ToolError {
    if let Some(trap) = e.downcast_ref::<wasmtime::Trap>() {
        if *trap == wasmtime::Trap::Interrupt {
            return ToolError::ResourceExhausted;
        }
    }
    ToolError::WasmTrap(e.to_string())
}

fn invoke_in_store(
    engine: &Engine,
    module: &Module,
    json_bytes: &[u8],
    max_memory: usize,
    epoch_deadline: u64,
) -> Result<ToolOutput, ToolError> {
    let wasi = WasiCtxBuilder::new().build_p1();
    let limits = StoreLimitsBuilder::new()
        .memory_size(max_memory)
        .build();

    let mut store = Store::new(engine, StoreState { wasi, limits });
    store.limiter(|state| &mut state.limits);
    store.set_epoch_deadline(epoch_deadline);
    store.epoch_deadline_trap();

    let mut linker = Linker::new(engine);
    wasmtime_wasi::preview1::add_to_linker_sync(&mut linker, |state: &mut StoreState| {
        &mut state.wasi
    })
    .map_err(|e| ToolError::ExecutionFailed(e.to_string()))?;

    let instance = linker
        .instantiate(&mut store, module)
        .map_err(|e| ToolError::ExecutionFailed(e.to_string()))?;

    let alloc = instance
        .get_typed_func::<i32, i32>(&mut store, "alloc")
        .map_err(|_| ToolError::WasmTrap("missing required export: alloc".into()))?;

    let invoke_fn = instance
        .get_typed_func::<(i32, i32), i64>(&mut store, "invoke")
        .map_err(|_| ToolError::WasmTrap("missing required export: invoke".into()))?;

    let len = json_bytes.len() as i32;
    let ptr = alloc.call(&mut store, len).map_err(map_trap)?;

    let memory = instance
        .get_memory(&mut store, "memory")
        .ok_or_else(|| ToolError::WasmTrap("missing memory export".into()))?;

    memory.data_mut(&mut store)[ptr as usize..ptr as usize + json_bytes.len()]
        .copy_from_slice(json_bytes);

    let result = invoke_fn.call(&mut store, (ptr, len)).map_err(map_trap)?;

    let result_ptr = (result >> 32) as u32 as usize;
    let result_len = (result & 0xFFFF_FFFF) as u32 as usize;

    let result_bytes = memory.data(&store)[result_ptr..result_ptr + result_len].to_vec();

    serde_json::from_slice(&result_bytes)
        .map_err(|_| ToolError::ExecutionFailed("invalid output".into()))
}
