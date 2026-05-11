use std::path::Path;

use tokio::sync::mpsc;

use crate::config::{Config, TOOL_DISPLAY_FULL_RESULT_CAP, ToolDisplayVerbosity};
use crate::llm;
use crate::llm::runtime::LlmRuntime;
use crate::llm::tools::{ToolCallResult, brief_action, dispatch_tool_call, execute_tool};
use crate::llm::types::{ChatRequest, FunctionCall, Message, StreamChunk, ToolCall};

use super::events::{AppEvent, LlmOutcome, RequestId};

// ── Streaming helpers ─────────────────────────────────────────────────────────

pub(super) struct PartialToolCall {
    pub id: Option<String>,
    pub name: Option<String>,
    pub arguments: String,
}

/// If `user_message_for_pending` is non-empty, it is used in `LlmOutcome::PendingConfirmation`
/// (new user turn from `send_message`). Otherwise, the most recent `user` turn in
/// `conversation` is used (e.g. after a confirmed tool).
pub(super) fn last_user_for_pending(
    user_message_for_pending: &str,
    conversation: &[Message],
) -> String {
    if !user_message_for_pending.is_empty() {
        user_message_for_pending.to_string()
    } else {
        conversation
            .iter()
            .rev()
            .find(|m| m.role == "user")
            .map(|m| m.content.clone())
            .unwrap_or_default()
    }
}

/// Format any tool error for the model with a category-specific recovery hint.
pub(super) fn format_tool_error(msg: &str) -> String {
    let lower = msg.to_ascii_lowercase();

    if lower.contains("path '")
        || lower.contains("outside the working directory")
        || lower.contains("'..' components")
        || lower.contains("working directory not accessible")
    {
        return format!(
            "Error: {msg}\n\
             Hint: paths must be relative (e.g. 'src/main.rs'). Use '.' to refer to the working \
             directory root. Do not use '..' or absolute paths from outside the working directory."
        );
    }

    if lower.contains("missing required argument") || lower.contains("missing required array") {
        return format!(
            "Error: {msg}\n\
             Hint: this is NOT a path-format problem. Re-read the tool's parameter list and \
             include EVERY required argument on the next call."
        );
    }

    if lower.contains("invalid regex") || lower.contains("invalid filename regex") {
        return format!(
            "Error: {msg}\n\
             Hint: escape special characters (use '\\\\.md$' to match files ending in .md) and \
             ensure the regex is valid Rust regex syntax."
        );
    }

    if lower.contains("no such file") || lower.contains("not found") || lower.contains("nosuchfile")
    {
        return format!(
            "Error: {msg}\n\
             Hint: the path does not exist. Verify with `find_files` or `list_files` before \
             retrying. Do not guess a different path — if you cannot locate the file, ask the user."
        );
    }

    if lower.contains("already exists") {
        return format!(
            "Error: {msg}\n\
             Hint: the destination is occupied. Either choose a different path, or — if the user's \
             intent was to modify the existing file — use `patch_file` or `edit_file` instead."
        );
    }

    if lower.contains("permission denied") || lower.contains("access is denied") {
        return format!(
            "Error: {msg}\n\
             Hint: this is a system permission error and cannot be fixed by retrying. Stop and \
             report it to the user in plain text."
        );
    }

    if lower.contains("matched files but produced no filename changes")
        || lower.contains("no files matched")
    {
        return format!(
            "Error: {msg}\n\
             Hint: your pattern did not match anything actionable. Either widen the regex or \
             confirm with the user that there is anything to do."
        );
    }

    if lower.contains("must appear exactly once") || lower.contains("does not appear in") {
        return format!(
            "Error: {msg}\n\
             Hint: `patch_file` needs a search string that appears EXACTLY ONCE. Read the file \
             with `read_file` first, then choose a longer/more unique substring. If the change \
             is broad, use `edit_file` instead."
        );
    }

    format!(
        "Error: {msg}\nHint: review the tool description and arguments, then either retry with corrected input or report the issue to the user."
    )
}

pub(super) async fn apply_confirmation_policy(
    mut result: ToolCallResult,
    base_path: &Path,
    skip_confirmations: bool,
) -> ToolCallResult {
    if !result.requires_confirmation || !skip_confirmations {
        return result;
    }

    let call = FunctionCall {
        name: result.tool_name.clone(),
        arguments: result.args.clone(),
    };
    result.result = Some(match execute_tool(&call, base_path).await {
        Ok(output) => output,
        Err(e) => format!("Error: {e}"),
    });
    result.requires_confirmation = false;
    result.description = format!("{} (confirmation skipped)", result.description);
    result
}

