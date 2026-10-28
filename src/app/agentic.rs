use std::path::Path;

use tokio::sync::mpsc;

use crate::config::{Config, TOOL_DISPLAY_FULL_RESULT_CAP, ToolDisplayVerbosity};
use crate::llm;
use crate::llm::runtime::LlmRuntime;
use crate::llm::tools::{ToolCallResult, brief_action, dispatch_tool_call, execute_tool};
use crate::llm::types::{
    ChatRequest, FunctionCall, Message, StreamChunk, ToolCall, parse_tool_arguments,
};

use super::events::{AppEvent, LlmOutcome, RequestId};
use super::state::PendingToolCall;

// ── Streaming helpers ─────────────────────────────────────────────────────────

pub(super) struct PartialToolCall {
    /// Provider-side `index` the fragments for this call arrive under.
    pub index: usize,
    pub id: Option<String>,
    pub name: Option<String>,
    pub arguments: String,
}

/// Fold one streamed tool-call delta into `partials`. Fragments are keyed by the provider's
/// `index`; a new non-empty `id` at an index that already holds a different id opens a new
/// call (some providers restart `index` at 0 for every parallel call).
pub(super) fn apply_tool_call_delta(
    partials: &mut Vec<PartialToolCall>,
    index: usize,
    id: Option<String>,
    name: Option<String>,
    arguments_fragment: &str,
) {
    let id = id.filter(|i| !i.is_empty());
    let slot = partials.iter().rposition(|p| p.index == index);
    let slot = match slot {
        Some(s) if id.is_none() || partials[s].id.is_none() || partials[s].id == id => s,
        _ => {
            partials.push(PartialToolCall {
                index,
                id: None,
                name: None,
                arguments: String::new(),
            });
            partials.len() - 1
        }
    };
    let ptc = &mut partials[slot];
    if id.is_some() {
        ptc.id = id;
    }
    if name.is_some() {
        ptc.name = name;
    }
    ptc.arguments.push_str(arguments_fragment);
}

/// Turn assembled streaming fragments into tool calls. Missing ids are synthesized so every
/// call can be answered by a tool message. `Err` carries `(name, raw arguments, parse error)`.
pub(super) fn assemble_tool_calls(
    partials: &[PartialToolCall],
) -> Result<Vec<ToolCall>, (String, String, String)> {
    let mut out = Vec::with_capacity(partials.len());
    for (n, ptc) in partials.iter().enumerate() {
        let name = ptc.name.clone().unwrap_or_default();
        match parse_tool_arguments(&ptc.arguments) {
            Ok(arguments) => out.push(ToolCall {
                id: Some(ptc.id.clone().unwrap_or_else(|| format!("call_{n}"))),
                r#type: "function".to_string(),
                function: FunctionCall { name, arguments },
            }),
            Err(e) => return Err((name, ptc.arguments.clone(), e.to_string())),
        }
    }
    Ok(out)
}

/// Upper bound on one tool result sent back to the model (characters).
const MAX_TOOL_RESULT_CHARS: usize = 100_000;

/// Truncate an oversized tool result at a char boundary with a visible marker.
fn cap_tool_result(result: String) -> String {
    let Some((byte, _)) = result.char_indices().nth(MAX_TOOL_RESULT_CHARS) else {
        return result;
    };
    let dropped = result[byte..].chars().count();
    format!("{}\n[truncated {dropped} chars]", &result[..byte])
}

/// A tool call that stopped the batch because it needs the user's confirmation.
pub(super) struct PendingStop {
    pub description: String,
    pub tool_name: String,
    pub arguments: serde_json::Value,
    pub tool_call_id: Option<String>,
    /// Calls after the gated one; not dispatched yet.
    pub remaining: Vec<ToolCall>,
}

pub(super) struct DispatchOutput {
    /// One tool message per dispatched call, in call order.
    pub messages: Vec<Message>,
    pub pending: Option<PendingStop>,
}

