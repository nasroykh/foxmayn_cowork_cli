pub mod ollama;
pub mod openrouter;
pub mod tools;
pub mod types;

use crate::config::{Config, Provider};
use crate::error::AppError;
use types::{ChatRequest, Message};

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

pub async fn health_check(client: &reqwest::Client, config: &Config) -> bool {
    match config.provider {
        Provider::Ollama => ollama::health_check(client, &config.ollama_base_url).await,
        Provider::OpenRouter => {
            let key = config.openrouter_api_key.as_deref().unwrap_or("");
            openrouter::health_check(client, &config.openrouter_base_url, key).await
        }
    }
}
