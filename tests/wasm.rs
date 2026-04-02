#![cfg(feature = "wasm")]

use std::sync::Arc;

use serde_json::json;

use mra::error::ToolError;
use mra::tool::Tool;
use mra::tool::ToolSpec;
use mra::wasm::WasmRuntime;
use mra::wasm::WasmTool;

fn fixture_path(name: &str) -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name)
}

fn make_spec(name: &str) -> ToolSpec {
    ToolSpec {
        name: name.into(),
        description: format!("Test tool: {name}"),
        parameters: json!({"type": "object"}),
    }
}

#[tokio::test]
async fn wasm_echo_tool_returns_input() {
    let runtime = Arc::new(WasmRuntime::new().unwrap());
    let tool =
        WasmTool::from_file(make_spec("echo"), &fixture_path("echo_tool.wasm"), runtime).unwrap();

    let input = json!({"text": "hello world"});
    let output = tool.invoke(input.clone()).await.unwrap();

    assert_eq!(output.content, input.to_string());
    assert!(!output.is_error);
}

#[tokio::test]
async fn wasm_infinite_loop_is_killed_by_epoch() {
    let runtime = Arc::new(WasmRuntime::new().unwrap());
    // Use 5 ticks (0.5s) for fast test
    let tool = WasmTool::from_file(
        make_spec("infinite_loop"),
        &fixture_path("infinite_loop_tool.wasm"),
        runtime,
    )
    .unwrap()
    .with_epoch_deadline(5);

    let start = std::time::Instant::now();
    let result = tool.invoke(json!({})).await;
    let elapsed = start.elapsed();

    assert!(
        matches!(result, Err(ToolError::ResourceExhausted)),
        "expected ResourceExhausted, got: {result:?}"
    );
    // 5 ticks * 100ms = 0.5s, with some tolerance
    assert!(elapsed.as_secs() < 3, "took too long: {elapsed:?}");
}

#[tokio::test]
async fn wasm_memory_hog_is_killed() {
    let runtime = Arc::new(WasmRuntime::new().unwrap());
    let tool = WasmTool::from_file(
        make_spec("memory_hog"),
        &fixture_path("memory_hog_tool.wasm"),
        runtime,
    )
    .unwrap();

    let result = tool.invoke(json!({})).await;

    assert!(
        matches!(result, Err(ToolError::WasmTrap(_))),
        "expected WasmTrap, got: {result:?}"
    );
}

// --- Manifest parsing ---

#[test]
fn manifest_parse_full() {
    use mra::wasm::WasmToolManifest;

    let toml = r#"
        name = "test"
        description = "A test"
        version = "1.0.0"
        wasm = "test.wasm"
        [limits]
        max_memory_bytes = 134217728
        epoch_deadline_ticks = 100
    "#;
    let m = WasmToolManifest::parse(toml).unwrap();
    assert_eq!(m.name, "test");
    assert_eq!(m.limits.max_memory_bytes, 128 * 1024 * 1024);
    assert_eq!(m.limits.epoch_deadline_ticks, 100);
}

#[test]
fn manifest_defaults_applied() {
    use mra::wasm::{DEFAULT_EPOCH_DEADLINE_TICKS, DEFAULT_MAX_MEMORY_BYTES, WasmToolManifest};

    let toml = r#"
        name = "minimal"
        description = "Minimal"
        version = "0.1.0"
        wasm = "m.wasm"
    "#;
    let m = WasmToolManifest::parse(toml).unwrap();
    assert_eq!(m.limits.max_memory_bytes, DEFAULT_MAX_MEMORY_BYTES);
    assert_eq!(m.limits.epoch_deadline_ticks, DEFAULT_EPOCH_DEADLINE_TICKS);
}

#[test]
fn manifest_rejects_memory_over_hard_cap() {
    use mra::wasm::WasmToolManifest;

    let toml = r#"
        name = "big"
        description = "Too much"
        version = "0.1.0"
        wasm = "big.wasm"
        [limits]
        max_memory_bytes = 536870912
    "#;
    assert!(WasmToolManifest::parse(toml).unwrap_err().to_string().contains("hard cap"));
}

