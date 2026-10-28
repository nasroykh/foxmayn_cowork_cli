use std::fmt::Display;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use futures::{Stream, StreamExt};
use serde::Deserialize;
use tokio::sync::mpsc::UnboundedSender;

use super::send_with_retry;
use super::types::{ChatRequest, FunctionCall, Message, StreamChunk, ToolCall};
use crate::error::AppError;

/// Total timeout for the lightweight health-check and `/api/show` requests.
const SHORT_REQUEST_TIMEOUT: Duration = Duration::from_secs(5);

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

    let resp = send_with_retry("Ollama", || client.post(&url).json(request)).await?;

    let parsed: OllamaResponse = resp.json().await?;
    Ok(normalize(parsed.message))
}

// ── Streaming ─────────────────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
struct NdjsonLine {
    message: Option<NdjsonMessage>,
    #[serde(default)]
    done: bool,
    /// Mid-stream failure (e.g. out of memory, model crash) reported as `{"error": "..."}`.
    error: Option<serde_json::Value>,
}

#[derive(Debug, Deserialize)]
struct NdjsonMessage {
    content: Option<String>,
    /// Ollama emits reasoning chunks under `thinking` when `think` is enabled on a thinking model.
    thinking: Option<String>,
    tool_calls: Option<Vec<OllamaToolCall>>,
}

/// Per-stream state for the NDJSON parser.
struct NdjsonState {
    // Ollama emits *complete* tool calls (arguments is a full JSON object per line, never a
    // fragment), but different models emit them on different lines: some on `done:false`
    // only (e.g. qwen3 thinking), some on `done:true` only, and some on both. Rather than
    // dedupe, keep only the most-recently-seen non-empty tool_calls array and emit it once
    // when the stream ends. This is correct for every observed Ollama behaviour and
    // preserves duplicate calls within a single batch (parallel tool calls).
    latest_tool_calls: Option<Vec<OllamaToolCall>>,
}

fn flush_tool_calls(calls: Vec<OllamaToolCall>, tx: &UnboundedSender<StreamChunk>) {
    for (idx, tc) in calls.into_iter().enumerate() {
        let id = tc.id.unwrap_or_else(generate_tool_call_id);
        let _ = tx.send(StreamChunk::ToolCallDelta {
            index: idx,
            id: Some(id),
            name: Some(tc.function.name),
            arguments_fragment: tc.function.arguments.to_string(),
        });
    }
}

/// Handle one complete NDJSON line. Returns `true` when the stream is finished (`done`,
/// error, or the receiver is gone) and the caller should stop reading.
fn handle_ndjson_line(
    line: &str,
    state: &mut NdjsonState,
    tx: &UnboundedSender<StreamChunk>,
) -> bool {
    let line = line.trim();
    if line.is_empty() {
        return false;
    }
    let Ok(parsed) = serde_json::from_str::<NdjsonLine>(line) else {
        return false;
    };

    if let Some(error) = &parsed.error {
        let text = error
            .as_str()
            .map_or_else(|| error.to_string(), str::to_string);
        let _ = tx.send(StreamChunk::Error(format!("Ollama stream error: {text}")));
        return true;
    }

    if let Some(msg) = &parsed.message {
        if let Some(thinking) = &msg.thinking
            && !thinking.is_empty()
            && tx
                .send(StreamChunk::ThinkingDelta(thinking.clone()))
                .is_err()
        {
            return true;
        }
        if let Some(content) = &msg.content
            && !content.is_empty()
            && tx.send(StreamChunk::ContentDelta(content.clone())).is_err()
        {
            return true;
        }
        if let Some(tool_calls) = &msg.tool_calls
            && !tool_calls.is_empty()
        {
            state.latest_tool_calls = Some(tool_calls.clone());
        }
    }

    if parsed.done {
        if let Some(calls) = state.latest_tool_calls.take() {
            flush_tool_calls(calls, tx);
        }
        let _ = tx.send(StreamChunk::Done);
        return true;
    }
    false
}

/// Read an NDJSON byte stream and forward chunks. Bytes are buffered and decoded only per
/// complete line, so a multibyte character split across network chunks stays intact. The
/// stream must end with `done:true`; EOF before that is reported as an error.
async fn process_ndjson_stream<S, B, E>(mut stream: S, tx: UnboundedSender<StreamChunk>)
where
    S: Stream<Item = Result<B, E>> + Unpin,
    B: AsRef<[u8]>,
    E: Display,
{
    let mut buffer: Vec<u8> = Vec::new();
    let mut state = NdjsonState {
        latest_tool_calls: None,
    };

    loop {
        match stream.next().await {
            None => break,
            Some(Err(e)) => {
                let _ = tx.send(StreamChunk::Error(e.to_string()));
                return;
            }
            Some(Ok(bytes)) => buffer.extend_from_slice(bytes.as_ref()),
        }

        while let Some(pos) = buffer.iter().position(|&b| b == b'\n') {
            let line: Vec<u8> = buffer.drain(..=pos).collect();
            if handle_ndjson_line(&String::from_utf8_lossy(&line), &mut state, &tx) {
                return;
            }
        }
    }

    // A final line without a trailing newline.
    if !buffer.is_empty() && handle_ndjson_line(&String::from_utf8_lossy(&buffer), &mut state, &tx)
    {
        return;
    }
    let _ = tx.send(StreamChunk::Error(
        "Ollama stream ended before the response completed; the reply may be truncated.".into(),
    ));
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

    let resp = send_with_retry("Ollama", || client.post(&url).json(request)).await?;

    tokio::spawn(process_ndjson_stream(resp.bytes_stream(), tx));
    Ok(())
}