type StreamCtx<'a> = Option<(RequestId, &'a mpsc::UnboundedSender<AppEvent>)>;

/// Surface a `[tool] …` line: live over the event channel when streaming, otherwise collected
/// into `summaries` for the final `LlmOutcome::Complete`.
fn emit_tool_summary(
    tool_name: &str,
    description: &str,
    result: &str,
    config: &Config,
    stream_ctx: StreamCtx<'_>,
    summaries: &mut Vec<String>,
) {
    let summary = format_tool_summary(
        tool_name,
        description,
        result,
        config.tool_display_verbosity,
    );
    if let Some((request_id, tx)) = stream_ctx {
        let _ = tx.send(AppEvent::IntermediateTool {
            request_id,
            name: tool_name.to_string(),
            result: summary,
        });
    } else {
        summaries.push(summary);
    }
}

/// Dispatch `calls` in order, producing one tool message per call. Stops at the first call that
/// needs confirmation and hands back the calls after it, so the caller can answer them later.
pub(super) async fn dispatch_tool_calls(
    calls: &[ToolCall],
    base_path: &Path,
    config: &Config,
    stream_ctx: StreamCtx<'_>,
    summaries: &mut Vec<String>,
) -> DispatchOutput {
    let mut messages = Vec::with_capacity(calls.len());
    for (k, tc) in calls.iter().enumerate() {
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
            return DispatchOutput {
                messages,
                pending: Some(PendingStop {
                    description: r.description,
                    tool_name: r.tool_name,
                    arguments: r.args,
                    tool_call_id: tc.id.clone(),
                    remaining: calls[k + 1..].to_vec(),
                }),
            };
        }

        let s = r.result.unwrap_or_default();
        emit_tool_summary(
            &tc.function.name,
            &r.description,
            &s,
            config,
            stream_ctx,
            summaries,
        );
        messages.push(Message::tool_result(
            &tc.function.name,
            cap_tool_result(s),
            tc.id.clone(),
        ));
    }
    DispatchOutput {
        messages,
        pending: None,
    }
}

