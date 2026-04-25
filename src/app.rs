use std::path::{Path, PathBuf};

use tokio::sync::mpsc;

use crate::config::Config;
use crate::error::AppError;
use crate::fs::FileEntry;
use crate::llm;
use crate::llm::tools::{ToolCallResult, dispatch_tool_call, execute_tool, tool_definitions};
use crate::llm::types::{ChatRequest, FunctionCall, Message, StreamChunk, ToolCall};

// ── File tree display type ────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct TreeEntry {
    pub name: String,
    pub path: String,
    pub is_dir: bool,
    #[expect(dead_code)]
    pub size: u64,
    pub depth: usize,
    pub expanded: bool,
}

impl TreeEntry {
    fn from_file_entry(entry: &FileEntry, depth: usize) -> Self {
        Self {
            name: entry.name.clone(),
            path: entry.path.clone(),
            is_dir: entry.is_dir,
            size: entry.size,
            depth,
            expanded: false,
        }
    }
}

// ── Input / focus state ──────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq)]
pub enum InputMode {
    Editing,
    Confirming,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Panel {
    FileTree,
    Chat,
}

// ── Display types ─────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq)]
pub enum ChatRole {
    User,
    Assistant,
    Tool,
    Error,
    Warning,
}

#[derive(Debug, Clone)]
pub struct ChatEntry {
    pub role: ChatRole,
    pub content: String,
}

#[derive(Debug, Clone)]
pub struct PendingToolCall {
    pub description: String,
    pub tool_name: String,
    pub arguments: serde_json::Value,
    pub tool_call_id: Option<String>,
    #[expect(dead_code)]
    pub user_message: String,
}

// ── Outcome returned by async tasks ──────────────────────────────────────────

#[derive(Debug)]
pub enum LlmOutcome {
    Complete {
        assistant_message: String,
        tool_results: Vec<String>,
    },
    PendingConfirmation {
        description: String,
        tool_name: String,
        arguments: serde_json::Value,
        tool_call_id: Option<String>,
        user_message: String,
    },
    Error {
        message: String,
    },
}

// ── Events sent from async tasks to the TUI event loop ───────────────────────

pub enum AppEvent {
    /// Result of send_message or confirm_tool: (outcome, updated conversation)
    LlmResponse(LlmOutcome, Vec<Message>),
    HealthCheckResult(bool),
    FileTreeLoaded(Result<Vec<FileEntry>, AppError>),
    SubdirLoaded {
        parent_path: String,
        result: Result<Vec<FileEntry>, AppError>,
    },
    /// A single streamed content delta — forwarded live during streaming.
    StreamChunk(StreamChunk),
    /// Streaming round complete — carry the final outcome and updated conversation.
    StreamComplete(LlmOutcome, Vec<Message>),
    /// Near-context-limit warning to display in chat before proceeding.
    ContextWarning(String),
}

// ── App ───────────────────────────────────────────────────────────────────────

pub struct App {
    pub config: Config,
    pub http_client: reqwest::Client,
    pub conversation: Vec<Message>,
    pub working_dir: Option<PathBuf>,
    pub chat_messages: Vec<ChatEntry>,
    pub file_tree: Vec<TreeEntry>,
    pub input_mode: InputMode,
    pub pending_confirmation: Option<PendingToolCall>,
    pub health_status: bool,
    pub is_loading: bool,
    /// Accumulates streaming text while a response is in progress. `None` when idle.
    pub streaming_text: Option<String>,
    pub chat_scroll: usize,
    pub file_tree_scroll: usize,
    pub focused_panel: Panel,
    pub should_quit: bool,
}

impl App {
    pub fn new(config: Config) -> Self {
        Self {
            config,
            http_client: reqwest::Client::new(),
            conversation: Vec::new(),
            working_dir: None,
            chat_messages: Vec::new(),
            file_tree: Vec::new(),
            input_mode: InputMode::Editing,
            pending_confirmation: None,
            health_status: false,
            is_loading: false,
            streaming_text: None,
            chat_scroll: 0,
            file_tree_scroll: 0,
            focused_panel: Panel::Chat,
            should_quit: false,
        }
    }

