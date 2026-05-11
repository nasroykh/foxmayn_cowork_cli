#[derive(Debug, thiserror::Error)]
pub enum AppError {
    #[error("Filesystem error: {0}")]
    Fs(#[from] std::io::Error),

    #[error("Request failed: {0}")]
    Http(#[from] reqwest::Error),

    #[error("LLM returned invalid response: {0}")]
    LlmError(String),

    #[error("Tool call validation failed: {0}")]
    ToolValidation(String),

    #[error("Serialization error: {0}")]
    Serialization(#[from] serde_json::Error),
}
