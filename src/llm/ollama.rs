use serde::Deserialize;

use super::types::{ChatRequest, FunctionCall, Message, ToolCall};
use crate::error::AppError;

#[derive(Debug, Deserialize)]
struct OllamaResponse {
    message: OllamaMessage,
}

#[derive(Debug, Deserialize)]
struct OllamaMessage {
    role: String,
    content: String,
    tool_calls: Option<Vec<OllamaToolCall>>,
}

#[derive(Debug, Deserialize)]
struct OllamaToolCall {
    function: OllamaFunctionCall,
}

/// Ollama gives arguments as an already-parsed JSON object (not a string)
#[derive(Debug, Deserialize)]
struct OllamaFunctionCall {
    name: String,
    arguments: serde_json::Value,
}

pub async fn chat(
    client: &reqwest::Client,
    request: &ChatRequest,
    base_url: &str,
) -> Result<Message, AppError> {
    let url = format!("{base_url}/api/chat");

    let resp = client.post(&url).json(request).send().await?;

    let status = resp.status();
    if !status.is_success() {
        let body = resp.text().await.unwrap_or_default();
        return Err(AppError::LlmError(format!(
            "Ollama returned {status}: {body}"
        )));
    }

    let parsed: OllamaResponse = resp.json().await?;
    Ok(normalize(parsed.message))
}

pub async fn health_check(client: &reqwest::Client, base_url: &str) -> bool {
    client
        .get(base_url)
        .send()
        .await
        .map(|r| r.status().is_success())
        .unwrap_or(false)
}

fn normalize(msg: OllamaMessage) -> Message {
    let tool_calls = msg.tool_calls.map(|calls| {
        calls
            .into_iter()
            .map(|tc| ToolCall {
                id: None,
                function: FunctionCall {
                    name: tc.function.name,
                    arguments: tc.function.arguments,
                },
            })
            .collect()
    });

    Message {
        role: msg.role,
        content: msg.content,
        tool_calls,
        name: None,
        tool_call_id: None,
    }
}
