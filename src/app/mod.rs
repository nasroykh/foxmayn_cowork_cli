mod agentic;
mod entry;
mod events;
mod state;
mod system_prompt;

// Re-export everything that `tui/mod.rs` and `main.rs` reference via `crate::app::*`.

pub use entry::{confirm_tool, confirm_tool_streaming, send_message, send_message_streaming};
pub use events::{AppEvent, LlmOutcome};
pub use state::{App, ChatEntry, ChatRole, InputMode, Panel};
