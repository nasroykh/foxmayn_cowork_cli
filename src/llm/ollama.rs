use std::sync::atomic::{AtomicU64, Ordering};

use futures::StreamExt;
use serde::Deserialize;
use tokio::sync::mpsc::UnboundedSender;

use super::types::{ChatRequest, FunctionCall, Message, StreamChunk, ToolCall};
use crate::error::AppError;

/// Ollama does not return tool-call IDs, but downstream code (OpenRouter replay, history
/// serialization) needs a stable identifier. Generate a unique one per call here.
static TOOL_CALL_COUNTER: AtomicU64 = AtomicU64::new(0);

fn generate_tool_call_id() -> String {
    let n = TOOL_CALL_COUNTER.fetch_add(1, Ordering::Relaxed);
    format!("ollama_call_{n:08x}")
}

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

#[derive(Debug, Clone, Deserialize)]
struct OllamaToolCall {
    /// Newer Ollama versions emit a stable tool-call id (e.g. `"call_abc123"`). When
    /// present we preserve it end-to-end; when absent we fall back to a locally
    /// generated id at emission time.
    #[serde(default)]
    id: Option<String>,
    function: OllamaFunctionCall,
}

/// Ollama gives arguments as an already-parsed JSON object (not a string)
#[derive(Debug, Clone, Deserialize)]
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
    // Ollama emits *complete* tool calls (arguments is a full JSON object per line, never a
    // fragment), but different models emit them on different lines: some on `done:false`
    // only (e.g. qwen3 thinking), some on `done:true` only, and some on both. Rather than
    // dedupe, keep only the most-recently-seen non-empty tool_calls array and emit it once
    // when the stream ends. This is correct for every observed Ollama behaviour and
    // preserves duplicate calls within a single batch (parallel tool calls).
    let mut latest_tool_calls: Option<Vec<OllamaToolCall>> = None;

    let flush_tool_calls = |calls: Vec<OllamaToolCall>, tx: &UnboundedSender<StreamChunk>| {
        for (idx, tc) in calls.into_iter().enumerate() {
            let id = tc.id.unwrap_or_else(generate_tool_call_id);
            let _ = tx.send(StreamChunk::ToolCallDelta {
                index: idx,
                id: Some(id),
                name: Some(tc.function.name),
                arguments_fragment: tc.function.arguments.to_string(),
            });
        }
    };

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
                if let Some(tool_calls) = &msg.tool_calls
                    && !tool_calls.is_empty()
                {
                    latest_tool_calls = Some(tool_calls.clone());
                }
            }

            if parsed.done {
                if let Some(calls) = latest_tool_calls.take() {
                    flush_tool_calls(calls, &tx);
                }
                let _ = tx.send(StreamChunk::Done);
                return;
            }
        }
    }

    if let Some(calls) = latest_tool_calls.take() {
        flush_tool_calls(calls, &tx);
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

/// Query `/api/show` for the model's declared capabilities. Returns `Ok(true)` if `tools`
/// appears in the `capabilities` array, `Ok(false)` if it explicitly does not, and `Ok(true)`
/// when the field is missing (older Ollama versions that predate `capabilities` — assume
/// supported rather than blocking the user). Network/HTTP errors propagate as `Err`.
pub async fn model_supports_tools(
    client: &reqwest::Client,
    base_url: &str,
    model: &str,
) -> Result<bool, AppError> {
    let url = format!("{base_url}/api/show");
    let resp = client
        .post(&url)
        .json(&serde_json::json!({ "model": model }))
        .send()
        .await?;

    let status = resp.status();
    if !status.is_success() {
        let body = resp.text().await.unwrap_or_default();
        return Err(AppError::LlmError(format!(
            "Ollama /api/show returned {status}: {body}"
        )));
    }

    let parsed: serde_json::Value = resp.json().await?;
    match parsed.get("capabilities").and_then(|v| v.as_array()) {
        Some(caps) => Ok(caps.iter().any(|c| c.as_str() == Some("tools"))),
        None => Ok(true),
    }
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
                id: Some(generate_tool_call_id()),
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
