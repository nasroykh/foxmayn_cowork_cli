use std::path::{Path, PathBuf};

use tokio::sync::mpsc;

use std::sync::Arc;

use crate::config::Config;
use crate::llm::runtime::LlmRuntime;
use crate::llm::types::Message;

use super::agentic::{resolve_pending, run_agentic_loop};
use super::events::{AppEvent, LlmOutcome, RequestId};
use super::state::PendingToolCall;
use super::system_prompt::{system_prompt, working_dir_summary};

pub async fn send_message(
    runtime: LlmRuntime,
    config: Arc<Config>,
    conversation: Vec<Message>,
    working_dir: Option<PathBuf>,
    user_message: String,
) -> (LlmOutcome, Vec<Message>) {
    if working_dir.is_none() {
        return (
            LlmOutcome::Error {
                message: "Please select a working directory first.".into(),
            },
            conversation,
        );
    }

    let base_path = working_dir.as_ref().unwrap().clone();
    let user_msg = Message::user(&user_message);
    let dir_listing = working_dir_summary(base_path.as_path()).await;
    let mut working: Vec<Message> =
        vec![Message::system(system_prompt(&working_dir, &dir_listing))];
    working.extend(conversation.iter().cloned());
    working.push(user_msg.clone());

    run_agentic_loop(
        &runtime,
        &config,
        base_path.as_path(),
        working,
        conversation,
        Some(user_msg),
        user_message,
        Vec::new(),
        None,
    )
    .await
}

pub async fn confirm_tool(
    runtime: LlmRuntime,
    config: Arc<Config>,
    working_dir: Option<PathBuf>,
    conversation: Vec<Message>,
    pending: PendingToolCall,
    approved: bool,
) -> (LlmOutcome, Vec<Message>) {
    confirm_tool_inner(
        runtime,
        config,
        working_dir,
        conversation,
        pending,
        approved,
        None,
    )
    .await
}

pub async fn send_message_streaming(
    runtime: LlmRuntime,
    config: Arc<Config>,
    conversation: Vec<Message>,
    working_dir: Option<PathBuf>,
    user_message: String,
    request_id: RequestId,
    tx: mpsc::UnboundedSender<AppEvent>,
) -> (LlmOutcome, Vec<Message>) {
    if working_dir.is_none() {
        return (
            LlmOutcome::Error {
                message: "Please select a working directory first.".into(),
            },
            conversation,
        );
    }

    let base_path = working_dir.as_ref().unwrap().clone();
    let user_msg = Message::user(&user_message);
    let dir_listing = working_dir_summary(base_path.as_path()).await;
    let mut working: Vec<Message> =
        vec![Message::system(system_prompt(&working_dir, &dir_listing))];
    working.extend(conversation.iter().cloned());
    working.push(user_msg.clone());

    run_agentic_loop(
        &runtime,
        &config,
        base_path.as_path(),
        working,
        conversation,
        Some(user_msg),
        user_message,
        Vec::new(),
        Some((request_id, &tx)),
    )
    .await
}

#[expect(
    clippy::too_many_arguments,
    reason = "Streaming confirmation needs the pending tool, request generation, and event channel"
)]
pub async fn confirm_tool_streaming(
    runtime: LlmRuntime,
    config: Arc<Config>,
    working_dir: Option<PathBuf>,
    conversation: Vec<Message>,
    pending: PendingToolCall,
    approved: bool,
    request_id: RequestId,
    tx: mpsc::UnboundedSender<AppEvent>,
) -> (LlmOutcome, Vec<Message>) {
    confirm_tool_inner(
        runtime,
        config,
        working_dir,
        conversation,
        pending,
        approved,
        Some((request_id, &tx)),
    )
    .await
}

/// Shared body of `confirm_tool` / `confirm_tool_streaming`. Answers the gated call and every
/// call queued behind it before any further LLM request (see `resolve_pending`).
async fn confirm_tool_inner(
    runtime: LlmRuntime,
    config: Arc<Config>,
    working_dir: Option<PathBuf>,
    mut conversation: Vec<Message>,
    pending: PendingToolCall,
    approved: bool,
    stream_ctx: Option<(RequestId, &mpsc::UnboundedSender<AppEvent>)>,
) -> (LlmOutcome, Vec<Message>) {
    let Some(base_path) = working_dir.clone() else {
        // Nothing can run without a directory; decline so the history stays valid.
        resolve_pending(
            pending,
            false,
            Path::new(""),
            &config,
            stream_ctx,
            &mut conversation,
            &mut Vec::new(),
        )
        .await;
        return (
            LlmOutcome::Error {
                message: "No working directory set.".into(),
            },
            conversation,
        );
    };

    let mut summaries = Vec::new();
    if let Some(outcome) = resolve_pending(
        pending,
        approved,
        base_path.as_path(),
        &config,
        stream_ctx,
        &mut conversation,
        &mut summaries,
    )
    .await
    {
        return (outcome, conversation);
    }

    let dir_listing = working_dir_summary(base_path.as_path()).await;
    let mut working: Vec<Message> =
        vec![Message::system(system_prompt(&working_dir, &dir_listing))];
    working.extend(conversation.iter().cloned());

    run_agentic_loop(
        &runtime,
        &config,
        base_path.as_path(),
        working,
        conversation,
        None,
        String::new(),
        summaries,
        stream_ctx,
    )
    .await
}
