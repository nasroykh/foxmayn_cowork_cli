use std::str::FromStr;

use clap::ValueEnum;

use crate::llm::types::{
    OllamaThink, OllamaThinkLevel, ReasoningEffort, ReasoningSummaryVerbosity, RequestReasoning,
};

#[derive(Debug, Clone, PartialEq, Eq, ValueEnum)]
pub enum Provider {
    Ollama,
    OpenRouter,
    /// Offline inference via llama.cpp. Requires building with `--features local`.
    Local,
}

/// Controls how much detail is shown for a Tool chat entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolDisplayVerbosity {
    /// `[tool_name] Brief action` — no arguments, no result. Cleanest for end users.
    Default,
    /// `[tool_name] Description with args` — adds the parameters.
    Minimal,
    /// `[tool_name] Description with args → result snippet (capped)` — for technical users.
    Full,
}

impl FromStr for ToolDisplayVerbosity {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.trim().to_lowercase().as_str() {
            "default" => Ok(Self::Default),
            "minimal" => Ok(Self::Minimal),
            "full" => Ok(Self::Full),
            other => Err(format!(
                "unknown TOOL_DISPLAY_VERBOSITY: {other} (expected default, minimal, or full)"
            )),
        }
    }
}

/// Cap on the result snippet length appended in `Full` mode.
pub const TOOL_DISPLAY_FULL_RESULT_CAP: usize = 240;

/// Controls whether the model's reasoning / thinking tokens are surfaced to the user.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ThinkingDisplay {
    /// Discard thinking tokens silently (default).
    Off,
    /// Show a single dimmed `[Thinking… (N chars)]` line while reasoning is in flight; clear it
    /// once content / tool calls arrive. Nothing is kept in the chat history.
    Inline,
    /// Stream thinking tokens live like the assistant text and keep a permanent dimmed
    /// "Thinking" entry in the chat once the round finishes.
    Full,
}

impl FromStr for ThinkingDisplay {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.trim().to_lowercase().as_str() {
            "off" | "false" | "0" | "no" => Ok(Self::Off),
            "inline" => Ok(Self::Inline),
            "full" => Ok(Self::Full),
            other => Err(format!(
                "unknown THINKING_DISPLAY: {other} (expected off, inline, or full)"
            )),
        }
    }
}

#[derive(Debug, Clone)]
#[cfg_attr(not(feature = "local"), allow(dead_code))]
pub struct Config {
    pub provider: Provider,
    pub model: String,
    pub openrouter_api_key: Option<String>,
    pub ollama_base_url: String,
    pub openrouter_base_url: String,
    /// Set when `PROVIDER=openrouter`. Sent as the `reasoning` object on chat completions; `None`
    /// means omit the field (e.g. `OPENROUTER_REASONING_EFFORT=off`).
    pub openrouter_reasoning: Option<RequestReasoning>,
    /// Set when `PROVIDER=ollama`. Sent as `think` on `/api/chat` (separate from OpenRouter
    /// `reasoning`). `None` = omit the field (`OLLAMA_THINK=off`).
    pub ollama_think: Option<OllamaThink>,
    /// Stream tokens as they arrive. Disabled by `STREAMING=false/0/off`. Default: `true`.
    pub streaming_enabled: bool,
    /// Execute destructive tools without asking for confirmation. Default: `false`.
    pub skip_confirmations: bool,
    /// Estimated token ceiling for the conversation. `0` disables the guard.
    pub context_max_tokens: usize,
    /// Fraction of `context_max_tokens` at which a warning is shown (e.g. `0.80`).
    pub context_warn_ratio: f64,

