//! OpenRouter (OpenAI-compatible) LLM client.
//!
//! Serialization borrows request data — no cloning of messages or model
//! strings per call. The chat endpoint URL is precomputed at construction.

use std::future::Future;
use std::pin::Pin;

use reqwest::header::{AUTHORIZATION, CONTENT_TYPE, HeaderMap, HeaderValue};
use serde::{Deserialize, Serialize};

use super::{LlmProvider, LlmRequest, LlmResponse};
use crate::error::LlmError;

/// Client for OpenRouter's OpenAI-compatible chat completions API.
///
/// Holds a shared `reqwest::Client` (connection pool) and precomputed
/// endpoint URL. Cheap to clone via `Arc` internally.
pub struct OpenRouterClient {
    http: reqwest::Client,
    chat_url: String,
    model: String,
    default_temperature: Option<f32>,
}

/// Borrows request data for serialization — avoids cloning messages.
#[derive(Serialize)]
struct ApiRequest<'a> {
    model: &'a str,
    messages: Vec<ApiOutMessage<'a>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    temperature: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    max_tokens: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tools: Option<Vec<ApiToolSpec<'a>>>,
}

#[derive(Serialize)]
struct ApiOutMessage<'a> {
    role: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    content: Option<&'a str>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    tool_calls: Vec<ApiOutToolCall<'a>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_call_id: Option<&'a str>,
}

#[derive(Serialize)]
struct ApiOutToolCall<'a> {
    id: &'a str,
    #[serde(rename = "type")]
    type_: &'static str,
    function: ApiOutFunction<'a>,
}

#[derive(Serialize)]
struct ApiOutFunction<'a> {
    name: &'a str,
    arguments: String,
}

#[derive(Serialize)]
struct ApiToolSpec<'a> {
    #[serde(rename = "type")]
    type_: &'static str,
    function: ApiToolFunction<'a>,
}

#[derive(Serialize)]
struct ApiToolFunction<'a> {
    name: &'a str,
    description: &'a str,
    parameters: &'a serde_json::Value,
}

#[derive(Deserialize)]
struct ApiResponse {
    choices: Vec<ApiChoice>,
    usage: ApiUsage,
}

#[derive(Deserialize)]
struct ApiChoice {
    message: ApiMessage,
}

#[derive(Deserialize)]
struct ApiMessage {
    #[serde(default)]
    content: Option<String>,
    tool_calls: Option<Vec<ApiToolCall>>,
}

#[derive(Deserialize)]
struct ApiToolCall {
    id: String,
    function: ApiToolCallFunction,
}

#[derive(Deserialize)]
struct ApiToolCallFunction {
    name: String,
    arguments: String,
}

#[derive(Deserialize)]
struct ApiUsage {
    prompt_tokens: u64,
    completion_tokens: u64,
}

impl OpenRouterClient {
    /// Creates a new OpenRouter client.
    ///
    /// Precomputes the chat endpoint URL and builds a `reqwest::Client`
    /// with default auth headers. The client is reused across all calls.
    pub fn new(base_url: String, api_key: String, model: String) -> Self {
        let mut headers = HeaderMap::new();
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
        headers.insert(
            AUTHORIZATION,
            HeaderValue::from_str(&format!("Bearer {api_key}"))
                .expect("invalid api key characters"),
        );
        headers.insert(
            "HTTP-Referer",
            HeaderValue::from_static("https://github.com/yuann3/mra"),
        );
        headers.insert("X-Title", HeaderValue::from_static("mra"));

        let http = reqwest::Client::builder()
            .default_headers(headers)
            .build()
            .expect("failed to build http client");

        let chat_url = format!("{base_url}/chat/completions");

        Self {
            http,
            chat_url,
            model,
            default_temperature: None,
        }
    }

    /// Returns a two-stage builder for constructing an [`OpenRouterClient`].
    ///
    /// The first stage requires an API key; subsequent fields are optional
    /// with sensible defaults.
    pub fn builder() -> OpenRouterClientBuilderInit {
        OpenRouterClientBuilderInit
    }

    /// Returns the configured default model name.
    pub fn model(&self) -> &str {
        &self.model
    }

    /// Returns the configured default temperature, if any.
    pub fn default_temperature(&self) -> Option<f32> {
        self.default_temperature
    }
}

/// Initial stage of [`OpenRouterClient`] builder — requires an API key.
pub struct OpenRouterClientBuilderInit;

impl OpenRouterClientBuilderInit {
    /// Sets the API key and advances to the main builder stage.
    pub fn api_key(self, key: impl Into<String>) -> OpenRouterClientBuilder {
        OpenRouterClientBuilder {
            api_key: key.into(),
            base_url: "https://openrouter.ai/api/v1".to_string(),
            default_model: "anthropic/claude-sonnet-4".to_string(),
            default_temperature: None,
        }
    }
}

