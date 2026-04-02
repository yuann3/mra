//! Resource limit constants and StoreLimits construction.

/// Default maximum linear memory per WASM invocation (64 MiB).
pub const DEFAULT_MAX_MEMORY_BYTES: usize = 64 * 1024 * 1024;

/// Hard cap on linear memory that cannot be exceeded by manifest overrides (256 MiB).
pub const MAX_MEMORY_HARD_CAP: usize = 256 * 1024 * 1024;

/// Default epoch deadline in ticks (50 ticks * 100ms = 5s).
pub const DEFAULT_EPOCH_DEADLINE_TICKS: u64 = 50;

/// Epoch tick interval in milliseconds.
pub const EPOCH_TICK_INTERVAL_MS: u64 = 100;
