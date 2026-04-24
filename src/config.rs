use std::str::FromStr;

use clap::ValueEnum;

use crate::llm::types::{
    OllamaThink, OllamaThinkLevel, ReasoningEffort, ReasoningSummaryVerbosity, RequestReasoning,
};

#[derive(Debug, Clone, PartialEq, Eq, ValueEnum)]
pub enum Provider {
    Ollama,
    OpenRouter,
}

#[derive(Debug, Clone)]
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

    let summary: Option<ReasoningSummaryVerbosity> = match std::env::var("OPENROUTER_REASONING_SUMMARY")
    {
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

/// Ollama `think` per <https://docs.ollama.com/api/chat> — `off` omits the field. Unset defaults
/// to `low` (closest to OpenRouter’s “minimal” effort; Ollama has no `minimal` string).
fn ollama_think_from_env() -> Option<OllamaThink> {
    match std::env::var("OLLAMA_THINK") {
        Ok(s) if s.is_empty() || s.eq_ignore_ascii_case("off") => None,
        Ok(s) => match s.parse::<OllamaThink>() {
            Ok(t) => Some(t),
            Err(e) => {
                eprintln!("[config] OLLAMA_THINK: {e} — using low");
                Some(OllamaThink::Level(OllamaThinkLevel::Low))
            }
        },
        Err(_) => Some(OllamaThink::Level(OllamaThinkLevel::Low)),
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
        }
    }

    pub fn with_overrides(mut self, provider: Option<Provider>, model: Option<String>) -> Self {
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
        self
    }
}
