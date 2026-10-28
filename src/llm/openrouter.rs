use std::fmt::Display;
use std::time::Duration;

use futures::{Stream, StreamExt};
use serde::Deserialize;
use tokio::sync::mpsc::UnboundedSender;

use super::send_with_retry;
use super::types::{
    ChatRequest, FunctionCall, Message, StreamChunk, ToolCall, parse_tool_arguments,
};
use crate::error::AppError;

/// Total timeout for the lightweight health-check request.
const HEALTH_CHECK_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Debug, Deserialize)]
struct OpenRouterResponse {
    #[serde(default)]
    choices: Vec<Choice>,
    /// OpenRouter can answer HTTP 200 with `{"error": {...}}` for upstream failures.
    error: Option<serde_json::Value>,
}

/// Human-readable text of an OpenRouter `error` payload (`{"message": ..., "code": ...}`).
fn error_text(error: &serde_json::Value) -> String {
    error
        .get("message")
        .and_then(|m| m.as_str())
        .map_or_else(|| error.to_string(), str::to_string)
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

/// OpenRouter (OpenAI format) requires `tool_calls[].function.arguments` to be a JSON *string*,
/// not an object. Our internal `FunctionCall` stores it as a `Value`, so we fix it up here.
fn to_openrouter_body(request: &ChatRequest) -> Result<serde_json::Value, AppError> {
    let mut body = serde_json::to_value(request)?;
    if let Some(messages) = body.get_mut("messages").and_then(|m| m.as_array_mut()) {
        for msg in messages.iter_mut() {
            if let Some(tool_calls) = msg.get_mut("tool_calls").and_then(|t| t.as_array_mut()) {
                for tc in tool_calls.iter_mut() {
                    if let Some(args) = tc.get_mut("function").and_then(|f| f.get_mut("arguments"))
                        && !args.is_string()
                    {
                        *args = serde_json::Value::String(args.to_string());
                    }
                }
            }
        }
    }
    Ok(body)
}

pub async fn chat(
    client: &reqwest::Client,
    request: &ChatRequest,
    base_url: &str,
    api_key: &str,
) -> Result<Message, AppError> {
    let url = format!("{base_url}/chat/completions");
    let body = to_openrouter_body(request)?;

    let resp = send_with_retry("OpenRouter", || {
        client
            .post(&url)
            .header("Authorization", format!("Bearer {api_key}"))
            .header("HTTP-Referer", "https://foxmayn.com")
            .header("X-Title", "Foxmayn CoWork")
            .json(&body)
    })
    .await?;

    let parsed: OpenRouterResponse = resp.json().await?;
    if let Some(error) = &parsed.error {
        return Err(AppError::LlmError(format!(
            "OpenRouter error: {}",
            error_text(error)
        )));
    }

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
    #[serde(default)]
    choices: Vec<SseChoice>,
    /// Mid-stream failure reported as an SSE event instead of an HTTP status.
    error: Option<serde_json::Value>,
}

#[derive(Debug, Deserialize)]
struct SseChoice {
    delta: SseDelta,
}

#[derive(Debug, Deserialize)]
struct SseDelta {
    content: Option<String>,
    /// OpenRouter forwards reasoning text from providers that expose it (e.g. OpenAI o-series,
    /// Anthropic extended thinking, Gemini thinking). Surfaced as `StreamChunk::ThinkingDelta`.
    reasoning: Option<String>,
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

/// Handle one complete SSE line. Returns `true` when the stream is finished (terminator,
/// error, or the receiver is gone) and the caller should stop reading.
fn handle_sse_line(line: &str, tx: &UnboundedSender<StreamChunk>) -> bool {
    let line = line.trim();
    // Blank separators and `: comment` keep-alives carry no data.
    let Some(data) = line.strip_prefix("data:") else {
        return false;
    };
    let data = data.trim();
    if data == "[DONE]" {
        let _ = tx.send(StreamChunk::Done);
        return true;
    }
    let Ok(resp) = serde_json::from_str::<SseResponse>(data) else {
        return false;
    };
    if let Some(error) = &resp.error {
        let _ = tx.send(StreamChunk::Error(format!(
            "OpenRouter stream error: {}",
            error_text(error)
        )));
        return true;
    }
    let Some(choice) = resp.choices.into_iter().next() else {
        return false;
    };
    let delta = choice.delta;
    if let Some(reasoning) = delta.reasoning
        && !reasoning.is_empty()
        && tx.send(StreamChunk::ThinkingDelta(reasoning)).is_err()
    {
        return true;
    }
    if let Some(content) = delta.content
        && !content.is_empty()
        && tx.send(StreamChunk::ContentDelta(content)).is_err()
    {
        return true;
    }
    if let Some(tool_calls) = delta.tool_calls {
        for tc in tool_calls {
            let sent = tx.send(StreamChunk::ToolCallDelta {
                index: tc.index,
                id: tc.id,
                name: tc.function.as_ref().and_then(|f| f.name.clone()),
                arguments_fragment: tc.function.and_then(|f| f.arguments).unwrap_or_default(),
            });
            if sent.is_err() {
                return true;
            }
        }
    }
    false
}

/// Read an SSE byte stream and forward chunks. Bytes are buffered and decoded only per
/// complete line, so a multibyte character split across network chunks stays intact. The
/// stream must end with `[DONE]`; EOF before that is reported as an error (truncated reply).
async fn process_sse_stream<S, B, E>(mut stream: S, tx: UnboundedSender<StreamChunk>)
where
    S: Stream<Item = Result<B, E>> + Unpin,
    B: AsRef<[u8]>,
    E: Display,
{
    let mut buffer: Vec<u8> = Vec::new();

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
            if handle_sse_line(&String::from_utf8_lossy(&line), &tx) {
                return;
            }
        }
    }

    // A final line without a trailing newline.
    if !buffer.is_empty() && handle_sse_line(&String::from_utf8_lossy(&buffer), &tx) {
        return;
    }
    let _ = tx.send(StreamChunk::Error(
        "OpenRouter stream ended before [DONE]; the reply may be truncated.".into(),
    ));
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
    let body = to_openrouter_body(request)?;

    let resp = send_with_retry("OpenRouter", || {
        client
            .post(&url)
            .header("Authorization", format!("Bearer {api_key}"))
            .header("HTTP-Referer", "https://foxmayn.com")
            .header("X-Title", "Foxmayn CoWork")
            .json(&body)
    })
    .await?;

    tokio::spawn(process_sse_stream(resp.bytes_stream(), tx));
    Ok(())
}

