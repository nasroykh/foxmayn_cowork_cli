use crate::error::AppError;
use crate::fs::FileEntry;
use crate::llm::types::{Message, StreamChunk};

pub type RequestId = u64;

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
    LlmResponse {
        request_id: Option<RequestId>,
        outcome: LlmOutcome,
        conversation: Vec<Message>,
    },
    HealthCheckResult(bool),
    FileTreeLoaded(Result<Vec<FileEntry>, AppError>),
    SubdirLoaded {
        parent_path: String,
        result: Result<Vec<FileEntry>, AppError>,
    },
    /// A single streamed content delta — forwarded live during streaming.
    StreamChunk {
        request_id: RequestId,
        chunk: StreamChunk,
    },
    /// Streaming round complete — carry the final outcome and updated conversation.
    StreamComplete {
        request_id: RequestId,
        outcome: LlmOutcome,
        conversation: Vec<Message>,
    },
    /// Near-context-limit warning to display in chat before proceeding.
    ContextWarning(String),
    /// Available models fetched asynchronously (Ollama /api/tags). Opens the model picker.
    ModelsLoaded(Vec<String>),
    /// Intermediate assistant text from a multi-round agentic loop. Flushes any
    /// in-progress streaming buffer to a permanent chat entry so subsequent
    /// rounds start fresh.
    IntermediateAssistant {
        request_id: RequestId,
        content: String,
    },
    /// A tool call that ran during the agentic loop — surfaced live so the user
    /// can see which operations the AI performed.
    IntermediateTool {
        request_id: RequestId,
        name: String,
        result: String,
    },
}
