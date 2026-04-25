use futures::StreamExt;
use serde::Deserialize;
use tokio::sync::mpsc::UnboundedSender;

use super::types::{ChatRequest, FunctionCall, Message, StreamChunk, ToolCall};
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

// ── Streaming structs ─────────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
struct SseResponse {
    choices: Vec<SseChoice>,
}

#[derive(Debug, Deserialize)]
struct SseChoice {
    delta: SseDelta,
}

#[derive(Debug, Deserialize)]
struct SseDelta {
    content: Option<String>,
    tool_calls: Option<Vec<SseToolCallDelta>>,
}

#[derive(Debug, Deserialize)]
struct SseToolCallDelta {
    index: usize,
    id: Option<String>,
    function: Option<SseFunctionDelta>,
}

#[derive(Debug, Deserialize)]
struct SseFunctionDelta {
    name: Option<String>,
    arguments: Option<String>,
}

async fn process_sse_stream(response: reqwest::Response, tx: UnboundedSender<StreamChunk>) {
    let mut stream = response.bytes_stream();
    let mut buffer = String::new();

    'outer: loop {
        match stream.next().await {
            None => break,
            Some(Err(e)) => {
                let _ = tx.send(StreamChunk::Error(e.to_string()));
                return;
            }
            Some(Ok(bytes)) => {
                buffer.push_str(&String::from_utf8_lossy(&bytes));
            }
        }

        while let Some(pos) = buffer.find("\n\n") {
            let message = buffer[..pos].to_string();
            buffer.drain(..pos + 2);

            for line in message.lines() {
                let Some(data) = line.strip_prefix("data: ") else { continue };
                let data = data.trim();
                if data == "[DONE]" {
                    let _ = tx.send(StreamChunk::Done);
                    break 'outer;
                }
                let Ok(resp) = serde_json::from_str::<SseResponse>(data) else { continue };
                let Some(choice) = resp.choices.into_iter().next() else { continue };
                let delta = choice.delta;
                if let Some(content) = delta.content
                    && !content.is_empty()
                {
                    let _ = tx.send(StreamChunk::ContentDelta(content));
                }
                if let Some(tool_calls) = delta.tool_calls {
                    for tc in tool_calls {
                        let _ = tx.send(StreamChunk::ToolCallDelta {
                            index: tc.index,
                            id: tc.id,
                            name: tc.function.as_ref().and_then(|f| f.name.clone()),
                            arguments_fragment: tc
                                .function
                                .and_then(|f| f.arguments)
                                .unwrap_or_default(),
                        });
                    }
                }
            }
        }
    }
}

/// Make a streaming chat request. Spawns SSE parsing in a background task;
/// chunks arrive via `tx`. Returns after the HTTP response headers are received.
pub async fn chat_stream(
    client: &reqwest::Client,
    request: &ChatRequest,
    base_url: &str,
    api_key: &str,
    tx: UnboundedSender<StreamChunk>,
) -> Result<(), AppError> {
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

    tokio::spawn(process_sse_stream(resp, tx));
    Ok(())
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