    // ── Local provider (Provider::Local) ──────────────────────────────────────
    /// HuggingFace repo id, e.g. `"Qwen/Qwen2.5-1.5B-Instruct-GGUF"`.
    pub local_model_repo: String,
    /// Filename inside the repo, e.g. `"qwen2.5-1.5b-instruct-q4_k_m.gguf"`.
    pub local_model_file: String,
    /// Override: use this local path directly, skip HuggingFace download.
    pub local_model_path: Option<std::path::PathBuf>,
    /// Context window size in tokens passed to llama.cpp.
    pub local_context_tokens: u32,
    /// Number of model layers to offload to GPU (0 = CPU-only).
    pub local_gpu_layers: u32,
    /// CPU thread count for inference (`None` = auto / llama.cpp default).
    pub local_threads: Option<usize>,
    /// Hard cap on generated tokens per response.
    pub local_max_output_tokens: usize,
    /// Sampling temperature (lower = more deterministic; 0.1 is good for tool calls).
    pub local_temperature: f32,

    /// How much detail to show for Tool chat entries.
    pub tool_display_verbosity: ToolDisplayVerbosity,

    /// Whether/how to surface model reasoning tokens in the TUI.
    pub thinking_display: ThinkingDisplay,
}

fn env_bool(name: &str, default: bool) -> bool {
    std::env::var(name)
        .map(|v| match v.to_lowercase().as_str() {
            "true" | "1" | "on" | "yes" => true,
            "false" | "0" | "off" | "no" => false,
            other => {
                eprintln!("[config] unknown {name} value: {other:?} — using {default}");
                default
            }
        })
        .unwrap_or(default)
}

/// Builds [`RequestReasoning`] for OpenRouter. `OPENROUTER_REASONING_EFFORT=off` (or empty) omits
/// the whole `reasoning` block. When the variable is unset, defaults to `effort: "minimal"`.
fn openrouter_reasoning_from_env() -> Option<RequestReasoning> {
    let effort: Option<ReasoningEffort> = match std::env::var("OPENROUTER_REASONING_EFFORT") {
        Ok(s) if s.is_empty() || s.eq_ignore_ascii_case("off") => return None,
        Ok(s) => match ReasoningEffort::from_str(&s) {
            Ok(e) => Some(e),
            Err(e) => {
                eprintln!("[config] {e} — using minimal");
                Some(ReasoningEffort::Minimal)
            }
        },
        Err(_) => Some(ReasoningEffort::Minimal),
    };

    let summary: Option<ReasoningSummaryVerbosity> =
        match std::env::var("OPENROUTER_REASONING_SUMMARY") {
            Ok(s) if s.is_empty() || s.eq_ignore_ascii_case("off") => None,
            Ok(s) => match ReasoningSummaryVerbosity::from_str(&s) {
                Ok(v) => Some(v),
                Err(e) => {
                    eprintln!("[config] {e} — omitting reasoning summary");
                    None
                }
            },
            Err(_) => None,
        };

    Some(RequestReasoning { effort, summary })
}

fn thinking_display_from_env() -> ThinkingDisplay {
    let val = match std::env::var("THINKING_DISPLAY") {
        Ok(s) if !s.is_empty() => s,
        _ => return ThinkingDisplay::Off,
    };
    match val.parse::<ThinkingDisplay>() {
        Ok(v) => v,
        Err(e) => {
            eprintln!("[config] {e} — using off");
            ThinkingDisplay::Off
        }
    }
}

fn tool_display_verbosity_from_env() -> ToolDisplayVerbosity {
    let val = match std::env::var("TOOL_DISPLAY_VERBOSITY") {
        Ok(s) if !s.is_empty() => s,
        _ => return ToolDisplayVerbosity::Default,
    };
    match val.parse::<ToolDisplayVerbosity>() {
        Ok(v) => v,
        Err(e) => {
            eprintln!("[config] {e} — using default");
            ToolDisplayVerbosity::Default
        }
    }
}

/// Ollama `think` per <https://docs.ollama.com/api/chat>. Defaults to `low` so thinking models
/// work out of the box. Set OLLAMA_THINK=off to omit the field for non-thinking models.
fn ollama_think_from_env() -> Option<OllamaThink> {
    let val = match std::env::var("OLLAMA_THINK") {
        Ok(s) => s,
        Err(_) => return Some(OllamaThink::Level(OllamaThinkLevel::Low)),
    };
    if val.is_empty() || val.eq_ignore_ascii_case("off") {
        return None;
    }
    match val.parse::<OllamaThink>() {
        Ok(t) => Some(t),
        Err(e) => {
            eprintln!("[config] OLLAMA_THINK: {e} -- using low");
            Some(OllamaThink::Level(OllamaThinkLevel::Low))
        }
    }
}

