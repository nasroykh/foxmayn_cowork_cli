#[cfg(feature = "local")]
use std::sync::Arc;

#[cfg(feature = "local")]
use super::local::LocalRuntime;
use crate::config::{Config, Provider};
use crate::error::AppError;

/// Holds all provider-level resources that survive across individual LLM calls.
/// Cheaply cloneable — cloning is just reference-counting.
#[derive(Clone)]
pub struct LlmRuntime {
    pub http_client: reqwest::Client,
    #[cfg(feature = "local")]
    pub local: Option<Arc<LocalRuntime>>,
}

impl LlmRuntime {
    /// Build the runtime from config. For `Provider::Local` this downloads and
    /// loads the GGUF model (potentially several hundred MB on first run).
    /// Returns an error immediately when `PROVIDER=local` but the binary was
    /// not compiled with `--features local`, so callers get one clear failure
    /// point rather than a later `llm::chat` error.
    pub async fn build(config: &Config) -> Result<Self, AppError> {
        let http_client = reqwest::Client::builder()
            .connect_timeout(std::time::Duration::from_secs(10))
            // No global request timeout: streaming responses can run for many minutes.
            // connect_timeout guards against stalled TCP handshakes.
            .build()
            .expect("reqwest TLS init failed");

        if matches!(config.provider, Provider::Local) {
            #[cfg(feature = "local")]
            {
                eprintln!("[local] Loading model (this may take a moment on first run)…");
                let local = LocalRuntime::load_or_download(config).await?;
                eprintln!("[local] Model ready.");
                return Ok(Self {
                    http_client,
                    local: Some(Arc::new(local)),
                });
            }
            #[cfg(not(feature = "local"))]
            return Err(AppError::LlmError(
                "Error: PROVIDER=local requires building with the `local` feature.\n\n\
                 Rebuild with:\n  cargo build --release --features local\n\n\
                 Or run directly:\n  cargo run --features local -- --provider local --dir <path>"
                    .into(),
            ));
        }

        Ok(Self {
            http_client,
            #[cfg(feature = "local")]
            local: None,
        })
    }
}
