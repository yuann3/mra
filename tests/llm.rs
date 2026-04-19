use std::sync::Arc;

use serde_json::json;

use mra::llm::{
    ChatMessage, LlmProvider, LlmRequest, LlmResponse, OpenRouterClient, Role, ToolCall,
};

#[test]
fn test_chat_message_construction() {
    let msg = ChatMessage {
        role: Role::User,
        content: "hello".into(),
        tool_calls: vec![],
        tool_call_id: None,
    };
    assert_eq!(msg.content, "hello");
    assert!(matches!(msg.role, Role::User));
}

#[test]
fn test_llm_request_with_model() {
    let req = LlmRequest {
        model: Some("test-model".into()),
        messages: vec![ChatMessage {
            role: Role::System,
            content: "you are helpful".into(),
            tool_calls: vec![],
            tool_call_id: None,
        }],
        temperature: Some(0.7),
        max_tokens: Some(100),
        tools: None,
    };
    assert_eq!(req.model.as_deref(), Some("test-model"));
    assert_eq!(req.messages.len(), 1);
}

#[test]
fn test_llm_request_default_model_is_none() {
    let req = LlmRequest {
        model: None,
        messages: vec![],
        temperature: None,
        max_tokens: None,
        tools: None,
    };
    assert!(req.model.is_none());
}

#[test]
fn test_llm_response_total_tokens() {
    let resp = LlmResponse {
        content: "hi".into(),
        prompt_tokens: 10,
        completion_tokens: 5,
        tool_calls: vec![],
    };
    assert_eq!(resp.total_tokens(), 15);
}

#[test]
fn test_openrouter_client_creation() {
    let client = OpenRouterClient::new(
        "https://openrouter.ai/api/v1".into(),
        "test-key".into(),
        "test-model".into(),
    );
    assert_eq!(client.model(), "test-model");
}

#[test]
fn test_openrouter_is_dyn_safe() {
    let client = OpenRouterClient::new(
        "https://openrouter.ai/api/v1".into(),
        "test-key".into(),
        "test-model".into(),
    );
    let _provider: Arc<dyn LlmProvider> = Arc::new(client);
}

#[test]
fn test_role_tool_variant_exists() {
    let role = Role::Tool;
    assert!(matches!(role, Role::Tool));
}

#[test]
fn test_tool_call_construction() {
    let tc = ToolCall {
        id: "call_123".into(),
        name: "shell".into(),
        arguments: json!({"command": "ls"}),
    };
    assert_eq!(tc.id, "call_123");
    assert_eq!(tc.name, "shell");
    assert_eq!(tc.arguments["command"], "ls");
}

#[test]
fn test_chat_message_with_tool_calls() {
    let msg = ChatMessage {
        role: Role::Assistant,
        content: "".into(),
        tool_calls: vec![ToolCall {
            id: "call_1".into(),
            name: "shell".into(),
            arguments: json!({"command": "pwd"}),
        }],
        tool_call_id: None,
    };
    assert_eq!(msg.tool_calls.len(), 1);
    assert_eq!(msg.tool_calls[0].name, "shell");
}

#[test]
fn test_chat_message_tool_result() {
    let msg = ChatMessage {
        role: Role::Tool,
        content: "/home/user".into(),
        tool_calls: vec![],
        tool_call_id: Some("call_1".into()),
    };
    assert!(matches!(msg.role, Role::Tool));
    assert_eq!(msg.tool_call_id.as_deref(), Some("call_1"));
}

#[test]
fn test_llm_request_with_tools() {
    use mra::tool::ToolSpec;

    let req = LlmRequest {
        model: None,
        messages: vec![],
        temperature: None,
        max_tokens: None,
        tools: Some(vec![ToolSpec {
            name: "shell".into(),
            description: "Run a command".into(),
            parameters: json!({}),
        }]),
    };
    assert_eq!(req.tools.as_ref().unwrap().len(), 1);
}

#[test]
fn test_llm_request_without_tools() {
    let req = LlmRequest {
        model: None,
        messages: vec![],
        temperature: None,
        max_tokens: None,
        tools: None,
    };
    assert!(req.tools.is_none());
}

#[test]
fn test_llm_response_with_tool_calls() {
    let resp = LlmResponse {
        content: "".into(),
        prompt_tokens: 10,
        completion_tokens: 5,
        tool_calls: vec![ToolCall {
            id: "call_abc".into(),
            name: "read_file".into(),
            arguments: json!({"path": "foo.txt"}),
        }],
    };
    assert_eq!(resp.tool_calls.len(), 1);
    assert_eq!(resp.total_tokens(), 15);
}

#[test]
fn test_llm_response_without_tool_calls() {
    let resp = LlmResponse {
        content: "hello".into(),
        prompt_tokens: 5,
        completion_tokens: 3,
        tool_calls: vec![],
    };
    assert!(resp.tool_calls.is_empty());
}

