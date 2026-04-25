use std::str::FromStr;

use serde::{Deserialize, Serialize};

// ── Ollama `/api/chat` `think` (thinking models) --------------------------------
// <https://docs.ollama.com/api/chat> — not the same as OpenRouter `reasoning`.

/// Ollama [`think`] on chat requests: a boolean, or for supported models
/// `high` / `medium` / `low`. There is no `minimal` value; use `low` for the lowest
/// string tier.
///
/// [`think`]: https://docs.ollama.com/api/chat
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum OllamaThinkLevel {
    High,
    Medium,
    Low,
}

/// Serialized as a JSON bool or as `"high"` / `"medium"` / `"low"`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum OllamaThink {
    OnOff(bool),
    Level(OllamaThinkLevel),
}

impl std::str::FromStr for OllamaThink {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let t = s.trim();
        if t.is_empty() {
            return Err("empty".into());
        }
        if t.eq_ignore_ascii_case("true")
            || t.eq_ignore_ascii_case("1")
            || t.eq_ignore_ascii_case("on")
        {
            return Ok(OllamaThink::OnOff(true));
        }
        if t.eq_ignore_ascii_case("false")
            || t.eq_ignore_ascii_case("0")
            || t.eq_ignore_ascii_case("off")
        {
            return Ok(OllamaThink::OnOff(false));
        }
        if t.eq_ignore_ascii_case("minimal") {
            return Ok(OllamaThink::Level(OllamaThinkLevel::Low));
        }
        OllamaThinkLevel::from_str(t).map(OllamaThink::Level)
    }
}

impl std::str::FromStr for OllamaThinkLevel {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.trim().to_lowercase().as_str() {
            "high" => Ok(Self::High),
            "medium" => Ok(Self::Medium),
            "low" => Ok(Self::Low),
            other => Err(format!(
                "unknown Ollama think level: {other} (expected true, false, high, medium, low, or minimal as alias for low)"
            )),
        }
    }
}

// ── OpenRouter chat completions `reasoning` --------------------------------

/// OpenRouter [chat completions] `reasoning` field. Omitted for Ollama; the separate
/// [`ChatRequest::think`] field is used for Ollama instead.
///
/// [chat completions]: https://openrouter.ai/docs/api/api-reference/chat/send-chat-completion-request
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ReasoningSummaryVerbosity {
    Auto,
    Concise,
    Detailed,
}

/// Wire values must be lowercase (e.g. `"minimal"`). Serde’s default would emit `"Minimal"`, which
/// OpenRouter rejects (400: invalid option).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ReasoningEffort {
    #[serde(rename = "xhigh")]
    XHigh,
    #[serde(rename = "high")]
    High,
    #[serde(rename = "medium")]
    Medium,
    #[serde(rename = "low")]
    Low,
    #[serde(rename = "minimal")]
    Minimal,
    #[serde(rename = "none")]
    None,
}

impl FromStr for ReasoningEffort {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.trim().to_lowercase().as_str() {
            "xhigh" | "x-high" => Ok(Self::XHigh),
            "high" => Ok(Self::High),
            "medium" => Ok(Self::Medium),
            "low" => Ok(Self::Low),
            "minimal" => Ok(Self::Minimal),
            "none" => Ok(Self::None),
            other => Err(format!(
                "unknown OPENROUTER_REASONING_EFFORT: {other} (expected xhigh, high, medium, low, minimal, none, or 'off' to disable)"
            )),
        }
    }
}

impl FromStr for ReasoningSummaryVerbosity {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.trim().to_lowercase().as_str() {
            "auto" => Ok(Self::Auto),
            "concise" => Ok(Self::Concise),
            "detailed" => Ok(Self::Detailed),
            other => Err(format!(
                "unknown OPENROUTER_REASONING_SUMMARY: {other} (expected auto, concise, detailed, or 'off' to omit)"
            )),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RequestReasoning {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub effort: Option<ReasoningEffort>,
    /// Reasoning summary verbosity; omit when not set.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub summary: Option<ReasoningSummaryVerbosity>,
}

/// A single event from a streaming LLM response.
#[derive(Debug, Clone)]
pub enum StreamChunk {
    ContentDelta(String),
    ToolCallDelta {
        index: usize,
        id: Option<String>,
        name: Option<String>,
        arguments_fragment: String,
    },
    Done,
    Error(String),
}

#[derive(Debug, Serialize)]
pub struct ChatRequest {
    pub model: String,
    pub messages: Vec<Message>,
    pub tools: Vec<Tool>,
    pub stream: bool,
    /// OpenRouter only. Omitted when `None`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reasoning: Option<RequestReasoning>,
    /// Ollama only: /api/chat `think`. Omitted when `None`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub think: Option<OllamaThink>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Message {
    pub role: String,
    pub content: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<Vec<ToolCall>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// OpenRouter requires this to match the tool_call id when role = "tool"
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
}

impl Message {
    pub fn user(content: impl Into<String>) -> Self {
        Self {
            role: "user".into(),
            content: content.into(),
            tool_calls: None,
            name: None,
            tool_call_id: None,
        }
    }

    pub fn system(content: impl Into<String>) -> Self {
        Self {
            role: "system".into(),
            content: content.into(),
            tool_calls: None,
            name: None,
            tool_call_id: None,
        }
    }

    pub fn tool_result(
        tool_name: impl Into<String>,
        content: impl Into<String>,
        tool_call_id: Option<String>,
    ) -> Self {
        Self {
            role: "tool".into(),
            content: content.into(),
            tool_calls: None,
            name: Some(tool_name.into()),
            tool_call_id,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCall {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    pub function: FunctionCall,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FunctionCall {
    pub name: String,
    pub arguments: serde_json::Value,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Tool {
    pub r#type: String,
    pub function: ToolFunction,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ToolFunction {
    pub name: String,
    pub description: String,
    pub parameters: serde_json::Value,
}

#[cfg(test)]
mod wire_format_tests {
    use super::*;
    use serde_json::json;

    /// <https://docs.ollama.com/api/chat> — `think`: bool or "high"|"medium"|"low"
    #[test]
    fn ollama_think_serializes_like_api_docs() {
        assert_eq!(
            serde_json::to_value(OllamaThink::OnOff(true)).unwrap(),
            json!(true)
        );
        assert_eq!(
            serde_json::to_value(OllamaThink::OnOff(false)).unwrap(),
            json!(false)
        );
        assert_eq!(
            serde_json::to_value(OllamaThink::Level(OllamaThinkLevel::High)).unwrap(),
            json!("high")
        );
        assert_eq!(
            serde_json::to_value(OllamaThink::Level(OllamaThinkLevel::Medium)).unwrap(),
            json!("medium")
        );
        assert_eq!(
            serde_json::to_value(OllamaThink::Level(OllamaThinkLevel::Low)).unwrap(),
            json!("low")
        );
    }

    /// OpenRouter rejects PascalCase effort values (e.g. `"Minimal"`).
    #[test]
    fn openrouter_reasoning_effort_is_lowercase() {
        let r = RequestReasoning {
            effort: Some(ReasoningEffort::Minimal),
            summary: None,
        };
        let v = serde_json::to_value(&r).unwrap();
        assert_eq!(v["effort"], json!("minimal"));
    }

    #[test]
    fn openrouter_reasoning_summary_is_lowercase() {
        let r = RequestReasoning {
            effort: None,
            summary: Some(ReasoningSummaryVerbosity::Auto),
        };
        let v = serde_json::to_value(&r).unwrap();
        assert_eq!(v["summary"], json!("auto"));
    }
}