impl Config {
    pub fn from_env() -> Self {
        let provider = match std::env::var("PROVIDER")
            .unwrap_or_default()
            .to_lowercase()
            .as_str()
        {
            "ollama" => Provider::Ollama,
            "local" => Provider::Local,
            _ => Provider::OpenRouter,
        };

        let openrouter_reasoning = if matches!(provider, Provider::OpenRouter) {
            openrouter_reasoning_from_env()
        } else {
            None
        };

        let ollama_think = if matches!(provider, Provider::Ollama) {
            ollama_think_from_env()
        } else {
            None
        };

        let local_model_repo = std::env::var("LOCAL_MODEL_REPO")
            .unwrap_or_else(|_| "Qwen/Qwen2.5-1.5B-Instruct-GGUF".into());
        let local_model_file = std::env::var("LOCAL_MODEL_FILE")
            .unwrap_or_else(|_| "qwen2.5-1.5b-instruct-q4_k_m.gguf".into());
        let local_model_path = std::env::var("LOCAL_MODEL_PATH").ok().map(Into::into);
        let local_context_tokens = std::env::var("LOCAL_CONTEXT_TOKENS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(8192);
        let local_gpu_layers = std::env::var("LOCAL_GPU_LAYERS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(0);
        let local_threads = std::env::var("LOCAL_THREADS")
            .ok()
            .and_then(|v| v.parse().ok());
        let local_max_output_tokens = std::env::var("LOCAL_MAX_OUTPUT_TOKENS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(1024);
        let local_temperature = std::env::var("LOCAL_TEMPERATURE")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(0.1_f32);

        Self {
            provider,
            model: std::env::var("MODEL").unwrap_or_else(|_| "google/gemini-2.5-flash-lite".into()),
            openrouter_api_key: std::env::var("OPENROUTER_API_KEY").ok(),
            ollama_base_url: std::env::var("OLLAMA_BASE_URL")
                .unwrap_or_else(|_| "http://localhost:11434".into()),
            openrouter_base_url: std::env::var("OPENROUTER_BASE_URL")
                .unwrap_or_else(|_| "https://openrouter.ai/api/v1".into()),
            openrouter_reasoning,
            ollama_think,
            streaming_enabled: env_bool("STREAMING", true),
            skip_confirmations: env_bool("SKIP_CONFIRMATIONS", false),
            context_max_tokens: std::env::var("CONTEXT_MAX_TOKENS")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(128_000),
            context_warn_ratio: std::env::var("CONTEXT_WARN_RATIO")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(0.80),
            local_model_repo,
            local_model_file,
            local_model_path,
            local_context_tokens,
            local_gpu_layers,
            local_threads,
            local_max_output_tokens,
            local_temperature,
            tool_display_verbosity: tool_display_verbosity_from_env(),
            thinking_display: thinking_display_from_env(),
        }
    }

    pub fn with_overrides(
        mut self,
        provider: Option<Provider>,
        model: Option<String>,
        streaming: Option<bool>,
        skip_confirmations: bool,
    ) -> Self {
        if let Some(p) = provider {
            if p != self.provider {
                self.openrouter_reasoning = if matches!(p, Provider::OpenRouter) {
                    openrouter_reasoning_from_env()
                } else {
                    None
                };
                self.ollama_think = if matches!(p, Provider::Ollama) {
                    ollama_think_from_env()
                } else {
                    None
                };
            }
            self.provider = p;
        }
        if let Some(m) = model {
            self.model = m;
        }
        if let Some(s) = streaming {
            self.streaming_enabled = s;
        }
        if skip_confirmations {
            self.skip_confirmations = true;
        }
        self
    }
}
