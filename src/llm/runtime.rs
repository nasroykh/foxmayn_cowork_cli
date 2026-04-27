#[cfg(feature = "local")]
use std::sync::Arc;

#[cfg(feature = "local")]
use super::local::LocalRuntime;
#[cfg(feature = "local")]
use crate::config::Provider;
use crate::config::Config;
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
    /// Build the runtime from config.  For `Provider::Local` this downloads and
    /// loads the GGUF model (potentially several hundred MB on first run).
    #[cfg_attr(not(feature = "local"), allow(unused_variables))]
    pub async fn build(config: &Config) -> Result<Self, AppError> {
        let http_client = reqwest::Client::builder()
            .connect_timeout(std::time::Duration::from_secs(10))
            .build()
            .unwrap_or_default();

        #[cfg(feature = "local")]
        if matches!(config.provider, Provider::Local) {
            eprintln!(
                "[local] Loading model (this may take a moment on first run)…"
            );
            let local = LocalRuntime::load_or_download(config).await?;
            eprintln!("[local] Model ready.");
            return Ok(Self {
                http_client,
                local: Some(Arc::new(local)),
            });
        }

        Ok(Self {
            http_client,
            #[cfg(feature = "local")]
            local: None,
        })
    }
}
