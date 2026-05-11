use std::path::PathBuf;

use tokio::sync::mpsc;

use std::sync::Arc;

use crate::config::Config;
use crate::llm::runtime::LlmRuntime;
use crate::llm::tools::execute_tool;
use crate::llm::types::{FunctionCall, Message};

use super::agentic::run_agentic_loop;
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
        None,
    )
    .await
}

pub async fn confirm_tool(
    runtime: LlmRuntime,
    config: Arc<Config>,
    working_dir: Option<PathBuf>,
    mut conversation: Vec<Message>,
    pending: PendingToolCall,
    approved: bool,
) -> (LlmOutcome, Vec<Message>) {
    if !approved {
        return (
            LlmOutcome::Complete {
                assistant_message: "Operation cancelled.".into(),
                tool_results: vec![],
            },
            conversation,
        );
    }

    let base_path = match &working_dir {
        Some(p) => p.clone(),
        None => {
            return (
                LlmOutcome::Error {
                    message: "No working directory set.".into(),
                },
                conversation,
            );
        }
    };

    let call = FunctionCall {
        name: pending.tool_name.clone(),
        arguments: pending.arguments,
    };

    let result_str = match execute_tool(&call, &base_path).await {
        Ok(s) => s,
        Err(e) => format!("Error: {e}"),
    };

    conversation.push(Message::tool_result(
        &pending.tool_name,
        &result_str,
        pending.tool_call_id,
    ));

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
    mut conversation: Vec<Message>,
    pending: PendingToolCall,
    approved: bool,
    request_id: RequestId,
    tx: mpsc::UnboundedSender<AppEvent>,
) -> (LlmOutcome, Vec<Message>) {
    if !approved {
        return (
            LlmOutcome::Complete {
                assistant_message: "Operation cancelled.".into(),
                tool_results: vec![],
            },
            conversation,
        );
    }

    let base_path = match &working_dir {
        Some(p) => p.clone(),
        None => {
            return (
                LlmOutcome::Error {
                    message: "No working directory set.".into(),
                },
                conversation,
            );
        }
    };

    let call = FunctionCall {
        name: pending.tool_name.clone(),
        arguments: pending.arguments,
    };

    let result_str = match execute_tool(&call, &base_path).await {
        Ok(s) => s,
        Err(e) => format!("Error: {e}"),
    };

    conversation.push(Message::tool_result(
        &pending.tool_name,
        &result_str,
        pending.tool_call_id,
    ));

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
        Some((request_id, &tx)),
    )
    .await
}