/// Main builder stage for [`OpenRouterClient`].
pub struct OpenRouterClientBuilder {
    api_key: String,
    base_url: String,
    default_model: String,
    default_temperature: Option<f32>,
}

impl OpenRouterClientBuilder {
    /// Overrides the default model (default: `anthropic/claude-sonnet-4`).
    pub fn default_model(mut self, model: impl Into<String>) -> Self {
        self.default_model = model.into();
        self
    }

    /// Sets the default sampling temperature applied when [`LlmRequest::temperature`]
    /// is `None`.
    pub fn default_temperature(mut self, temp: f32) -> Self {
        self.default_temperature = Some(temp);
        self
    }

    /// Overrides the base URL (default: `https://openrouter.ai/api/v1`).
    pub fn base_url(mut self, url: impl Into<String>) -> Self {
        self.base_url = url.into();
        self
    }

    /// Consumes the builder and produces an [`OpenRouterClient`].
    pub fn build(self) -> super::OpenRouterClient {
        let mut client =
            super::OpenRouterClient::new(self.base_url, self.api_key, self.default_model);
        client.default_temperature = self.default_temperature;
        client
    }
}

impl LlmProvider for OpenRouterClient {
    fn chat<'a>(
        &'a self,
        request: &'a LlmRequest,
    ) -> Pin<Box<dyn Future<Output = Result<LlmResponse, LlmError>> + Send + 'a>> {
        Box::pin(async move {
            let model = request.model.as_deref().unwrap_or(&self.model);

            let messages: Vec<ApiOutMessage<'_>> = request
                .messages
                .iter()
                .map(|m| {
                    let role = match m.role {
                        super::Role::System => "system",
                        super::Role::User => "user",
                        super::Role::Assistant => "assistant",
                        super::Role::Tool => "tool",
                    };
                    let content = if m.content.is_empty() && !m.tool_calls.is_empty() {
                        None
                    } else {
                        Some(m.content.as_str())
                    };
                    ApiOutMessage {
                        role,
                        content,
                        tool_calls: m
                            .tool_calls
                            .iter()
                            .map(|tc| ApiOutToolCall {
                                id: &tc.id,
                                type_: "function",
                                function: ApiOutFunction {
                                    name: &tc.name,
                                    arguments: tc.arguments.to_string(),
                                },
                            })
                            .collect(),
                        tool_call_id: m.tool_call_id.as_deref(),
                    }
                })
                .collect();

            let tools = request.tools.as_ref().map(|specs| {
                specs
                    .iter()
                    .map(|s| ApiToolSpec {
                        type_: "function",
                        function: ApiToolFunction {
                            name: &s.name,
                            description: &s.description,
                            parameters: &s.parameters,
                        },
                    })
                    .collect()
            });

            let temperature = request.temperature.or(self.default_temperature);

            let api_req = ApiRequest {
                model,
                messages,
                temperature,
                max_tokens: request.max_tokens,
                tools,
            };

            let resp = self
                .http
                .post(&self.chat_url)
                .json(&api_req)
                .send()
                .await
                .map_err(|e| LlmError::ApiError {
                    status: 0,
                    message: e.to_string(),
                })?;

            let status = resp.status().as_u16();
            if !resp.status().is_success() {
                let body = resp.text().await.unwrap_or_default();
                if status == 429 {
                    return Err(LlmError::RateLimit);
                }
                return Err(LlmError::ApiError {
                    status,
                    message: body,
                });
            }

            let api_resp: ApiResponse = resp
                .json()
                .await
                .map_err(|e| LlmError::InvalidResponse(e.to_string()))?;

            let choice = api_resp
                .choices
                .into_iter()
                .next()
                .ok_or_else(|| LlmError::InvalidResponse("no choices returned".into()))?;

            let ApiMessage {
                content,
                tool_calls,
                ..
            } = choice.message;

            let tool_calls = tool_calls
                .unwrap_or_default()
                .into_iter()
                .map(|tc| {
                    let arguments = serde_json::from_str(&tc.function.arguments).map_err(|e| {
                        LlmError::InvalidResponse(format!(
                            "invalid tool call arguments for {}: {}",
                            tc.function.name, e
                        ))
                    })?;
                    Ok(super::ToolCall {
                        id: tc.id,
                        name: tc.function.name,
                        arguments,
                    })
                })
                .collect::<Result<Vec<_>, LlmError>>()?;

            let content = content.unwrap_or_default();

            if content.is_empty() && tool_calls.is_empty() {
                return Err(LlmError::InvalidResponse(
                    "assistant message had neither content nor tool_calls".into(),
                ));
            }

            Ok(LlmResponse {
                content,
                prompt_tokens: api_resp.usage.prompt_tokens,
                completion_tokens: api_resp.usage.completion_tokens,
                tool_calls,
            })
        })
    }
}
