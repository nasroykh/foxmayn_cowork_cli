use serde::Deserialize;

use super::types::{ChatRequest, FunctionCall, Message, ToolCall};
use crate::error::AppError;

#[derive(Debug, Deserialize)]
struct OpenRouterResponse {
    choices: Vec<Choice>,
}

#[derive(Debug, Deserialize)]
struct Choice {
    message: OpenRouterMessage,
}

#[derive(Debug, Deserialize)]
struct OpenRouterMessage {
    role: String,
    content: Option<String>,
    tool_calls: Option<Vec<OpenRouterToolCall>>,
}

#[derive(Debug, Deserialize)]
struct OpenRouterToolCall {
    id: String,
    function: OpenRouterFunctionCall,
}

/// OpenRouter follows the OpenAI format: arguments is a JSON *string*, not an object
#[derive(Debug, Deserialize)]
struct OpenRouterFunctionCall {
    name: String,
    arguments: String,
}

pub async fn chat(
    client: &reqwest::Client,
    request: &ChatRequest,
    base_url: &str,
    api_key: &str,
) -> Result<Message, AppError> {
    let url = format!("{base_url}/chat/completions");

    let resp = client
        .post(&url)
        .header("Authorization", format!("Bearer {api_key}"))
        .header("HTTP-Referer", "https://foxmayn.com")
        .header("X-Title", "Foxmayn CoWork")
        .json(request)
        .send()
        .await?;

    let status = resp.status();
    if !status.is_success() {
        let body = resp.text().await.unwrap_or_default();
        return Err(AppError::LlmError(format!(
            "OpenRouter returned {status}: {body}"
        )));
    }

    let parsed: OpenRouterResponse = resp.json().await?;

    parsed
        .choices
        .into_iter()
        .next()
        .ok_or_else(|| AppError::LlmError("OpenRouter returned empty choices".into()))
        .and_then(|choice| normalize(choice.message))
}

pub async fn health_check(client: &reqwest::Client, base_url: &str, api_key: &str) -> bool {
    client
        .get(format!("{base_url}/models"))
        .header("Authorization", format!("Bearer {api_key}"))
        .send()
        .await
        .map(|r| r.status().is_success())
        .unwrap_or(false)
}

fn normalize(msg: OpenRouterMessage) -> Result<Message, AppError> {
    let tool_calls = msg
        .tool_calls
        .map(|calls| {
            calls
                .into_iter()
                .map(|tc| {
                    let arguments: serde_json::Value = serde_json::from_str(&tc.function.arguments)
                        .map_err(|e| {
                            AppError::LlmError(format!(
                                "Failed to parse tool arguments for '{}': {e}",
                                tc.function.name
                            ))
                        })?;
                    Ok(ToolCall {
                        id: Some(tc.id),
                        function: FunctionCall {
                            name: tc.function.name,
                            arguments,
                        },
                    })
                })
                .collect::<Result<Vec<_>, AppError>>()
        })
        .transpose()?;

    Ok(Message {
        role: msg.role,
        content: msg.content.unwrap_or_default(),
        tool_calls,
        name: None,
        tool_call_id: None,
    })
}
