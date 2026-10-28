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
    /// A fragment of the model's reasoning / thinking trace. Surfaced separately from `ContentDelta`
    /// so the TUI can render or ignore it independently of the main answer.
    ThinkingDelta(String),
    ToolCallDelta {
        index: usize,
        id: Option<String>,
        name: Option<String>,
        arguments_fragment: String,
    },
    Done,
    Error(String),
}

#[derive(Debug, Clone, Serialize)]
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

/// Content of the synthetic tool message inserted for a tool call that has no recorded result.
pub const MISSING_TOOL_RESULT: &str = "No result recorded: the request was interrupted. \
The operation may or may not have run; check the files before retrying.";

/// Restore the invariant every provider requires: each assistant `tool_calls` id is answered by
/// exactly one `tool` message directly after it (before the next non-tool message), and no
/// `tool` message exists without a matching call.
///
/// Missing results are inserted after the existing tool results of that turn, in call order.
/// Orphan and duplicate tool messages are dropped. A call without an id is matched by a tool
/// message that also has no id. Valid histories are left unchanged.
///
/// Must not be applied while a confirmation is pending: that snapshot is intentionally open.
pub fn repair_tool_history(messages: &mut Vec<Message>) {
    let old = std::mem::take(messages);
    let mut out: Vec<Message> = Vec::with_capacity(old.len());
    let mut iter = old.into_iter().peekable();

    while let Some(msg) = iter.next() {
        if msg.role == "tool" {
            // Not consumed by an assistant turn below, so it answers nothing.
            continue;
        }
        let calls: Vec<ToolCall> = if msg.role == "assistant" {
            msg.tool_calls.clone().unwrap_or_default()
        } else {
            Vec::new()
        };
        out.push(msg);
        if calls.is_empty() {
            continue;
        }

        let mut answered = vec![false; calls.len()];
        while iter.peek().is_some_and(|m| m.role == "tool") {
            let tool = iter.next().expect("peeked");
            let slot = calls
                .iter()
                .enumerate()
                .position(|(i, c)| !answered[i] && c.id == tool.tool_call_id);
            if let Some(i) = slot {
                answered[i] = true;
                out.push(tool);
            }
        }
        for (i, call) in calls.iter().enumerate() {
            if !answered[i] {
                out.push(Message::tool_result(
                    call.function.name.clone(),
                    MISSING_TOOL_RESULT,
                    call.id.clone(),
                ));
            }
        }
    }
    *messages = out;
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCall {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    /// OpenAI/OpenRouter wire format requires `"type": "function"` in every tool call.
    /// Default lets us deserialize messages that omit it (e.g. from our own internal storage).
    #[serde(rename = "type", default = "ToolCall::type_function")]
    pub r#type: String,
    pub function: FunctionCall,
}

impl ToolCall {
    fn type_function() -> String {
        "function".to_string()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FunctionCall {
    pub name: String,
    /// Stored and serialized as a JSON Value (object). OpenRouter needs this converted to a
    /// JSON *string* before sending — see `openrouter::args_to_string_in_request`.
    pub arguments: serde_json::Value,
}

/// Parse a provider-supplied tool-call arguments string. Empty or whitespace-only input
/// (zero-argument calls from some providers) is treated as `{}`.
pub fn parse_tool_arguments(raw: &str) -> Result<serde_json::Value, serde_json::Error> {
    if raw.trim().is_empty() {
        return Ok(serde_json::json!({}));
    }
    serde_json::from_str(raw)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Tool {
    pub r#type: String,
    pub function: ToolFunction,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
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
    fn empty_tool_arguments_become_empty_object() {
        assert_eq!(parse_tool_arguments("").unwrap(), json!({}));
        assert_eq!(parse_tool_arguments("  \n").unwrap(), json!({}));
        assert_eq!(
            parse_tool_arguments(r#"{"path":"a"}"#).unwrap(),
            json!({"path": "a"})
        );
        assert!(parse_tool_arguments("{").is_err());
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

#[cfg(test)]
mod repair_tests {
    use super::*;
    use serde_json::json;

    fn call(id: Option<&str>, name: &str) -> ToolCall {
        ToolCall {
            id: id.map(Into::into),
            r#type: "function".into(),
            function: FunctionCall {
                name: name.into(),
                arguments: json!({}),
            },
        }
    }

    fn assistant(calls: Vec<ToolCall>) -> Message {
        Message {
            role: "assistant".into(),
            content: String::new(),
            tool_calls: Some(calls),
            name: None,
            tool_call_id: None,
        }
    }

    fn tool(id: Option<&str>, content: &str) -> Message {
        Message::tool_result("t", content, id.map(Into::into))
    }

    fn shape(m: &[Message]) -> Vec<(String, Option<String>)> {
        m.iter()
            .map(|m| (m.role.clone(), m.tool_call_id.clone()))
            .collect()
    }

    #[test]
    fn valid_history_unchanged() {
        let mut m = vec![
            Message::user("hi"),
            assistant(vec![call(Some("a"), "x"), call(Some("b"), "y")]),
            tool(Some("b"), "rb"),
            tool(Some("a"), "ra"),
            Message::user("next"),
        ];
        let before = serde_json::to_string(&m).unwrap();
        repair_tool_history(&mut m);
        assert_eq!(serde_json::to_string(&m).unwrap(), before);
    }

    #[test]
    fn missing_results_inserted_after_existing_in_call_order() {
        let mut m = vec![
            Message::user("hi"),
            assistant(vec![
                call(Some("a"), "x"),
                call(Some("b"), "y"),
                call(Some("c"), "z"),
            ]),
            tool(Some("b"), "rb"),
        ];
        repair_tool_history(&mut m);
        let ids: Vec<_> = shape(&m).into_iter().skip(2).collect();
        assert_eq!(
            ids,
            vec![
                ("tool".into(), Some("b".into())),
                ("tool".into(), Some("a".into())),
                ("tool".into(), Some("c".into())),
            ]
        );
        assert_eq!(m[3].content, MISSING_TOOL_RESULT);
        assert_eq!(m[3].name.as_deref(), Some("x"));
        assert_eq!(m[2].content, "rb");
    }

    #[test]
    fn missing_results_inserted_before_following_user_message() {
        let mut m = vec![
            assistant(vec![call(Some("a"), "x")]),
            Message::user("again"),
        ];
        repair_tool_history(&mut m);
        assert_eq!(m.len(), 3);
        assert_eq!(m[1].role, "tool");
        assert_eq!(m[2].role, "user");
    }

    #[test]
    fn orphan_and_duplicate_tool_messages_dropped() {
        let mut m = vec![
            tool(Some("z"), "leading orphan"),
            Message::user("hi"),
            assistant(vec![call(Some("a"), "x")]),
            tool(Some("a"), "first"),
            tool(Some("a"), "dup"),
            tool(Some("nope"), "orphan"),
            Message::user("u"),
            tool(Some("a"), "after user"),
        ];
        repair_tool_history(&mut m);
        assert_eq!(
            shape(&m),
            vec![
                ("user".into(), None),
                ("assistant".into(), None),
                ("tool".into(), Some("a".into())),
                ("user".into(), None),
            ]
        );
        assert_eq!(m[2].content, "first");
    }

    #[test]
    fn multiple_turns_repaired_independently() {
        let mut m = vec![
            Message::user("1"),
            assistant(vec![call(Some("a"), "x")]),
            Message::user("2"),
            assistant(vec![call(Some("b"), "y"), call(Some("c"), "z")]),
            tool(Some("c"), "rc"),
        ];
        repair_tool_history(&mut m);
        assert_eq!(
            shape(&m),
            vec![
                ("user".into(), None),
                ("assistant".into(), None),
                ("tool".into(), Some("a".into())),
                ("user".into(), None),
                ("assistant".into(), None),
                ("tool".into(), Some("c".into())),
                ("tool".into(), Some("b".into())),
            ]
        );
    }

    #[test]
    fn calls_without_ids_match_idless_results() {
        let mut m = vec![
            assistant(vec![call(None, "x"), call(None, "y")]),
            tool(None, "r"),
        ];
        repair_tool_history(&mut m);
        assert_eq!(m.len(), 3);
        assert_eq!(m[1].content, "r");
        assert_eq!(m[2].content, MISSING_TOOL_RESULT);
        assert_eq!(m[2].tool_call_id, None);
    }
}