    /// Call before spawning send_message task: records user message in display and sets loading.
    pub fn begin_send(&mut self, text: &str) {
        self.chat_messages.push(ChatEntry {
            role: ChatRole::User,
            content: text.to_owned(),
        });
        self.is_loading = true;
    }

    /// Append a streamed content delta and auto-scroll to bottom.
    pub fn handle_stream_chunk(&mut self, chunk: &StreamChunk) {
        if let StreamChunk::ContentDelta(text) = chunk {
            let buf = self.streaming_text.get_or_insert_with(String::new);
            buf.push_str(text);
            self.chat_scroll = 0;
        }
    }

    /// Clear the streaming buffer (called when StreamComplete arrives).
    pub fn finalize_stream(&mut self) {
        self.streaming_text = None;
    }

    /// Apply the result of a completed send_message or confirm_tool task.
    pub fn handle_outcome(&mut self, outcome: LlmOutcome, updated_conversation: Vec<Message>) {
        self.conversation = updated_conversation;
        self.is_loading = false;

        match outcome {
            LlmOutcome::Complete {
                assistant_message,
                tool_results,
            } => {
                if !assistant_message.trim().is_empty() {
                    self.chat_messages.push(ChatEntry {
                        role: ChatRole::Assistant,
                        content: assistant_message,
                    });
                }
                for r in tool_results {
                    self.chat_messages.push(ChatEntry {
                        role: ChatRole::Tool,
                        content: r,
                    });
                }
                self.input_mode = InputMode::Editing;
                self.pending_confirmation = None;
            }
            LlmOutcome::PendingConfirmation {
                description,
                tool_name,
                arguments,
                tool_call_id,
                user_message,
            } => {
                self.pending_confirmation = Some(PendingToolCall {
                    description,
                    tool_name,
                    arguments,
                    tool_call_id,
                    user_message,
                });
                self.input_mode = InputMode::Confirming;
            }
            LlmOutcome::Error { message } => {
                self.chat_messages.push(ChatEntry {
                    role: ChatRole::Error,
                    content: message,
                });
                self.input_mode = InputMode::Editing;
                self.pending_confirmation = None;
            }
        }
    }

    pub fn handle_health(&mut self, ok: bool) {
        self.health_status = ok;
    }

    pub fn handle_file_tree(&mut self, result: Result<Vec<FileEntry>, AppError>) {
        match result {
            Ok(entries) => {
                self.file_tree = entries
                    .iter()
                    .map(|e| TreeEntry::from_file_entry(e, 0))
                    .collect();
                self.file_tree_scroll = 0;
            }
            Err(e) => {
                self.chat_messages.push(ChatEntry {
                    role: ChatRole::Error,
                    content: format!("Failed to load directory: {e}"),
                });
            }
        }
    }

    /// Insert children loaded for a directory into the flat tree vec.
    pub fn handle_subdir_loaded(
        &mut self,
        parent_path: String,
        result: Result<Vec<FileEntry>, AppError>,
    ) {
        let Some(parent_idx) = self
            .file_tree
            .iter()
            .position(|e| e.path == parent_path && e.is_dir)
        else {
            return;
        };

        if !self.file_tree[parent_idx].expanded {
            return;
        }

        match result {
            Ok(entries) => {
                let parent_depth = self.file_tree[parent_idx].depth;
                let child_depth = parent_depth + 1;
                let children: Vec<TreeEntry> = entries
                    .iter()
                    .map(|e| TreeEntry::from_file_entry(e, child_depth))
                    .collect();

                let insert_at = parent_idx + 1;
                // Remove any stale children already in the vec
                let remove_end = self.file_tree[insert_at..]
                    .iter()
                    .position(|e| e.depth <= parent_depth)
                    .map(|pos| insert_at + pos)
                    .unwrap_or(self.file_tree.len());
                self.file_tree.drain(insert_at..remove_end);

                for (i, child) in children.into_iter().enumerate() {
                    self.file_tree.insert(insert_at + i, child);
                }
            }
            Err(e) => {
                self.file_tree[parent_idx].expanded = false;
                self.chat_messages.push(ChatEntry {
                    role: ChatRole::Error,
                    content: format!("Failed to load directory: {e}"),
                });
            }
        }
    }

