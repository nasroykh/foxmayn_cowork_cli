pub mod ollama;
pub mod openrouter;
pub mod runtime;
pub mod tools;
pub mod types;

/// Maximum number of LLM ↔ tool back-and-forth rounds before the loop errors out.
pub const MAX_TOOL_ROUNDS: usize = 10;

#[cfg(feature = "local")]
pub mod local;

#[cfg(feature = "local")]
use std::sync::Arc;

use std::time::Duration;

use tokio::sync::mpsc::UnboundedSender;

use crate::config::{Config, Provider};
use crate::error::AppError;
use runtime::LlmRuntime;
use types::{ChatRequest, Message, StreamChunk};

/// Retries after a transient failure status, before any body has been read.
const MAX_RETRIES: u32 = 2;
/// Upper bound on a single retry wait, including a server-supplied `Retry-After`.
const MAX_RETRY_DELAY: Duration = Duration::from_secs(10);
/// Error bodies longer than this are cut when surfaced to the user.
const MAX_ERROR_BODY_CHARS: usize = 500;

fn is_retryable(status: reqwest::StatusCode) -> bool {
    matches!(status.as_u16(), 429 | 502 | 503 | 504)
}

/// Wait before retry number `attempt` (0-based): `Retry-After` seconds when the server sent
/// one, else 1s, 2s, … Always capped at [`MAX_RETRY_DELAY`].
fn retry_delay(retry_after: Option<&str>, attempt: u32) -> Duration {
    let secs = retry_after
        .and_then(|v| v.trim().parse::<u64>().ok())
        .unwrap_or(1 << attempt);
    Duration::from_secs(secs).min(MAX_RETRY_DELAY)
}

fn trim_error_body(body: &str) -> String {
    let body = body.trim();
    match body.char_indices().nth(MAX_ERROR_BODY_CHARS) {
        Some((byte, _)) => format!("{}…", &body[..byte]),
        None => body.to_string(),
    }
}

/// Send a request, retrying 429/502/503/504 up to [`MAX_RETRIES`] times. A non-success final
/// status becomes an error carrying the status and a trimmed body. `build` is called once per
/// attempt because a `RequestBuilder` is consumed by `send`.
pub(crate) async fn send_with_retry(
    provider: &str,
    build: impl Fn() -> reqwest::RequestBuilder,
) -> Result<reqwest::Response, AppError> {
    let mut attempt = 0;
    loop {
        let resp = build().send().await?;
        let status = resp.status();
        if status.is_success() {
            return Ok(resp);
        }
        if is_retryable(status) && attempt < MAX_RETRIES {
            let retry_after = resp
                .headers()
                .get(reqwest::header::RETRY_AFTER)
                .and_then(|v| v.to_str().ok())
                .map(str::to_owned);
            tokio::time::sleep(retry_delay(retry_after.as_deref(), attempt)).await;
            attempt += 1;
            continue;
        }
        let body = resp.text().await.unwrap_or_default();
        return Err(AppError::LlmError(format!(
            "{provider} returned {status}: {}",
            trim_error_body(&body)
        )));
    }
}

pub async fn chat(
    runtime: &LlmRuntime,
    request: &ChatRequest,
    config: &Config,
) -> Result<Message, AppError> {
    match config.provider {
        Provider::Ollama => {
            ollama::chat(&runtime.http_client, request, &config.ollama_base_url).await
        }
        Provider::OpenRouter => {
            let key = config.openrouter_api_key.as_deref().unwrap_or("");
            openrouter::chat(
                &runtime.http_client,
                request,
                &config.openrouter_base_url,
                key,
            )
            .await
        }
        Provider::Local => {
            #[cfg(feature = "local")]
            {
                let local = runtime
                    .local
                    .as_ref()
                    .ok_or_else(|| AppError::LlmError("Local runtime not initialized".into()))?;
                local::chat(Arc::clone(local), request, config).await
            }
            #[cfg(not(feature = "local"))]
            Err(AppError::LlmError(
                "Built without --features local. Rebuild with `cargo build --features local`."
                    .into(),
            ))
        }
    }
}

pub async fn chat_stream(
    runtime: &LlmRuntime,
    request: &ChatRequest,
    config: &Config,
    tx: UnboundedSender<StreamChunk>,
) -> Result<(), AppError> {
    match config.provider {
        Provider::Ollama => {
            ollama::chat_stream(&runtime.http_client, request, &config.ollama_base_url, tx).await
        }
        Provider::OpenRouter => {
            let key = config.openrouter_api_key.as_deref().unwrap_or("");
            openrouter::chat_stream(
                &runtime.http_client,
                request,
                &config.openrouter_base_url,
                key,
                tx,
            )
            .await
        }
        Provider::Local => {
            #[cfg(feature = "local")]
            {
                let local = runtime
                    .local
                    .as_ref()
                    .ok_or_else(|| AppError::LlmError("Local runtime not initialized".into()))?;
                local::chat_stream(Arc::clone(local), request, config, tx).await
            }
            #[cfg(not(feature = "local"))]
            {
                let _ = tx.send(StreamChunk::Error("Built without --features local.".into()));
                Ok(())
            }
        }
    }
}

