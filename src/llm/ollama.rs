use futures::StreamExt;
use serde::Deserialize;
use tokio::sync::mpsc::UnboundedSender;

use super::types::{ChatRequest, FunctionCall, Message, StreamChunk, ToolCall};
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

// ── Streaming ─────────────────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
struct NdjsonLine {
    message: Option<NdjsonMessage>,
    done: bool,
}

#[derive(Debug, Deserialize)]
struct NdjsonMessage {
    content: Option<String>,
    /// Ollama emits reasoning chunks under `thinking` when `think` is enabled on a thinking model.
    thinking: Option<String>,
    tool_calls: Option<Vec<OllamaToolCall>>,
}

async fn process_ndjson_stream(response: reqwest::Response, tx: UnboundedSender<StreamChunk>) {
    let mut stream = response.bytes_stream();
    let mut buffer = String::new();

    loop {
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

        while let Some(pos) = buffer.find('\n') {
            let line = buffer[..pos].trim().to_string();
            buffer.drain(..pos + 1);
            if line.is_empty() {
                continue;
            }
            let Ok(parsed) = serde_json::from_str::<NdjsonLine>(&line) else {
                continue;
            };

            if let Some(msg) = &parsed.message {
                if let Some(thinking) = &msg.thinking
                    && !thinking.is_empty()
                {
                    let _ = tx.send(StreamChunk::ThinkingDelta(thinking.clone()));
                }
                if let Some(content) = &msg.content
                    && !content.is_empty()
                {
                    let _ = tx.send(StreamChunk::ContentDelta(content.clone()));
                }
                // Tool calls may arrive on a done:false line (e.g. qwen3 thinking models)
                // or on the done:true line (other models). Handle both.
                if let Some(tool_calls) = &msg.tool_calls {
                    for (idx, tc) in tool_calls.iter().enumerate() {
                        let _ = tx.send(StreamChunk::ToolCallDelta {
                            index: idx,
                            id: None,
                            name: Some(tc.function.name.clone()),
                            arguments_fragment: tc.function.arguments.to_string(),
                        });
                    }
                }
            }

            if parsed.done {
                let _ = tx.send(StreamChunk::Done);
                return;
            }
        }
    }

    let _ = tx.send(StreamChunk::Done);
}

/// Make a streaming chat request. Spawns NDJSON parsing in a background task;
/// chunks arrive via `tx`. Returns after the HTTP response headers are received.
pub async fn chat_stream(
    client: &reqwest::Client,
    request: &ChatRequest,
    base_url: &str,
    tx: UnboundedSender<StreamChunk>,
) -> Result<(), AppError> {
    let url = format!("{base_url}/api/chat");

    let resp = client.post(&url).json(request).send().await?;

    let status = resp.status();
    if !status.is_success() {
        let body = resp.text().await.unwrap_or_default();
        return Err(AppError::LlmError(format!(
            "Ollama returned {status}: {body}"
        )));
    }

    tokio::spawn(process_ndjson_stream(resp, tx));
    Ok(())
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
                r#type: "function".to_string(),
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
