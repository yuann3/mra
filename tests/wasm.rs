#![cfg(feature = "wasm")]

use std::sync::Arc;

use serde_json::json;

use mra::tool::Tool;
use mra::wasm::WasmRuntime;
use mra::wasm::WasmTool;
use mra::tool::ToolSpec;

#[tokio::test]
async fn wasm_echo_tool_returns_input() {
    let runtime = Arc::new(WasmRuntime::new().unwrap());

    let spec = ToolSpec {
        name: "echo".into(),
        description: "Echoes input".into(),
        parameters: json!({"type": "object"}),
    };

    let wasm_path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/echo_tool.wasm");

    let tool = WasmTool::from_file(spec, &wasm_path, runtime).unwrap();

    let input = json!({"text": "hello world"});
    let output = tool.invoke(input.clone()).await.unwrap();

    assert_eq!(output.content, input.to_string());
    assert!(!output.is_error);
}