#[test]
fn test_chat_message_serde_roundtrip_plain() {
    let msg = ChatMessage {
        role: Role::User,
        content: "hi".into(),
        tool_calls: vec![],
        tool_call_id: None,
    };
    let json = serde_json::to_string(&msg).unwrap();
    // tool_calls and tool_call_id should be skipped when empty/None
    assert!(!json.contains("tool_calls"));
    assert!(!json.contains("tool_call_id"));
    let roundtripped: ChatMessage = serde_json::from_str(&json).unwrap();
    assert_eq!(roundtripped.content, "hi");
    assert!(roundtripped.tool_calls.is_empty());
    assert!(roundtripped.tool_call_id.is_none());
}

#[test]
fn test_chat_message_serde_roundtrip_with_tools() {
    let msg = ChatMessage {
        role: Role::Assistant,
        content: "".into(),
        tool_calls: vec![ToolCall {
            id: "call_1".into(),
            name: "shell".into(),
            arguments: json!({"command": "ls"}),
        }],
        tool_call_id: None,
    };
    let json = serde_json::to_string(&msg).unwrap();
    assert!(json.contains("tool_calls"));
    let roundtripped: ChatMessage = serde_json::from_str(&json).unwrap();
    assert_eq!(roundtripped.tool_calls.len(), 1);
    assert_eq!(roundtripped.tool_calls[0].name, "shell");
}

#[test]
fn test_chat_message_serde_tool_result() {
    let msg = ChatMessage {
        role: Role::Tool,
        content: "file contents".into(),
        tool_calls: vec![],
        tool_call_id: Some("call_1".into()),
    };
    let json = serde_json::to_string(&msg).unwrap();
    assert!(json.contains("tool_call_id"));
    let roundtripped: ChatMessage = serde_json::from_str(&json).unwrap();
    assert_eq!(roundtripped.tool_call_id.as_deref(), Some("call_1"));
}

// ── LlmRequest builder tests ──────────────────────────────────────

#[test]
fn test_llm_request_builder_minimal() {
    let msg = ChatMessage {
        role: Role::User,
        content: "hi".into(),
        tool_calls: vec![],
        tool_call_id: None,
    };
    let req = LlmRequest::builder().messages(vec![msg]).build();
    assert_eq!(req.messages.len(), 1);
    assert!(req.model.is_none());
    assert!(req.temperature.is_none());
    assert!(req.max_tokens.is_none());
    assert!(req.tools.is_none());
}

#[test]
fn test_llm_request_builder_all_fields() {
    use mra::tool::ToolSpec;

    let msg = ChatMessage {
        role: Role::System,
        content: "you are helpful".into(),
        tool_calls: vec![],
        tool_call_id: None,
    };
    let tool = ToolSpec {
        name: "shell".into(),
        description: "Run a command".into(),
        parameters: json!({}),
    };
    let req = LlmRequest::builder()
        .messages(vec![msg])
        .model("gpt-4")
        .temperature(0.5)
        .max_tokens(200)
        .tools(vec![tool])
        .build();

    assert_eq!(req.model.as_deref(), Some("gpt-4"));
    assert_eq!(req.messages.len(), 1);
    assert_eq!(req.temperature, Some(0.5));
    assert_eq!(req.max_tokens, Some(200));
    assert_eq!(req.tools.as_ref().unwrap().len(), 1);
}

#[test]
fn test_llm_request_builder_accumulates_messages() {
    let msg1 = ChatMessage {
        role: Role::User,
        content: "hello".into(),
        tool_calls: vec![],
        tool_call_id: None,
    };
    let msg2 = ChatMessage {
        role: Role::Assistant,
        content: "hi".into(),
        tool_calls: vec![],
        tool_call_id: None,
    };
    let req = LlmRequest::builder().message(msg1).message(msg2).build();

    assert_eq!(req.messages.len(), 2);
    assert_eq!(req.messages[0].content, "hello");
    assert_eq!(req.messages[1].content, "hi");
}

// ── OpenRouterClient builder tests ─────────────────────────────────

#[test]
fn test_openrouter_builder_constructs() {
    let client = OpenRouterClient::builder().api_key("test-key").build();

    // Default model should be anthropic/claude-sonnet-4
    assert_eq!(client.model(), "anthropic/claude-sonnet-4");
}

#[test]
fn test_openrouter_builder_custom_model() {
    let client = OpenRouterClient::builder()
        .api_key("test-key")
        .default_model("openai/gpt-4")
        .build();

    assert_eq!(client.model(), "openai/gpt-4");
}

#[test]
fn test_openrouter_builder_custom_base_url() {
    let client = OpenRouterClient::builder()
        .api_key("test-key")
        .base_url("http://localhost:8080")
        .build();

    // Should construct without panic; model retains default
    assert_eq!(client.model(), "anthropic/claude-sonnet-4");
}

#[test]
fn test_openrouter_builder_with_temperature() {
    let client = OpenRouterClient::builder()
        .api_key("test-key")
        .default_temperature(0.3)
        .build();

    assert_eq!(client.default_temperature(), Some(0.3));
}

#[test]
fn test_openrouter_builder_temperature_default_is_none() {
    let client = OpenRouterClient::builder().api_key("test-key").build();

    assert_eq!(client.default_temperature(), None);
}
