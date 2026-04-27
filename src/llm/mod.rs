pub mod ollama;
pub mod openrouter;
pub mod runtime;
pub mod tools;
pub mod types;

#[cfg(feature = "local")]
pub mod local;

#[cfg(feature = "local")]
use std::sync::Arc;

use tokio::sync::mpsc::UnboundedSender;

use crate::config::{Config, Provider};
use crate::error::AppError;
use runtime::LlmRuntime;
use types::{ChatRequest, Message, StreamChunk};

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
            openrouter::chat(&runtime.http_client, request, &config.openrouter_base_url, key).await
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
                let _ = tx.send(StreamChunk::Error(
                    "Built without --features local.".into(),
                ));
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

/// Rough token estimate: 1 token ≈ 4 characters.
pub fn estimate_tokens(messages: &[types::Message]) -> usize {
    messages
        .iter()
        .map(|m| {
            let mut chars = m.content.len() + m.role.len();
            if let Some(calls) = &m.tool_calls {
                for tc in calls {
                    chars += tc.function.name.len();
                    chars += tc.function.arguments.to_string().len();
                }
            }
            chars / 4
        })
        .sum()
}
