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
        matches!(result, Err(ToolError::FuelExhausted)),
        "expected FuelExhausted, got: {result:?}"
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