/// Query `/api/show` and return the model's declared `capabilities`, or `None` when the field
/// is missing (older Ollama versions that predate it). Network/HTTP errors propagate as `Err`.
async fn model_capabilities(
    client: &reqwest::Client,
    base_url: &str,
    model: &str,
) -> Result<Option<Vec<String>>, AppError> {
    let url = format!("{base_url}/api/show");
    let resp = client
        .post(&url)
        .json(&serde_json::json!({ "model": model }))
        .timeout(SHORT_REQUEST_TIMEOUT)
        .send()
        .await?;

    let status = resp.status();
    if !status.is_success() {
        let body = resp.text().await.unwrap_or_default();
        return Err(AppError::LlmError(format!(
            "Ollama /api/show returned {status}: {}",
            super::trim_error_body(&body)
        )));
    }

    let parsed: serde_json::Value = resp.json().await?;
    Ok(parsed
        .get("capabilities")
        .and_then(|v| v.as_array())
        .map(|caps| {
            caps.iter()
                .filter_map(|c| c.as_str().map(str::to_string))
                .collect()
        }))
}

/// Returns `Ok(true)` if `tools` appears in the model's `capabilities`, `Ok(false)` if it
/// explicitly does not, and `Ok(true)` when the field is missing (older Ollama versions that
/// predate `capabilities` — assume supported rather than blocking the user). Network/HTTP
/// errors propagate as `Err`.
pub async fn model_supports_tools(
    client: &reqwest::Client,
    base_url: &str,
    model: &str,
) -> Result<bool, AppError> {
    Ok(model_capabilities(client, base_url, model)
        .await?
        .is_none_or(|caps| caps.iter().any(|c| c == "tools")))
}

/// Returns `Ok(true)` only when `thinking` appears in the model's `capabilities`. Callers
/// should drop the `think` request field when this is `false` (non-thinking models answer
/// HTTP 400 to any `think` value). A missing `capabilities` field counts as unsupported.
pub async fn model_supports_thinking(
    client: &reqwest::Client,
    base_url: &str,
    model: &str,
) -> Result<bool, AppError> {
    Ok(model_capabilities(client, base_url, model)
        .await?
        .is_some_and(|caps| caps.iter().any(|c| c == "thinking")))
}

pub async fn health_check(client: &reqwest::Client, base_url: &str) -> bool {
    client
        .get(base_url)
        .timeout(SHORT_REQUEST_TIMEOUT)
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
                id: Some(tc.id.unwrap_or_else(generate_tool_call_id)),
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

#[cfg(test)]
mod tests {
    use super::*;
    use futures::stream;
    use tokio::sync::mpsc;

    type Chunk = Result<Vec<u8>, String>;

    async fn run(chunks: Vec<Chunk>) -> Vec<StreamChunk> {
        let (tx, mut rx) = mpsc::unbounded_channel();
        process_ndjson_stream(stream::iter(chunks), tx).await;
        let mut out = Vec::new();
        while let Ok(c) = rx.try_recv() {
            out.push(c);
        }
        out
    }

    fn content_of(chunks: &[StreamChunk]) -> String {
        chunks
            .iter()
            .filter_map(|c| match c {
                StreamChunk::ContentDelta(t) => Some(t.as_str()),
                _ => None,
            })
            .collect()
    }

    #[tokio::test]
    async fn multibyte_char_split_across_chunks_is_preserved() {
        let body = "{\"message\":{\"content\":\"h\u{e9}llo \u{1f600}\"},\"done\":false}\n{\"done\":true}\n";
        let bytes = body.as_bytes();
        let e = body.find('\u{e9}').unwrap() + 1;
        let emoji = body.find('\u{1f600}').unwrap() + 3;
        let out = run(vec![
            Ok(bytes[..e].to_vec()),
            Ok(bytes[e..emoji].to_vec()),
            Ok(bytes[emoji..].to_vec()),
        ])
        .await;
        assert_eq!(content_of(&out), "h\u{e9}llo \u{1f600}");
        assert!(matches!(out.last(), Some(StreamChunk::Done)));
    }

    #[tokio::test]
    async fn error_line_surfaces_as_error() {
        let out = run(vec![Ok(
            b"{\"message\":{\"content\":\"x\"},\"done\":false}\n{\"error\":\"model runner crashed\"}\n"
                .to_vec(),
        )])
        .await;
        match out.last() {
            Some(StreamChunk::Error(e)) => assert!(e.contains("model runner crashed"), "{e}"),
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[tokio::test]
    async fn eof_without_done_is_an_error() {
        let out = run(vec![Ok(
            b"{\"message\":{\"content\":\"par\"},\"done\":false}\n".to_vec(),
        )])
        .await;
        assert_eq!(content_of(&out), "par");
        assert!(matches!(out.last(), Some(StreamChunk::Error(_))));
    }

    #[tokio::test]
    async fn tool_calls_are_emitted_once_at_done_with_provider_ids() {
        let line = "{\"message\":{\"content\":\"\",\"tool_calls\":[{\"id\":\"call_1\",\"function\":{\"name\":\"list_files\",\"arguments\":{\"path\":\".\"}}}]},\"done\":false}\n";
        let out = run(vec![
            Ok(line.as_bytes().to_vec()),
            Ok(b"{\"done\":true}\n".to_vec()),
        ])
        .await;
        let calls: Vec<_> = out
            .iter()
            .filter(|c| matches!(c, StreamChunk::ToolCallDelta { .. }))
            .collect();
        assert_eq!(calls.len(), 1);
        assert!(matches!(
            calls[0],
            StreamChunk::ToolCallDelta { id: Some(id), .. } if id == "call_1"
        ));
        assert!(matches!(out.last(), Some(StreamChunk::Done)));
    }
}
