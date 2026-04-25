pub mod ollama;
pub mod openrouter;
pub mod tools;
pub mod types;

use tokio::sync::mpsc::UnboundedSender;

use crate::config::{Config, Provider};
use crate::error::AppError;
use types::{ChatRequest, Message, StreamChunk};

pub async fn chat(
    client: &reqwest::Client,
    request: &ChatRequest,
    config: &Config,
) -> Result<Message, AppError> {
    match config.provider {
        Provider::Ollama => ollama::chat(client, request, &config.ollama_base_url).await,
        Provider::OpenRouter => {
            let key = config.openrouter_api_key.as_deref().unwrap_or("");
            openrouter::chat(client, request, &config.openrouter_base_url, key).await
        }
    }
}

/// Start a streaming chat request. Chunks arrive via `tx`; returns after HTTP headers.
pub async fn chat_stream(
    client: &reqwest::Client,
    request: &ChatRequest,
    config: &Config,
    tx: UnboundedSender<StreamChunk>,
) -> Result<(), crate::error::AppError> {
    match config.provider {
        Provider::Ollama => {
            ollama::chat_stream(client, request, &config.ollama_base_url, tx).await
        }
        Provider::OpenRouter => {
            let key = config.openrouter_api_key.as_deref().unwrap_or("");
            openrouter::chat_stream(client, request, &config.openrouter_base_url, key, tx).await
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

pub async fn health_check(client: &reqwest::Client, config: &Config) -> bool {
    match config.provider {
        Provider::Ollama => ollama::health_check(client, &config.ollama_base_url).await,
        Provider::OpenRouter => {
            let key = config.openrouter_api_key.as_deref().unwrap_or("");
            openrouter::health_check(client, &config.openrouter_base_url, key).await
        }
    }
}
