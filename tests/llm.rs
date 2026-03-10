use std::sync::Arc;

use mra::llm::{ChatMessage, LlmProvider, LlmRequest, LlmResponse, OpenRouterClient, Role};

#[test]
fn test_chat_message_construction() {
    let msg = ChatMessage {
        role: Role::User,
        content: "hello".into(),
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
        }],
        temperature: Some(0.7),
        max_tokens: Some(100),
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
    };
    assert!(req.model.is_none());
}

#[test]
fn test_llm_response_total_tokens() {
    let resp = LlmResponse {
        content: "hi".into(),
        prompt_tokens: 10,
        completion_tokens: 5,
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