/// Build the full chat-panel line for a tool entry, honouring the user's verbosity setting.
/// Always prefixed with `[tool_name]`; subsequent renderers should not add another prefix.
pub(super) fn format_tool_summary(
    tool_name: &str,
    description: &str,
    result: &str,
    verbosity: ToolDisplayVerbosity,
) -> String {
    match verbosity {
        ToolDisplayVerbosity::Default => format!("[{tool_name}] {}", brief_action(tool_name)),
        ToolDisplayVerbosity::Minimal => format!("[{tool_name}] {description}"),
        ToolDisplayVerbosity::Full => {
            let trimmed = result.trim();
            let first_line = trimmed.lines().next().unwrap_or("").trim();
            let cap = TOOL_DISPLAY_FULL_RESULT_CAP;
            let snippet: String = if first_line.chars().count() <= cap {
                first_line.to_string()
            } else {
                let mut s: String = first_line.chars().take(cap.saturating_sub(1)).collect();
                s.push('…');
                s
            };
            if snippet.is_empty() {
                format!("[{tool_name}] {description}")
            } else {
                format!("[{tool_name}] {description} → {snippet}")
            }
        }
    }
}

/// Unified agentic loop. When `stream_ctx` is `None`, uses non-streaming `llm::chat`.
/// When `stream_ctx` is `Some((request_id, tx))`, uses `llm::chat_stream` and forwards
/// `StreamChunk` events as they arrive.
///
/// `new_user`: the current user line when starting from `send_message` (not yet in `conversation`);
/// `None` when the transcript already contains the full user turn (e.g. right after
/// a confirmed tool).
#[allow(clippy::too_many_arguments)]
pub(super) async fn run_agentic_loop(
    runtime: &LlmRuntime,
    config: &Config,
    base_path: &Path,
    mut working: Vec<Message>,
    mut conversation: Vec<Message>,
    new_user: Option<Message>,
    user_message_for_pending: String,
    stream_ctx: Option<(RequestId, &mpsc::UnboundedSender<AppEvent>)>,
) -> (LlmOutcome, Vec<Message>) {
    let mut user_in_conversation = new_user.is_none();
    let mut tool_summaries: Vec<String> = Vec::new();

    for _ in 0..crate::llm::MAX_TOOL_ROUNDS {
        let request = ChatRequest {
            model: config.model.clone(),
            messages: working.clone(),
            tools: crate::llm::tools::tool_definitions(),
            stream: stream_ctx.is_some(),
            reasoning: config.openrouter_reasoning.clone(),
            think: config.ollama_think,
        };

        // ── Fetch the assistant message (streaming or non-streaming) ──────────
        let (assistant_msg, assembled_tool_calls, content) = if let Some((request_id, tx)) =
            stream_ctx.as_ref()
        {
            // Streaming path
            let (chunk_tx, mut chunk_rx) = mpsc::unbounded_channel::<StreamChunk>();

            if let Err(e) = llm::chat_stream(runtime, &request, config, chunk_tx).await {
                return (
                    LlmOutcome::Error {
                        message: e.to_string(),
                    },
                    conversation,
                );
            }

            let mut content = String::new();
            let mut partial_tool_calls: Vec<PartialToolCall> = Vec::new();

            loop {
                match chunk_rx.recv().await {
                    None => break,
                    Some(StreamChunk::ContentDelta(text)) => {
                        content.push_str(&text);
                        let _ = tx.send(AppEvent::StreamChunk {
                            request_id: *request_id,
                            chunk: StreamChunk::ContentDelta(text),
                        });
                    }
                    Some(StreamChunk::ThinkingDelta(text)) => {
                        let _ = tx.send(AppEvent::StreamChunk {
                            request_id: *request_id,
                            chunk: StreamChunk::ThinkingDelta(text),
                        });
                    }
                    Some(StreamChunk::ToolCallDelta {
                        index,
                        id,
                        name,
                        arguments_fragment,
                    }) => {
                        while partial_tool_calls.len() <= index {
                            partial_tool_calls.push(PartialToolCall {
                                id: None,
                                name: None,
                                arguments: String::new(),
                            });
                        }
                        let ptc = &mut partial_tool_calls[index];
                        if id.is_some() {
                            ptc.id = id;
                        }
                        if name.is_some() {
                            ptc.name = name;
                        }
                        ptc.arguments.push_str(&arguments_fragment);
                    }
                    Some(StreamChunk::Done) => break,
                    Some(StreamChunk::Error(e)) => {
                        return (LlmOutcome::Error { message: e }, conversation);
                    }
                }
            }

            let has_tool_calls = !partial_tool_calls.is_empty();
            let assembled: Option<Vec<ToolCall>> = if has_tool_calls {
                Some(
                    partial_tool_calls
                        .iter()
                        .map(|ptc| {
                            let arguments: serde_json::Value =
                                serde_json::from_str(&ptc.arguments).unwrap_or_default();
                            ToolCall {
                                id: ptc.id.clone(),
                                r#type: "function".to_string(),
                                function: FunctionCall {
                                    name: ptc.name.clone().unwrap_or_default(),
                                    arguments,
                                },
                            }
                        })
                        .collect(),
                )
            } else {
                None
            };

            let msg = Message {
                role: "assistant".into(),
                content: content.clone(),
                tool_calls: assembled.clone(),
                name: None,
                tool_call_id: None,
            };
            (msg, assembled, content)
        } else {
            // Non-streaming path
            let msg = match llm::chat(runtime, &request, config).await {
                Ok(r) => r,
                Err(e) => {
                    return (
                        LlmOutcome::Error {
                            message: e.to_string(),
                        },
                        conversation,
                    );
                }
            };
            let content = msg.content.clone();
            let tc = msg.tool_calls.clone();
            (msg, tc, content)
        };

        // ── Handle response ───────────────────────────────────────────────────
        let has_tool_calls = assembled_tool_calls
            .as_ref()
            .is_some_and(|c| !c.is_empty());

        if !has_tool_calls {
            if content.trim().is_empty() {
                return (
                    LlmOutcome::Error {
                        message: "The model returned an empty response. Try being more specific, or break the task into smaller steps.".into(),
                    },
                    conversation,
                );
            }
            if !user_in_conversation && let Some(m) = new_user.as_ref() {
                conversation.push(m.clone());
            }
            conversation.push(assistant_msg);
            return (
                LlmOutcome::Complete {
                    assistant_message: content,
                    // Non-streaming path: accumulated summaries. Streaming: empty (surfaced live).
                    tool_results: tool_summaries,
                },
                conversation,
            );
        }

        let tool_calls: Vec<ToolCall> = assembled_tool_calls.unwrap_or_default();
        let mut dispatched: Vec<ToolCallResult> = Vec::new();

        for tc in &tool_calls {
            let r = match dispatch_tool_call(&tc.function, base_path).await {
                Ok(r) => r,
                Err(e) => {
                    let msg = e.to_string();
                    ToolCallResult {
                        result: Some(format_tool_error(&msg)),
                        requires_confirmation: false,
                        description: format!("Tool error: {msg}"),
                        tool_name: tc.function.name.clone(),
                        args: tc.function.arguments.clone(),
                    }
                }
            };
            let r = apply_confirmation_policy(r, base_path, config.skip_confirmations).await;

            if r.requires_confirmation {
                if !user_in_conversation && let Some(m) = new_user.as_ref() {
                    conversation.push(m.clone());
                }
                conversation.push(assistant_msg.clone());
                return (
                    LlmOutcome::PendingConfirmation {
                        description: r.description,
                        tool_name: r.tool_name,
                        arguments: r.args,
                        tool_call_id: tc.id.clone(),
                        user_message: last_user_for_pending(
                            &user_message_for_pending,
                            &conversation,
                        ),
                    },
                    conversation,
                );
            }
            dispatched.push(r);
        }

        if !user_in_conversation && let Some(m) = new_user.as_ref() {
            conversation.push(m.clone());
            user_in_conversation = true;
        }

        // Flush streamed assistant text to a permanent entry before the next round.
        if let Some((request_id, tx)) = stream_ctx.as_ref()
            && !content.trim().is_empty()
        {
            let _ = tx.send(AppEvent::IntermediateAssistant {
                request_id: *request_id,
                content: content.clone(),
            });
        }

        working.push(assistant_msg.clone());
        conversation.push(assistant_msg);

        for (tc, result) in tool_calls.iter().zip(dispatched.iter()) {
            let s = result.result.clone().unwrap_or_default();
            let summary = format_tool_summary(
                &tc.function.name,
                &result.description,
                &s,
                config.tool_display_verbosity,
            );

            if let Some((request_id, tx)) = stream_ctx.as_ref() {
                let _ = tx.send(AppEvent::IntermediateTool {
                    request_id: *request_id,
                    name: tc.function.name.clone(),
                    result: summary,
                });
            } else {
                tool_summaries.push(summary);
            }

            let tmsg = Message::tool_result(&tc.function.name, &s, tc.id.clone());
            working.push(tmsg.clone());
            conversation.push(tmsg);
        }
    }

    (
        LlmOutcome::Error {
            message: "Maximum tool round limit reached (too many back-to-back tool calls).".into(),
        },
        conversation,
    )
}