pub async fn health_check(client: &reqwest::Client, base_url: &str, api_key: &str) -> bool {
    client
        .get(format!("{base_url}/models"))
        .header("Authorization", format!("Bearer {api_key}"))
        .timeout(HEALTH_CHECK_TIMEOUT)
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
                    let arguments = parse_tool_arguments(&tc.function.arguments).map_err(|e| {
                        AppError::LlmError(format!(
                            "Failed to parse tool arguments for '{}': {e}",
                            tc.function.name
                        ))
                    })?;
                    Ok(ToolCall {
                        id: Some(tc.id),
                        r#type: "function".to_string(),
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

#[cfg(test)]
mod tests {
    use super::*;
    use futures::stream;
    use tokio::sync::mpsc;

    type Chunk = Result<Vec<u8>, String>;

    async fn run(chunks: Vec<Chunk>) -> Vec<StreamChunk> {
        let (tx, mut rx) = mpsc::unbounded_channel();
        process_sse_stream(stream::iter(chunks), tx).await;
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
        let event = "data: {\"choices\":[{\"delta\":{\"content\":\"h\u{e9}llo \u{1f600}\"}}]}\n\ndata: [DONE]\n\n";
        let bytes = event.as_bytes();
        // Split inside the two-byte e-acute and inside the four-byte emoji.
        let e = event.find('\u{e9}').unwrap() + 1;
        let emoji = event.find('\u{1f600}').unwrap() + 2;
        let chunks = vec![
            Ok(bytes[..e].to_vec()),
            Ok(bytes[e..emoji].to_vec()),
            Ok(bytes[emoji..].to_vec()),
        ];
        let out = run(chunks).await;
        assert_eq!(content_of(&out), "h\u{e9}llo \u{1f600}");
        assert!(matches!(out.last(), Some(StreamChunk::Done)));
    }

    #[tokio::test]
    async fn mid_stream_error_event_surfaces_as_error() {
        let out = run(vec![Ok(
            b"data: {\"error\":{\"message\":\"upstream died\",\"code\":502}}\n\n".to_vec(),
        )])
        .await;
        match out.as_slice() {
            [StreamChunk::Error(e)] => assert!(e.contains("upstream died"), "{e}"),
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[tokio::test]
    async fn eof_without_done_is_an_error() {
        let out = run(vec![Ok(
            b"data: {\"choices\":[{\"delta\":{\"content\":\"par\"}}]}\n\n".to_vec(),
        )])
        .await;
        assert_eq!(content_of(&out), "par");
        assert!(matches!(out.last(), Some(StreamChunk::Error(_))));
    }

    #[tokio::test]
    async fn crlf_and_compact_data_prefix_are_accepted() {
        let out = run(vec![Ok(
            b": keep-alive\r\n\r\ndata:{\"choices\":[{\"delta\":{\"content\":\"hi\"}}]}\r\n\r\ndata: [DONE]"
                .to_vec(),
        )])
        .await;
        assert_eq!(content_of(&out), "hi");
        assert!(matches!(out.last(), Some(StreamChunk::Done)));
    }

    #[tokio::test]
    async fn transport_error_is_reported() {
        let out = run(vec![Err("connection reset".to_string())]).await;
        assert!(matches!(out.as_slice(), [StreamChunk::Error(e)] if e.contains("reset")));
    }
}