    /// Toggle expand/collapse on the currently selected directory.
    /// Returns `Some(path)` when the directory needs its children loaded.
    pub fn toggle_expand(&mut self) -> Option<String> {
        let idx = self.file_tree_scroll;
        let (is_dir, is_expanded, path) = self
            .file_tree
            .get(idx)
            .map(|e| (e.is_dir, e.expanded, e.path.clone()))?;

        if !is_dir {
            return None;
        }
        if is_expanded {
            self.collapse_dir(idx);
            None
        } else {
            self.file_tree[idx].expanded = true;
            Some(path)
        }
    }

    /// Collapse a directory at `idx`, removing all of its children from the vec.
    pub fn collapse_dir(&mut self, idx: usize) {
        let depth = match self.file_tree.get(idx) {
            Some(e) => e.depth,
            None => return,
        };
        self.file_tree[idx].expanded = false;

        let remove_start = idx + 1;
        let remove_end = self.file_tree[remove_start..]
            .iter()
            .position(|e| e.depth <= depth)
            .map(|pos| remove_start + pos)
            .unwrap_or(self.file_tree.len());
        self.file_tree.drain(remove_start..remove_end);

        // Clamp scroll so it doesn't point past the end
        let max_idx = self.file_tree.len().saturating_sub(1);
        self.file_tree_scroll = self.file_tree_scroll.min(max_idx);
    }

    /// Move selection to the parent directory entry of the currently selected item.
    pub fn jump_to_parent(&mut self) {
        let idx = self.file_tree_scroll;
        let depth = match self.file_tree.get(idx) {
            Some(e) if e.depth > 0 => e.depth,
            _ => return,
        };
        let parent_depth = depth - 1;
        for i in (0..idx).rev() {
            if self.file_tree[i].depth == parent_depth && self.file_tree[i].is_dir {
                self.file_tree_scroll = i;
                return;
            }
        }
    }

    pub fn set_working_dir(&mut self, path: PathBuf) {
        self.working_dir = Some(path);
        self.conversation.clear();
        self.chat_messages.clear();
        self.pending_confirmation = None;
        self.input_mode = InputMode::Editing;
        self.file_tree_scroll = 0;
        self.chat_scroll = 0;
    }

    pub fn clear_conversation(&mut self) {
        self.conversation.clear();
        self.chat_messages.clear();
        self.pending_confirmation = None;
        self.input_mode = InputMode::Editing;
    }

    pub fn scroll_chat_up(&mut self) {
        // Increase offset from bottom → reveals older messages
        self.chat_scroll = self.chat_scroll.saturating_add(3);
    }

    pub fn scroll_chat_down(&mut self) {
        // Decrease offset from bottom → moves toward newest messages
        self.chat_scroll = self.chat_scroll.saturating_sub(3);
    }

    pub fn scroll_tree_up(&mut self) {
        self.file_tree_scroll = self.file_tree_scroll.saturating_sub(1);
    }