pub async fn health_check(runtime: &LlmRuntime, config: &Config) -> bool {
    match config.provider {
        Provider::Ollama => {
            ollama::health_check(&runtime.http_client, &config.ollama_base_url).await
        }
        Provider::OpenRouter => {
            let key = config.openrouter_api_key.as_deref().unwrap_or("");
            openrouter::health_check(&runtime.http_client, &config.openrouter_base_url, key).await
        }
        Provider::Local => {
            #[cfg(feature = "local")]
            {
                local::health_check(runtime.local.as_ref())
            }
            #[cfg(not(feature = "local"))]
            false
        }
    }
}

/// Fixed allowance for the system prompt (rules text plus a capped directory listing), which
/// is sent with every request but is not part of the stored conversation.
const SYSTEM_PROMPT_TOKENS: usize = 2_500;

/// Rough token estimate: 1 token ≈ 4 characters. Includes the tool schemas and the system
/// prompt allowance, since both are sent with every request.
pub fn estimate_tokens(messages: &[types::Message]) -> usize {
    let schema_chars = serde_json::to_string(&tools::tool_definitions()).map_or(0, |s| s.len());
    let chars: usize = messages
        .iter()
        .map(|m| {
            let mut chars = m.content.len() + m.role.len();
            if let Some(calls) = &m.tool_calls {
                for tc in calls {
                    chars += tc.function.name.len();
                    chars += tc.function.arguments.to_string().len();
                }
            }
            chars
        })
        .sum();
    (chars + schema_chars) / 4 + SYSTEM_PROMPT_TOKENS
}

#[cfg(test)]
mod retry_tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    #[test]
    fn retry_delay_honors_retry_after_and_caps() {
        assert_eq!(retry_delay(Some("3"), 0), Duration::from_secs(3));
        assert_eq!(retry_delay(Some("999"), 0), MAX_RETRY_DELAY);
        assert_eq!(retry_delay(None, 0), Duration::from_secs(1));
        assert_eq!(retry_delay(Some("soon"), 1), Duration::from_secs(2));
    }

    #[test]
    fn error_body_is_trimmed_on_char_boundary() {
        let long = "é".repeat(MAX_ERROR_BODY_CHARS + 10);
        let t = trim_error_body(&long);
        assert_eq!(t.chars().count(), MAX_ERROR_BODY_CHARS + 1);
        assert!(t.ends_with('…'));
        assert_eq!(trim_error_body("  short \n"), "short");
    }

    /// Serve the given raw HTTP responses, one per connection, and return the base URL.
    async fn serve(responses: Vec<&'static str>) -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            for r in responses {
                let (mut sock, _) = listener.accept().await.unwrap();
                let mut buf = [0u8; 4096];
                let _ = sock.read(&mut buf).await;
                sock.write_all(r.as_bytes()).await.unwrap();
            }
        });
        format!("http://{addr}")
    }

    #[tokio::test]
    async fn retries_503_then_succeeds() {
        let url = serve(vec![
            "HTTP/1.1 503 Service Unavailable\r\nRetry-After: 0\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
            "HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok",
        ])
        .await;
        let client = reqwest::Client::new();
        let resp = send_with_retry("Test", || client.get(&url)).await.unwrap();
        assert_eq!(resp.text().await.unwrap(), "ok");
    }

    #[tokio::test]
    async fn non_retryable_status_surfaces_status_and_body() {
        let url = serve(vec![
            "HTTP/1.1 400 Bad Request\r\nContent-Length: 9\r\nConnection: close\r\n\r\nbad model",
        ])
        .await;
        let client = reqwest::Client::new();
        let err = send_with_retry("Test", || client.get(&url))
            .await
            .unwrap_err()
            .to_string();
        assert!(err.contains("400") && err.contains("bad model"), "{err}");
    }

    #[tokio::test]
    async fn gives_up_after_max_retries() {
        let r = "HTTP/1.1 429 Too Many Requests\r\nRetry-After: 0\r\nContent-Length: 4\r\nConnection: close\r\n\r\nslow";
        let url = serve(vec![r, r, r]).await;
        let client = reqwest::Client::new();
        let err = send_with_retry("Test", || client.get(&url))
            .await
            .unwrap_err()
            .to_string();
        assert!(err.contains("429") && err.contains("slow"), "{err}");
    }

    #[test]
    fn estimate_counts_tool_schemas_and_system_prompt() {
        assert!(estimate_tokens(&[]) > SYSTEM_PROMPT_TOKENS);
    }
}