/// Apply the user's answer to a pending confirmation, keeping the history valid: every tool
/// call of the interrupted assistant message ends up with exactly one tool message.
///
/// Returns `Some(outcome)` when the turn ends here (declined, or another call needs
/// confirmation); `None` when all calls are answered and the LLM loop should continue.
pub(super) async fn resolve_pending(
    pending: PendingToolCall,
    approved: bool,
    base_path: &Path,
    config: &Config,
    stream_ctx: StreamCtx<'_>,
    conversation: &mut Vec<Message>,
    summaries: &mut Vec<String>,
) -> Option<LlmOutcome> {
    *summaries = pending.summaries;

    if !approved {
        conversation.push(Message::tool_result(
            &pending.tool_name,
            "The user declined this operation.",
            pending.tool_call_id,
        ));
        for tc in pending.remaining {
            conversation.push(Message::tool_result(
                &tc.function.name,
                "Not executed: the user declined an earlier operation in this turn.",
                tc.id,
            ));
        }
        return Some(LlmOutcome::Complete {
            assistant_message: "Operation cancelled.".into(),
            tool_results: std::mem::take(summaries),
        });
    }

    let call = FunctionCall {
        name: pending.tool_name.clone(),
        arguments: pending.arguments,
    };
    let result = match execute_tool(&call, base_path).await {
        Ok(s) => s,
        Err(e) => format!("Error: {e}"),
    };
    emit_tool_summary(
        &pending.tool_name,
        &pending.description,
        &result,
        config,
        stream_ctx,
        summaries,
    );
    conversation.push(Message::tool_result(
        &pending.tool_name,
        cap_tool_result(result),
        pending.tool_call_id,
    ));

    let out =
        dispatch_tool_calls(&pending.remaining, base_path, config, stream_ctx, summaries).await;
    conversation.extend(out.messages);
    let stop = out.pending?;
    Some(LlmOutcome::PendingConfirmation {
        description: stop.description,
        tool_name: stop.tool_name,
        arguments: stop.arguments,
        tool_call_id: stop.tool_call_id,
        user_message: last_user_for_pending("", conversation),
        remaining: stop.remaining,
        summaries: std::mem::take(summaries),
    })
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
///
/// `tool_summaries`: non-streaming `[tool]` lines already collected earlier in this turn.
#[allow(clippy::too_many_arguments)]
pub(super) async fn run_agentic_loop(
    runtime: &LlmRuntime,
    config: &Config,
    base_path: &Path,
    mut working: Vec<Message>,
    mut conversation: Vec<Message>,
    new_user: Option<Message>,
    user_message_for_pending: String,
    mut tool_summaries: Vec<String>,
    stream_ctx: Option<(RequestId, &mpsc::UnboundedSender<AppEvent>)>,
) -> (LlmOutcome, Vec<Message>) {
    let mut user_in_conversation = new_user.is_none();

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
                    // Producers always send `Done` or `Error`; a bare close means truncation.
                    None => {
                        return (
                            LlmOutcome::Error {
                                message: "The response stream ended unexpectedly.".into(),
                            },
                            conversation,
                        );
                    }
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
                        apply_tool_call_delta(
                            &mut partial_tool_calls,
                            index,
                            id,
                            name,
                            &arguments_fragment,
                        );
                    }
                    Some(StreamChunk::Done) => break,
                    Some(StreamChunk::Error(e)) => {
                        return (LlmOutcome::Error { message: e }, conversation);
                    }
                }
            }

            let assembled: Option<Vec<ToolCall>> = if partial_tool_calls.is_empty() {
                None
            } else {
                match assemble_tool_calls(&partial_tool_calls) {
                    Ok(calls) => Some(calls),
                    Err((name, raw, err)) => {
                        return (
                            LlmOutcome::Error {
                                message: format!(
                                    "Failed to parse streamed tool-call arguments for '{name}': {err}. \
                                     Raw fragment: {raw}. This usually means the provider streamed \
                                     the same tool call twice or split it inconsistently."
                                ),
                            },
                            conversation,
                        );
                    }
                }
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
        let has_tool_calls = assembled_tool_calls.as_ref().is_some_and(|c| !c.is_empty());

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

        // Every tool call id of the assistant message must get exactly one tool message before
        // the next request; a gated call parks the rest in the pending confirmation.
        let out = dispatch_tool_calls(
            &tool_calls,
            base_path,
            config,
            stream_ctx,
            &mut tool_summaries,
        )
        .await;
        working.extend(out.messages.iter().cloned());
        conversation.extend(out.messages);

        if let Some(stop) = out.pending {
            let user_message = last_user_for_pending(&user_message_for_pending, &conversation);
            return (
                LlmOutcome::PendingConfirmation {
                    description: stop.description,
                    tool_name: stop.tool_name,
                    arguments: stop.arguments,
                    tool_call_id: stop.tool_call_id,
                    user_message,
                    remaining: stop.remaining,
                    summaries: tool_summaries,
                },
                conversation,
            );
        }
    }

    (
        LlmOutcome::Error {
            message: "Maximum tool round limit reached (too many back-to-back tool calls).".into(),
        },
        conversation,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::collections::HashSet;

    fn call(id: &str, name: &str, args: serde_json::Value) -> ToolCall {
        ToolCall {
            id: Some(id.into()),
            r#type: "function".into(),
            function: FunctionCall {
                name: name.into(),
                arguments: args,
            },
        }
    }

    fn test_config(skip_confirmations: bool) -> Config {
        let mut config = Config::from_env();
        config.skip_confirmations = skip_confirmations;
        config
    }

    fn temp_dir(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("cowork_agentic_{tag}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("a.txt"), "a").unwrap();
        std::fs::write(dir.join("b.txt"), "b").unwrap();
        dir
    }

    fn assistant(calls: &[ToolCall]) -> Message {
        Message {
            role: "assistant".into(),
            content: String::new(),
            tool_calls: Some(calls.to_vec()),
            name: None,
            tool_call_id: None,
        }
    }

    /// Every assistant tool_call id is answered by exactly one tool message, in order,
    /// directly after the assistant message.
    fn assert_history_valid(conversation: &[Message]) {
        let mut i = 0;
        while i < conversation.len() {
            if let Some(calls) = &conversation[i].tool_calls {
                let ids: Vec<_> = calls.iter().map(|c| c.id.clone()).collect();
                let answers: Vec<_> = conversation[i + 1..]
                    .iter()
                    .take(ids.len())
                    .map(|m| {
                        assert_eq!(m.role, "tool");
                        m.tool_call_id.clone()
                    })
                    .collect();
                assert_eq!(ids, answers, "tool calls not answered 1:1 in order");
                i += ids.len();
            }
            i += 1;
        }
        let tool_ids: Vec<_> = conversation
            .iter()
            .filter(|m| m.role == "tool")
            .map(|m| m.tool_call_id.clone())
            .collect();
        let unique: HashSet<_> = tool_ids.iter().collect();
        assert_eq!(
            unique.len(),
            tool_ids.len(),
            "duplicate tool message for one id"
        );
    }

    fn three_calls() -> Vec<ToolCall> {
        vec![
            call("c0", "list_files", json!({"path": "."})),
            call("c1", "delete_file", json!({"path": "a.txt"})),
            call("c2", "delete_file", json!({"path": "b.txt"})),
        ]
    }

    async fn first_dispatch(
        dir: &Path,
        config: &Config,
        calls: &[ToolCall],
    ) -> (Vec<Message>, Option<PendingStop>, Vec<String>) {
        let mut summaries = Vec::new();
        let out = dispatch_tool_calls(calls, dir, config, None, &mut summaries).await;
        let mut conv = vec![Message::user("go"), assistant(calls)];
        conv.extend(out.messages);
        (conv, out.pending, summaries)
    }

    fn pending_from(stop: PendingStop, summaries: Vec<String>) -> PendingToolCall {
        PendingToolCall {
            description: stop.description,
            tool_name: stop.tool_name,
            arguments: stop.arguments,
            tool_call_id: stop.tool_call_id,
            user_message: String::new(),
            remaining: stop.remaining,
            summaries,
        }
    }

    #[tokio::test]
    async fn gated_call_parks_the_rest_and_keeps_earlier_results() {
        let dir = temp_dir("park");
        let config = test_config(false);
        let calls = three_calls();
        let (conv, pending, summaries) = first_dispatch(&dir, &config, &calls).await;
        // Only c0 answered so far; c1 gated, c2 parked.
        assert_eq!(conv.len(), 3);
        assert_eq!(conv[2].tool_call_id.as_deref(), Some("c0"));
        let stop = pending.expect("c1 must need confirmation");
        assert_eq!(stop.tool_call_id.as_deref(), Some("c1"));
        assert_eq!(stop.remaining.len(), 1);
        assert_eq!(summaries.len(), 1);
        assert!(dir.join("a.txt").exists() && dir.join("b.txt").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn approve_chain_answers_every_call_once() {
        let dir = temp_dir("approve");
        let config = test_config(false);
        let calls = three_calls();
        let (mut conv, pending, summaries) = first_dispatch(&dir, &config, &calls).await;
        let mut sums = Vec::new();

        // Approve c1: c2 is gated too, so we get a second confirmation.
        let outcome = resolve_pending(
            pending_from(pending.unwrap(), summaries),
            true,
            &dir,
            &config,
            None,
            &mut conv,
            &mut sums,
        )
        .await;
        let Some(LlmOutcome::PendingConfirmation {
            tool_call_id,
            remaining,
            summaries,
            ..
        }) = outcome
        else {
            panic!("expected a second confirmation");
        };
        assert_eq!(tool_call_id.as_deref(), Some("c2"));
        assert!(remaining.is_empty());
        assert!(!dir.join("a.txt").exists());
        assert_eq!(summaries.len(), 2);

        // Approve c2: all answered, loop may continue.
        let stop = PendingToolCall {
            description: "d".into(),
            tool_name: "delete_file".into(),
            arguments: json!({"path": "b.txt"}),
            tool_call_id,
            user_message: String::new(),
            remaining,
            summaries,
        };
        let outcome = resolve_pending(stop, true, &dir, &config, None, &mut conv, &mut sums).await;
        assert!(outcome.is_none());
        assert!(!dir.join("b.txt").exists());
        assert_eq!(conv.len(), 5);
        assert_history_valid(&conv);
        assert_eq!(sums.len(), 3);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn decline_answers_gated_and_remaining_calls() {
        let dir = temp_dir("decline");
        let config = test_config(false);
        let calls = three_calls();
        let (mut conv, pending, summaries) = first_dispatch(&dir, &config, &calls).await;
        let mut sums = Vec::new();
        let outcome = resolve_pending(
            pending_from(pending.unwrap(), summaries),
            false,
            &dir,
            &config,
            None,
            &mut conv,
            &mut sums,
        )
        .await;
        let Some(LlmOutcome::Complete {
            assistant_message,
            tool_results,
        }) = outcome
        else {
            panic!("expected Complete");
        };
        assert_eq!(assistant_message, "Operation cancelled.");
        assert_eq!(tool_results.len(), 1);
        assert_eq!(conv.len(), 5);
        assert_history_valid(&conv);
        assert!(dir.join("a.txt").exists() && dir.join("b.txt").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn skip_confirmations_answers_all_calls_in_one_pass() {
        let dir = temp_dir("skip");
        let config = test_config(true);
        let calls = three_calls();
        let (conv, pending, _) = first_dispatch(&dir, &config, &calls).await;
        assert!(pending.is_none());
        assert_eq!(conv.len(), 5);
        assert_history_valid(&conv);
        assert!(!dir.join("a.txt").exists() && !dir.join("b.txt").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn tool_result_is_capped_at_char_boundary() {
        let s = "é".repeat(MAX_TOOL_RESULT_CHARS + 7);
        let capped = cap_tool_result(s);
        assert!(capped.ends_with("[truncated 7 chars]"));
        assert_eq!(
            capped.chars().filter(|c| *c == 'é').count(),
            MAX_TOOL_RESULT_CHARS
        );
        let small = "hello".to_string();
        assert_eq!(cap_tool_result(small.clone()), small);
    }

    #[test]
    fn new_id_at_same_index_starts_a_new_call() {
        let mut p = Vec::new();
        apply_tool_call_delta(
            &mut p,
            0,
            Some("a".into()),
            Some("list_files".into()),
            "{\"path\"",
        );
        apply_tool_call_delta(&mut p, 0, None, None, ":\".\"}");
        apply_tool_call_delta(&mut p, 0, Some("b".into()), Some("read_file".into()), "{}");
        // A repeated id keeps appending to the same call.
        apply_tool_call_delta(&mut p, 0, Some("b".into()), None, "");
        let calls = assemble_tool_calls(&p).unwrap();
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0].function.arguments, json!({"path": "."}));
        assert_eq!(calls[1].id.as_deref(), Some("b"));
    }

    #[test]
    fn empty_arguments_and_missing_ids_are_tolerated() {
        let mut p = Vec::new();
        apply_tool_call_delta(&mut p, 2, None, Some("list_files".into()), "");
        let calls = assemble_tool_calls(&p).unwrap();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].function.arguments, json!({}));
        assert_eq!(calls[0].id.as_deref(), Some("call_0"));
    }
}