// --- Tool discovery ---

#[tokio::test]
async fn load_tools_from_directory() {
    let runtime = Arc::new(WasmRuntime::new().unwrap());
    let tools_dir = fixture_path("tools");
    let tools = runtime.load_tools(&tools_dir).unwrap();

    assert_eq!(tools.len(), 1);
    assert_eq!(tools[0].spec().name, "echo");

    // Verify the loaded tool actually works
    let output = tools[0].invoke(json!({"text": "hi"})).await.unwrap();
    assert_eq!(output.content, r#"{"text":"hi"}"#);
    assert!(!output.is_error);
}

#[test]
fn load_tools_empty_directory() {
    let runtime = Arc::new(WasmRuntime::new().unwrap());
    let dir = tempfile::tempdir().unwrap();
    let tools = runtime.load_tools(dir.path()).unwrap();
    assert!(tools.is_empty());
}

#[test]
fn load_tools_nonexistent_directory() {
    let runtime = Arc::new(WasmRuntime::new().unwrap());
    let tools = runtime
        .load_tools(std::path::Path::new("/nonexistent/path"))
        .unwrap();
    assert!(tools.is_empty());
}

#[test]
fn load_tools_broken_wasm_fails() {
    let runtime = Arc::new(WasmRuntime::new().unwrap());
    let dir = tempfile::tempdir().unwrap();
    let tool_dir = dir.path().join("broken");
    std::fs::create_dir(&tool_dir).unwrap();
    std::fs::write(
        tool_dir.join("tool.toml"),
        r#"
            name = "broken"
            description = "Broken tool"
            version = "0.1.0"
            wasm = "broken.wasm"
        "#,
    )
    .unwrap();
    std::fs::write(tool_dir.join("broken.wasm"), b"not a valid wasm").unwrap();

    let result = runtime.load_tools(dir.path());
    assert!(result.is_err());
}

// --- Error mapping ---

#[tokio::test]
async fn wasm_bad_output_returns_execution_failed() {
    let runtime = Arc::new(WasmRuntime::new().unwrap());
    let tool = WasmTool::from_file(
        make_spec("bad_output"),
        &fixture_path("bad_output_tool.wasm"),
        runtime,
    )
    .unwrap();

    let result = tool.invoke(json!({})).await;
    match result {
        Err(ToolError::ExecutionFailed(msg)) => {
            assert!(msg.contains("invalid output"), "unexpected message: {msg}");
        }
        other => panic!("expected ExecutionFailed, got: {other:?}"),
    }
}

#[test]
fn wasm_missing_invoke_export() {
    let runtime = Arc::new(WasmRuntime::new().unwrap());
    // Use the memory_hog tool but it does have invoke...
    // Instead, create a minimal wasm with missing exports using wat
    // For now, test that a broken module is caught at load time via load_tools
    let dir = tempfile::tempdir().unwrap();
    let tool_dir = dir.path().join("no-invoke");
    std::fs::create_dir(&tool_dir).unwrap();
    std::fs::write(
        tool_dir.join("tool.toml"),
        r#"
            name = "no-invoke"
            description = "Missing invoke"
            version = "0.1.0"
            wasm = "empty.wasm"
        "#,
    )
    .unwrap();
    // Minimal valid WASM module with no exports
    // (module) in binary format
    std::fs::write(
        tool_dir.join("empty.wasm"),
        b"\x00asm\x01\x00\x00\x00",
    )
    .unwrap();

    // This should load OK (it's valid wasm), but invoking should fail
    let tools = runtime.load_tools(dir.path()).unwrap();
    assert_eq!(tools.len(), 1);

    // Try to invoke — should get a missing export error
    let rt = tokio::runtime::Runtime::new().unwrap();
    let result = rt.block_on(tools[0].invoke(json!({})));
    assert!(
        matches!(result, Err(ToolError::ExecutionFailed(_)) | Err(ToolError::WasmTrap(_))),
        "expected error for missing export, got: {result:?}"
    );
}
