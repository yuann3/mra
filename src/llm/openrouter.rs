//! OpenRouter (OpenAI-compatible) LLM client.
//!
//! Serialization borrows request data — no cloning of messages or model
//! strings per call. The chat endpoint URL is precomputed at construction.

use std::future::Future;
use std::pin::Pin;

use reqwest::header::{AUTHORIZATION, CONTENT_TYPE, HeaderMap, HeaderValue};
use serde::{Deserialize, Serialize};

use super::{ChatMessage, LlmProvider, LlmRequest, LlmResponse};
use crate::error::LlmError;

/// Client for OpenRouter's OpenAI-compatible chat completions API.
///
/// Holds a shared `reqwest::Client` (connection pool) and precomputed
/// endpoint URL. Cheap to clone via `Arc` internally.
pub struct OpenRouterClient {
    http: reqwest::Client,
    chat_url: String,
    model: String,
}

/// Borrows request data for serialization — avoids cloning messages.
#[derive(Serialize)]
struct ApiRequest<'a> {
    model: &'a str,
    messages: &'a [ChatMessage],
    #[serde(skip_serializing_if = "Option::is_none")]
    temperature: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    max_tokens: Option<u32>,
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
    content: String,
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
        }
    }

    /// Returns the configured default model name.
    pub fn model(&self) -> &str {
        &self.model
    }
}

impl LlmProvider for OpenRouterClient {
    fn chat<'a>(
        &'a self,
        request: &'a LlmRequest,
    ) -> Pin<Box<dyn Future<Output = Result<LlmResponse, LlmError>> + Send + 'a>> {
        Box::pin(async move {
            let model = request.model.as_deref().unwrap_or(&self.model);

            let api_req = ApiRequest {
                model,
                messages: &request.messages,
                temperature: request.temperature,
                max_tokens: request.max_tokens,
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

            Ok(LlmResponse {
                content: choice.message.content,
                prompt_tokens: api_resp.usage.prompt_tokens,
                completion_tokens: api_resp.usage.completion_tokens,
            })
        })
    }
}
