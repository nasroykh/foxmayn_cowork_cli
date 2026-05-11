use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use tokio::task::JoinHandle;

use crate::config::{Config, ThinkingDisplay};
use crate::error::AppError;
use crate::fs::FileEntry;
use crate::llm::runtime::LlmRuntime;
use crate::llm::types::StreamChunk;
use crate::storage::Storage;

use super::events::{LlmOutcome, RequestId};

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
    pub(crate) fn from_file_entry(entry: &FileEntry, depth: usize) -> Self {
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

// ── Slash picker ─────────────────────────────────────────────────────────────

/// A single selectable item inside the interactive slash picker.
pub struct SlashPickerItem {
    /// Text shown in the popup row.
    pub display: String,
    /// Value substituted as the command argument on selection.
    pub value: String,
}

/// Interactive picker state — shown instead of the completions popup when a
/// picker-capable command (e.g. `/resume`) is entered without an argument.
pub struct SlashPicker {
    /// The command that owns this picker (e.g. `"/resume"`).
    pub command: &'static str,
    pub items: Vec<SlashPickerItem>,
    pub selected: usize,
}

impl SlashPicker {
    pub fn select_prev(&mut self) {
        if self.items.is_empty() {
            return;
        }
        self.selected = self.selected.checked_sub(1).unwrap_or(self.items.len() - 1);
    }

    pub fn select_next(&mut self) {
        if self.items.is_empty() {
            return;
        }
        self.selected = (self.selected + 1) % self.items.len();
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

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum ChatRole {
    User,
    Assistant,
    Tool,
    /// Permanent dimmed entry used by `ThinkingDisplay::Full` to keep reasoning in the transcript.
    Thinking,
    Error,
    Warning,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
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

// ── App ───────────────────────────────────────────────────────────────────────

pub struct App {
    pub config: Arc<Config>,
    pub llm_runtime: LlmRuntime,
    pub conversation: Vec<crate::llm::types::Message>,
    pub working_dir: Option<PathBuf>,
    pub chat_messages: Vec<ChatEntry>,
    pub file_tree: Vec<TreeEntry>,
    pub input_mode: InputMode,
    pub pending_confirmation: Option<PendingToolCall>,
    pub health_status: bool,
    pub is_loading: bool,
    /// Accumulates streaming text while a response is in progress. `None` when idle.
    pub streaming_text: Option<String>,
    /// Accumulates streamed reasoning / thinking text for the current round. Rendered or
    /// finalized based on `Config::thinking_display`. `None` when no reasoning has arrived.
    pub thinking_text: Option<String>,
    pub chat_scroll: usize,
    pub file_tree_scroll: usize,
    pub focused_panel: Panel,
    pub should_quit: bool,
    /// Paths of directories that should be re-expanded after the next root
    /// file-tree reload. Populated by `prepare_refresh`, drained by the TUI
    /// event handler as `FileTreeLoaded` / `SubdirLoaded` events arrive.
    pub pending_expansions: HashSet<String>,
    /// Path of the entry to scroll back to after the next root reload.
    pub pending_scroll_path: Option<String>,
    /// Handle of the currently in-flight LLM request (if any), so the user
    /// can cancel it with Esc. Cleared on `StreamComplete` / `LlmResponse`.
    pub current_request: Option<JoinHandle<()>>,
    /// Monotonic request generation. LLM events carry this id so stale events
    /// from cancelled or superseded tasks can be ignored safely.
    pub active_request_id: Option<RequestId>,
    next_request_id: RequestId,
    /// Indices into `tui::commands::COMMANDS` that match the current slash prefix. Empty when
    /// the input does not start with `/` or no commands match.
    pub slash_completions: Vec<usize>,
    /// Which entry in `slash_completions` is currently highlighted.
    pub slash_selected: usize,
    /// Persistent storage (global settings DB + current project DB).
    pub storage: Storage,
    /// Row id of the current open session in the project DB. `None` until the first
    /// completed exchange (session is created lazily with the first user message as title).
    pub current_session_id: Option<i64>,
    /// Active interactive picker (e.g. session list for `/resume`). Mutually exclusive
    /// with `slash_completions` — when this is `Some`, the picker popup is rendered instead.
    pub slash_picker: Option<SlashPicker>,
}

impl App {
    pub fn new(config: Arc<Config>, llm_runtime: LlmRuntime, storage: Storage) -> Self {
        Self {
            config,
            llm_runtime,
            conversation: Vec::new(),
            working_dir: None,
            chat_messages: Vec::new(),
            file_tree: Vec::new(),
            input_mode: InputMode::Editing,
            pending_confirmation: None,
            health_status: false,
            is_loading: false,
            streaming_text: None,
            thinking_text: None,
            chat_scroll: 0,
            file_tree_scroll: 0,
            focused_panel: Panel::Chat,
            should_quit: false,
            pending_expansions: HashSet::new(),
            pending_scroll_path: None,
            current_request: None,
            active_request_id: None,
            next_request_id: 1,
            slash_completions: Vec::new(),
            slash_selected: 0,
            storage,
            current_session_id: None,
            slash_picker: None,
        }
    }

    /// Recompute slash completions from the current textarea content.
    /// Call this after every keystroke while in Editing mode.
    pub fn update_slash_completions(&mut self, input: &str) {
        let first_line = input.lines().next().unwrap_or(input);
        if first_line.starts_with('/') {
            // Match only on the command word (before any space / argument)
            let word = first_line.split_whitespace().next().unwrap_or(first_line);
            let matches = crate::tui::commands::match_commands(word);
            // Keep the selected index clamped; reset to 0 only when the list changes
            if matches != self.slash_completions {
                self.slash_selected = 0;
            }
            self.slash_completions = matches;
        } else {
            self.slash_completions.clear();
            self.slash_selected = 0;
        }
    }

    pub fn slash_select_prev(&mut self) {
        if self.slash_completions.is_empty() {
            return;
        }
        self.slash_selected = self
            .slash_selected
            .checked_sub(1)
            .unwrap_or(self.slash_completions.len() - 1);
    }

    pub fn slash_select_next(&mut self) {
        if self.slash_completions.is_empty() {
            return;
        }
        self.slash_selected = (self.slash_selected + 1) % self.slash_completions.len();
    }

    pub fn allocate_request_id(&mut self) -> RequestId {
        let id = self.next_request_id;
        self.next_request_id = self.next_request_id.saturating_add(1);
        self.active_request_id = Some(id);
        id
    }

    pub fn is_active_request(&self, request_id: RequestId) -> bool {
        self.active_request_id == Some(request_id)
    }

    /// Snapshot expansion + scroll state into `pending_*` so they can be
    /// restored after the file tree is rebuilt by the next reload. Call this
    /// before issuing an auto-refresh that would otherwise wipe expansion.
    pub fn prepare_refresh(&mut self) {
        self.pending_expansions = self
            .file_tree
            .iter()
            .filter(|e| e.is_dir && e.expanded)
            .map(|e| e.path.clone())
            .collect();
        self.pending_scroll_path = self
            .file_tree
            .get(self.file_tree_scroll)
            .map(|e| e.path.clone());
    }

    /// Mark `path` expanded if it exists in the tree and isn't already.
    /// Returns true if the caller should spawn a subdir load for it.
    pub fn mark_expanded(&mut self, path: &str) -> bool {
        if let Some(idx) = self.file_tree.iter().position(|e| e.path == path)
            && self.file_tree[idx].is_dir
            && !self.file_tree[idx].expanded
        {
            self.file_tree[idx].expanded = true;
            return true;
        }
        false
    }

    /// Walk the current tree and return paths still pending expansion that
    /// now exist as un-expanded directory entries. Caller is expected to
    /// `mark_expanded` each one and spawn the corresponding subdir load.
    pub fn drain_ready_pending_expansions(&mut self) -> Vec<String> {
        if self.pending_expansions.is_empty() {
            return Vec::new();
        }
        let ready: Vec<String> = self
            .file_tree
            .iter()
            .filter(|e| e.is_dir && !e.expanded && self.pending_expansions.contains(&e.path))
            .map(|e| e.path.clone())
            .collect();
        for p in &ready {
            self.pending_expansions.remove(p);
        }
        ready
    }

    /// Move scroll back to `pending_scroll_path` if it still exists in the
    /// tree. Cleared after a single successful application.
    pub fn restore_pending_scroll(&mut self) {
        let Some(path) = self.pending_scroll_path.clone() else {
            return;
        };
        if let Some(idx) = self.file_tree.iter().position(|e| e.path == path) {
            self.file_tree_scroll = idx;
            self.pending_scroll_path = None;
        }
    }

    /// Abort the in-flight LLM request silently (no chat entry). Used when a new
    /// request supersedes an old one so the stale task stops consuming API quota.
    pub fn abort_in_flight(&mut self) {
        if let Some(h) = self.current_request.take() {
            h.abort();
            self.active_request_id = None;
            self.is_loading = false;
            self.streaming_text = None;
            self.thinking_text = None;
        }
    }

    /// Abort the in-flight LLM request (if any) and reset transient UI state
    /// so the user can immediately type a new prompt.
    pub fn cancel_request(&mut self) -> bool {
        let Some(handle) = self.current_request.take() else {
            return false;
        };
        handle.abort();
        self.active_request_id = None;
        self.is_loading = false;
        self.streaming_text = None;
        self.thinking_text = None;
        self.input_mode = InputMode::Editing;
        self.pending_confirmation = None;
        self.chat_messages.push(ChatEntry {
            role: ChatRole::Warning,
            content: "Request cancelled.".into(),
        });
        true
    }

    /// Call before spawning send_message task: records user message in display and sets loading.
    pub fn begin_send(&mut self, text: &str) {
        self.chat_messages.push(ChatEntry {
            role: ChatRole::User,
            content: text.to_owned(),
        });
        self.is_loading = true;
    }

    /// Append a streamed content / thinking delta and auto-scroll to bottom.
    /// Thinking deltas are dropped when `ThinkingDisplay::Off` so we never carry useless state.
    pub fn handle_stream_chunk(&mut self, chunk: &StreamChunk) {
        match chunk {
            StreamChunk::ContentDelta(text) => {
                let buf = self.streaming_text.get_or_insert_with(String::new);
                buf.push_str(text);
                self.chat_scroll = 0;
            }
            StreamChunk::ThinkingDelta(text) => {
                if matches!(self.config.thinking_display, ThinkingDisplay::Off) {
                    return;
                }
                let buf = self.thinking_text.get_or_insert_with(String::new);
                buf.push_str(text);
                self.chat_scroll = 0;
            }
            _ => {}
        }
    }

    /// Promote / discard the thinking buffer for the round that just finished, based on
    /// `Config::thinking_display`. Called whenever the live response is sealed (round end,
    /// stream complete, or non-streaming outcome). Idempotent — safe to call repeatedly.
    pub fn finalize_thinking_for_round(&mut self) {
        let Some(text) = self.thinking_text.take() else {
            return;
        };
        if !matches!(self.config.thinking_display, ThinkingDisplay::Full) {
            return;
        }
        let trimmed = text.trim();
        if trimmed.is_empty() {
            return;
        }
        self.chat_messages.push(ChatEntry {
            role: ChatRole::Thinking,
            content: trimmed.to_string(),
        });
    }

    /// Clear the streaming buffer (called when StreamComplete arrives).
    pub fn finalize_stream(&mut self) {
        self.streaming_text = None;
        self.finalize_thinking_for_round();
    }

    /// Apply the result of a completed send_message or confirm_tool task.
    pub fn handle_outcome(
        &mut self,
        outcome: LlmOutcome,
        updated_conversation: Vec<crate::llm::types::Message>,
    ) {
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
        self.abort_in_flight();
        // Open (or create) the per-project DB before moving `path`.
        self.storage.open_project(&path);
        self.current_session_id = None;
        self.working_dir = Some(path);
        self.conversation.clear();
        self.chat_messages.clear();
        self.pending_confirmation = None;
        self.input_mode = InputMode::Editing;
        self.file_tree_scroll = 0;
        self.chat_scroll = 0;
        // Drop any expansion/scroll memory from the previous working dir.
        self.pending_expansions.clear();
        self.pending_scroll_path = None;
    }

    pub fn clear_conversation(&mut self) {
        self.conversation.clear();
        self.chat_messages.clear();
        self.pending_confirmation = None;
        self.streaming_text = None;
        self.thinking_text = None;
        self.input_mode = InputMode::Editing;
        self.current_session_id = None;
    }

    /// Persist the current conversation to the project DB. Creates the session row on the first
    /// call (using the first user message as the title). Silently no-ops when no project is open.
    pub fn save_current_session(&mut self) {
        if self.storage.project.is_none() || self.conversation.is_empty() {
            return;
        }
        let conv_json = match serde_json::to_string(&self.conversation) {
            Ok(s) => s,
            Err(_) => return,
        };
        let chat_json = match serde_json::to_string(&self.chat_messages) {
            Ok(s) => s,
            Err(_) => return,
        };
        if self.current_session_id.is_none() {
            let title: String = self
                .conversation
                .iter()
                .find(|m| m.role == "user")
                .map(|m| m.content.chars().take(60).collect())
                .unwrap_or_else(|| "Untitled".to_string());
            if let Some(project) = self.storage.project.as_ref() {
                match project.create_session(&title) {
                    Ok(id) => self.current_session_id = Some(id),
                    Err(_) => return,
                }
            }
        }
        if let (Some(id), Some(project)) = (self.current_session_id, self.storage.project.as_ref())
        {
            let _ = project.save_session(id, &conv_json, &chat_json);
        }
    }

    /// Return recent sessions for the current project (up to 20, newest first).
    pub fn list_sessions(&self) -> Vec<crate::storage::SessionSummary> {
        self.storage
            .project
            .as_ref()
            .and_then(|p| p.list_sessions().ok())
            .unwrap_or_default()
    }

    /// Load a past session into memory. Returns `false` if the id is not found or no project
    /// is open. The loaded session becomes the active one (subsequent saves update it).
    pub fn resume_session(&mut self, id: i64) -> bool {
        let Some(project) = self.storage.project.as_ref() else {
            return false;
        };
        let Some((conv_json, chat_json)) = project.load_session(id).ok().flatten() else {
            return false;
        };
        let Ok(conv) = serde_json::from_str(&conv_json) else {
            return false;
        };
        let Ok(msgs) = serde_json::from_str::<Vec<ChatEntry>>(&chat_json) else {
            return false;
        };
        self.conversation = conv;
        self.chat_messages = msgs;
        self.current_session_id = Some(id);
        self.chat_scroll = 0;
        true
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