    pub fn scroll_tree_down(&mut self) {
        self.file_tree_scroll = self
            .file_tree_scroll
            .saturating_add(1)
            .min(self.file_tree.len().saturating_sub(1));
    }
}

// ── Async task functions ──────────────────────────────────────────────────────
// These take owned/cloned values so they can be spawned with tokio::spawn.
// They return (LlmOutcome, updated_conversation) so the event loop can apply
// the result back to App without any shared mutable state.

// ── Streaming helpers ─────────────────────────────────────────────────────────

struct PartialToolCall {
    id: Option<String>,
    name: Option<String>,
    arguments: String,
}

/// If `user_message_for_pending` is non-empty, it is used in `LlmOutcome::PendingConfirmation`
/// (new user turn from `send_message`). Otherwise, the most recent `user` turn in
/// `conversation` is used (e.g. after a confirmed tool).
fn last_user_for_pending(
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

/// LLM call → (optional) non-destructive tool execution → feed back → repeat until
/// a plain-text reply, confirmation pause, or hard error.
///
/// `new_user`: the current user line when starting from `send_message` (not yet in `conversation`);
/// `None` when the transcript already contains the full user turn (e.g. right after
/// a confirmed tool).
async fn run_agentic_loop(
    client: &reqwest::Client,
    config: &Config,
    base_path: &Path,
    mut working: Vec<Message>,
    mut conversation: Vec<Message>,
    new_user: Option<Message>,
    user_message_for_pending: String,
) -> (LlmOutcome, Vec<Message>) {
    let mut user_in_conversation = new_user.is_none();
    const MAX_TOOL_ROUNDS: usize = 10;

    for _ in 0..MAX_TOOL_ROUNDS {
        let request = ChatRequest {
            model: config.model.clone(),
            messages: working.clone(),
            tools: tool_definitions(),
            stream: false,
            reasoning: config.openrouter_reasoning.clone(),
            think: config.ollama_think,
        };

        let assistant_msg = match llm::chat(client, &request, config).await {
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

        let has_tool_calls = assistant_msg
            .tool_calls
            .as_ref()
            .is_some_and(|c| !c.is_empty());
        if !has_tool_calls {
            let content = assistant_msg.content.clone();
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
                    tool_results: vec![],
                },
                conversation,
            );
        }

        let tool_calls: Vec<ToolCall> = match &assistant_msg.tool_calls {
            Some(t) if !t.is_empty() => t.clone(),
            _ => {
                return (
                    LlmOutcome::Error {
                        message: "Internal: expected non-empty tool_calls from model.".into(),
                    },
                    conversation,
                );
            }
        };

        let mut dispatched: Vec<ToolCallResult> = Vec::new();
        for tc in &tool_calls {
            let r = match dispatch_tool_call(&tc.function, base_path).await {
                Ok(r) => r,
                Err(AppError::ToolValidation(msg)) => ToolCallResult {
                    result: Some(format!(
                        "Error: {msg}. Use a relative path (e.g. 'src/file.rs') instead of an absolute path."
                    )),
                    requires_confirmation: false,
                    description: format!("Validation failed: {msg}"),
                    tool_name: tc.function.name.clone(),
                    args: tc.function.arguments.clone(),
                },
                Err(e) => {
                    return (
                        LlmOutcome::Error {
                            message: e.to_string(),
                        },
                        conversation,
                    );
                }
            };

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
        working.push(assistant_msg.clone());
        conversation.push(assistant_msg);
        for (tc, result) in tool_calls.iter().zip(dispatched.iter()) {
            let s = result.result.clone().unwrap_or_default();
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

/// Streaming variant of `run_agentic_loop`. Forwards `ContentDelta` chunks to `tx`
/// as they arrive so the TUI can render them incrementally.
#[allow(clippy::too_many_arguments)]
async fn run_agentic_loop_streaming(
    client: &reqwest::Client,
    config: &Config,
    base_path: &Path,
    mut working: Vec<Message>,
    mut conversation: Vec<Message>,
    new_user: Option<Message>,
    user_message_for_pending: String,
    tx: &mpsc::UnboundedSender<AppEvent>,
) -> (LlmOutcome, Vec<Message>) {
    let mut user_in_conversation = new_user.is_none();
    const MAX_TOOL_ROUNDS: usize = 10;

    for _ in 0..MAX_TOOL_ROUNDS {
        let request = ChatRequest {
            model: config.model.clone(),
            messages: working.clone(),
            tools: tool_definitions(),
            stream: true,
            reasoning: config.openrouter_reasoning.clone(),
            think: config.ollama_think,
        };

        let (chunk_tx, mut chunk_rx) = mpsc::unbounded_channel::<StreamChunk>();

        if let Err(e) = llm::chat_stream(client, &request, config, chunk_tx).await {
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
                    let _ = tx.send(AppEvent::StreamChunk(StreamChunk::ContentDelta(text)));
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
        let assembled_tool_calls: Option<Vec<ToolCall>> = if has_tool_calls {
            Some(
                partial_tool_calls
                    .iter()
                    .map(|ptc| {
                        let arguments: serde_json::Value =
                            serde_json::from_str(&ptc.arguments).unwrap_or_default();
                        ToolCall {
                            id: ptc.id.clone(),
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

        let assistant_msg = Message {
            role: "assistant".into(),
            content: content.clone(),
            tool_calls: assembled_tool_calls.clone(),
            name: None,
            tool_call_id: None,
        };

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
                    tool_results: vec![],
                },
                conversation,
            );
        }

        let tool_calls = assembled_tool_calls.unwrap_or_default();
        let mut dispatched: Vec<ToolCallResult> = Vec::new();

        for tc in &tool_calls {
            let r = match dispatch_tool_call(&tc.function, base_path).await {
                Ok(r) => r,
                Err(AppError::ToolValidation(msg)) => ToolCallResult {
                    result: Some(format!(
                        "Error: {msg}. Use a relative path (e.g. 'src/file.rs') instead of an absolute path."
                    )),
                    requires_confirmation: false,
                    description: format!("Validation failed: {msg}"),
                    tool_name: tc.function.name.clone(),
                    args: tc.function.arguments.clone(),
                },
                Err(e) => {
                    return (
                        LlmOutcome::Error {
                            message: e.to_string(),
                        },
                        conversation,
                    );
                }
            };

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
        working.push(assistant_msg.clone());
        conversation.push(assistant_msg);
        for (tc, result) in tool_calls.iter().zip(dispatched.iter()) {
            let s = result.result.clone().unwrap_or_default();
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

pub async fn send_message(
    client: reqwest::Client,
    config: Config,
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
    let mut working: Vec<Message> = vec![Message::system(system_prompt(&working_dir))];
    working.extend(conversation.iter().cloned());
    working.push(user_msg.clone());

    run_agentic_loop(
        &client,
        &config,
        base_path.as_path(),
        working,
        conversation,
        Some(user_msg),
        user_message,
    )
    .await
}

pub async fn confirm_tool(
    client: reqwest::Client,
    config: Config,
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

    let mut working: Vec<Message> = vec![Message::system(system_prompt(&working_dir))];
    working.extend(conversation.iter().cloned());

    run_agentic_loop(
        &client,
        &config,
        base_path.as_path(),
        working,
        conversation,
        None,
        String::new(),
    )
    .await
}

pub async fn send_message_streaming(
    client: reqwest::Client,
    config: Config,
    conversation: Vec<Message>,
    working_dir: Option<PathBuf>,
    user_message: String,
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
    let mut working: Vec<Message> = vec![Message::system(system_prompt(&working_dir))];
    working.extend(conversation.iter().cloned());
    working.push(user_msg.clone());

    run_agentic_loop_streaming(
        &client,
        &config,
        base_path.as_path(),
        working,
        conversation,
        Some(user_msg),
        user_message,
        &tx,
    )
    .await
}

pub async fn confirm_tool_streaming(
    client: reqwest::Client,
    config: Config,
    working_dir: Option<PathBuf>,
    mut conversation: Vec<Message>,
    pending: PendingToolCall,
    approved: bool,
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

    let mut working: Vec<Message> = vec![Message::system(system_prompt(&working_dir))];
    working.extend(conversation.iter().cloned());

    run_agentic_loop_streaming(
        &client,
        &config,
        base_path.as_path(),
        working,
        conversation,
        None,
        String::new(),
        &tx,
    )
    .await
}

// ── System prompt ─────────────────────────────────────────────────────────────

fn system_prompt(working_dir: &Option<PathBuf>) -> String {
    let dir = working_dir
        .as_ref()
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|| "(none selected)".into());

    format!(
        "You are a file operations assistant. Help users manage files and directories using the available tools.\n\
        Always use tools for actual operations — never fabricate file contents or listings.\n\
        After performing an operation, briefly confirm what you did.\n\
        \n\
        Working directory: {dir}\n\
        \n\
        IMPORTANT PATH RULES:\n\
        - Prefer relative paths (e.g. 'src/main.rs', 'README.md') — they are resolved against the working directory automatically.\n\
        - If you use absolute paths, they MUST start with exactly: {dir}\n\
        - Never construct absolute paths from memory — use relative paths to avoid errors.\n\
        - If a tool returns a path error, retry using a relative path instead.\n\
        \n\
        EDITING FILES:\n\
        - For small targeted changes, prefer `patch_file` (search & replace a unique string) over `edit_file` (full overwrite).\n\
        - `patch_file` fails if the search text appears 0 or more than once — fall back to `edit_file` in that case.\n\
        \n\
        MULTI-STEP OPERATIONS:\n\
        - For tasks that affect multiple files (e.g. 'delete all .md files'), handle them one file at a time.\n\
        - Start by calling list_files to find the relevant files, then act on the first one.\n\
        - After each operation completes, the user will ask you to continue if there are more.\n\
        - Never try to batch multiple destructive operations in a single response."
    )
}
