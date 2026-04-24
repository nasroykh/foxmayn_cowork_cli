# Plan: Complete MVP to 100%

## Context

The MVP_REPORT.md identified 7 gaps preventing MVP completion. This plan implements all of them in dependency order across 6 phases. Each phase produces a compilable, testable increment.

**Gaps addressed (from MVP_REPORT.md):**

1. File tree not refreshed after AI operations — **High**
2. `edit_file` overwrites entire file (no patch) — **Medium**
3. No `create_directory` tool — **Medium**
4. File tree is single-level only — **Medium**
5. No streaming (`stream: false` hardcoded) — **Medium**
6. No token / context-window management — **Low-Medium**
7. No `copy_file` or `search_in_files` — **Low**

---

## Phase 1: Auto-refresh file tree after LLM responses

**Files:** `src/tui/mod.rs`

The smallest and highest-impact fix. After any `LlmOutcome::Complete`, re-run `spawn_file_tree_load`.

1. Change `handle_app_event` signature to accept `tx: &UnboundedSender<AppEvent>` (currently takes only `event` and `app`).
2. Update the call site in `run_loop` (line 75) to pass `&tx`.
3. In the `AppEvent::LlmResponse` match arm, check the outcome variant _before_ passing it to `handle_outcome` (which moves it):
   ```rust
   AppEvent::LlmResponse(outcome, conversation) => {
       let should_refresh = matches!(outcome, LlmOutcome::Complete { .. });
       app.handle_outcome(outcome, conversation);
       if should_refresh {
           if let Some(dir) = app.working_dir.clone() {
               spawn_file_tree_load(dir, tx.clone());
           }
       }
   }
   ```

**Verify:** `cargo check`, then run the app, ask the AI to create a file, confirm the tree updates.

---

## Phase 2: New tools — `create_directory`, `copy_file`, `search_in_files`

**Files:** `Cargo.toml`, `src/fs.rs`, `src/llm/tools.rs`

Each tool follows the existing 6-step pattern: (1) `tool_definitions()`, (2) `known_tools` array, (3) `DESTRUCTIVE_OPS` if needed, (4) `execute_tool()` match arm, (5) `build_description()` match arm, (6) `fs.rs` function.

### 2a. Add `regex` dependency

```toml
regex = "1"
```

### 2b. `create_directory` (non-destructive)

- **`src/fs.rs`**: `pub async fn create_directory(path: String) -> Result<(), AppError>` — calls `fs::create_dir_all`. Fails with `ToolValidation` if path already exists.
- **`src/llm/tools.rs`**: Tool def with param `{ "path": string }`. Add to `known_tools`. Match arm in `execute_tool` and `build_description`. NOT in `DESTRUCTIVE_OPS`.

### 2c. `copy_file` (non-destructive)

- **`src/fs.rs`**: `pub async fn copy_file(source: String, destination: String) -> Result<(), AppError>` — fails if destination exists, creates parent dirs, calls `fs::copy`.
- **`src/llm/tools.rs`**: Tool def with params `{ "source": string, "destination": string }`. Add to `known_tools`. NOT in `DESTRUCTIVE_OPS`.

### 2d. `search_in_files` (non-destructive, read-only)

- **`src/fs.rs`**: `pub async fn search_in_files(dir: String, pattern: String, max_results: usize) -> Result<String, AppError>` — iterative directory walk (BFS using a `Vec<String>` work queue to avoid async recursion/Pin), regex match per line, returns formatted `file:line: content` output. Caps at `max_results` (50). Skips directories starting with `.` and `node_modules`/`target`.
- **`src/llm/tools.rs`**: Tool def with params `{ "path": string, "pattern": string }`. Add to `known_tools`. Hardcode max_results=50 in `execute_tool`.

**Verify:** `cargo clippy -- -D warnings`, `cargo test`.

---

## Phase 3: `patch_file` tool (search-and-replace)

**Files:** `src/fs.rs`, `src/llm/tools.rs`, `src/app.rs` (system prompt)

### 3a. `patch_file` fs function

```rust
pub async fn patch_file(path: String, search: String, replace: String) -> Result<(), AppError>
```

Reads file, counts occurrences of `search`. Fails on 0 matches ("not found") or >1 matches ("ambiguous: found N occurrences"). On exactly 1 match, performs `content.replacen(&search, &replace, 1)` and writes back.

### 3b. Tool registration

- Add to `tool_definitions()` with params `{ "path": string, "search": string, "replace": string }`
- Add `"patch_file"` to `DESTRUCTIVE_OPS` (requires confirmation since it modifies file content)
- Add to `known_tools`, `execute_tool`, `build_description`

### 3c. Update system prompt

In `src/app.rs` `system_prompt()`, add a line encouraging the LLM to prefer `patch_file` over `edit_file` for small changes:

```
- For small changes to existing files, prefer `patch_file` (search & replace) over `edit_file` (full overwrite).
```

**Verify:** `cargo clippy -- -D warnings`, run the app and ask AI to make a small edit, confirm it uses `patch_file` and the confirmation dialog appears.

---

## Phase 4: Expandable subdirectory navigation

**Files:** `src/app.rs`, `src/tui/mod.rs`, `src/tui/widgets/file_tree.rs`

### 4a. Data model — `TreeEntry` struct in `src/app.rs`

```rust
#[derive(Debug, Clone)]
pub struct TreeEntry {
    pub name: String,
    pub path: String,
    pub is_dir: bool,
    pub size: u64,
    pub depth: usize,      // 0 = root level
    pub expanded: bool,     // meaningful only when is_dir
}
```

Add `TreeEntry::from_file_entry(entry: &FileEntry, depth: usize) -> Self`.

### 4b. Change `App.file_tree` from `Vec<FileEntry>` to `Vec<TreeEntry>`

Update `handle_file_tree()` to convert incoming `Vec<FileEntry>` into `Vec<TreeEntry>` at depth 0 with `expanded: false`.

### 4c. New `AppEvent::SubdirLoaded`

```rust
SubdirLoaded { parent_path: String, result: Result<Vec<FileEntry>, AppError> }
```

### 4d. Tree manipulation methods on `App`

- `toggle_expand(&mut self) -> Option<String>` — if selected entry is a collapsed dir, mark expanded and return its path (to trigger load). If expanded, call `collapse_dir`. If file, return None.
- `collapse_dir(&mut self, idx: usize)` — remove all entries after `idx` whose `depth > entry.depth`. Clamp `file_tree_scroll` after drain.
- `handle_subdir_loaded(&mut self, parent_path: String, result: Result<Vec<FileEntry>, AppError>)` — find parent by path, check still expanded (race guard), convert children to `TreeEntry` at `parent.depth + 1`, insert after parent (drain existing children first for reload case).
- `jump_to_parent(&mut self)` — walk backward from current scroll to find first entry at `depth - 1`.

### 4e. New spawner in `src/tui/mod.rs`

```rust
fn spawn_subdir_load(path: String, tx: UnboundedSender<AppEvent>) {
    tokio::spawn(async move {
        let result = fs::list_files(path.clone()).await;
        let _ = tx.send(AppEvent::SubdirLoaded { parent_path: path, result });
    });
}
```

### 4f. Event handling

Add `AppEvent::SubdirLoaded` arm in `handle_app_event` → calls `app.handle_subdir_loaded(...)`.

### 4g. Keyboard shortcuts

In `InputMode::Editing` within `handle_crossterm_event`:

- **`KeyCode::Right`** when `focused_panel == FileTree` → call `toggle_expand()`, if Some(path) spawn subdir load.
- **`KeyCode::Left`** when `focused_panel == FileTree` → if selected is expanded dir, collapse. Otherwise, `jump_to_parent()`.
- **`KeyCode::Enter`** when `focused_panel == FileTree` → same as Right (toggle expand). Guard existing Enter logic with `focused_panel == Panel::Chat`.

### 4h. Widget renderer update (`src/tui/widgets/file_tree.rs`)

Update to use `TreeEntry`. Render indentation (`"  ".repeat(depth)`) and expand/collapse arrows:

- Collapsed dir: `▶ dirname`
- Expanded dir: `▼ dirname`
- File: `  filename` (aligned, no arrow)

**Verify:** `cargo check`, run the app, navigate to a directory with subdirs, press Right/Enter to expand, Left to collapse, confirm indentation renders correctly.

---

## Phase 5: Streaming responses

**Files:** `Cargo.toml`, `src/config.rs`, `src/llm/types.rs`, `src/llm/openrouter.rs`, `src/llm/ollama.rs`, `src/llm/mod.rs`, `src/app.rs`, `src/tui/mod.rs`, `src/tui/widgets/chat.rs`, `src/main.rs`

This is the largest phase. The existing non-streaming path is preserved as fallback (`STREAMING=false`).

### 5a. Dependencies and config

- Add `stream` feature to reqwest: `reqwest = { version = "0.12", features = ["json", "stream"] }`
- Add to `Config`: `pub streaming_enabled: bool` — from env var `STREAMING` (default: `true`; `false`/`0`/`off` disables)
- Add `--streaming` CLI flag to `Cli` struct in `main.rs`, wire into `Config::with_overrides`

### 5b. New types in `src/llm/types.rs`

```rust
#[derive(Debug, Clone)]
pub enum StreamChunk {
    ContentDelta(String),
    ToolCallDelta { index: usize, id: Option<String>, name: Option<String>, arguments_fragment: String },
    Done { done_reason: Option<String> },
    Error(String),
}
```

### 5c. Provider streaming functions

**`src/llm/openrouter.rs`** — `pub fn chat_stream(...) -> BoxStream<'static, StreamChunk>`

- Uses `resp.bytes_stream()` (from reqwest `stream` feature)
- SSE parser: buffer bytes, split on `\n\n`, strip `data: ` prefix, parse JSON
- Extract `choices[0].delta.content` → `ContentDelta`
- Extract `choices[0].delta.tool_calls[i]` → `ToolCallDelta`
- `data: [DONE]` → `Done`

**`src/llm/ollama.rs`** — `pub fn chat_stream(...) -> BoxStream<'static, StreamChunk>`

- NDJSON parser: buffer bytes, split on `\n`, parse each line
- `message.content` → `ContentDelta`
- Final line (`done: true`) with `message.tool_calls` → emit `ToolCallDelta` for each, then `Done`

**`src/llm/mod.rs`** — `pub fn chat_stream(...)` dispatcher.

### 5d. New `AppEvent` variants

```rust
StreamChunk(StreamChunk),
StreamComplete(LlmOutcome, Vec<Message>),
```

### 5e. Streaming state on `App`

Add `pub streaming_text: Option<String>` to `App` (init `None`). `Some(_)` = currently streaming.

Methods:

- `handle_stream_chunk(&mut self, chunk: StreamChunk)` — on `ContentDelta`, append to `streaming_text` and reset `chat_scroll` to 0.
- `finalize_stream(&mut self)` — set `streaming_text = None`.

### 5f. Streaming agentic loop in `src/app.rs`

Add `run_agentic_loop_streaming(...)` — same structure as `run_agentic_loop` but:

1. Sets `stream: true` on `ChatRequest`.
2. Calls `llm::chat_stream()` instead of `llm::chat()`.
3. Consumes stream in a loop, forwarding `ContentDelta` chunks through `chunk_tx: UnboundedSender<AppEvent>`.
4. Accumulates text and tool-call deltas in a `StreamAccumulator { content: String, tool_calls: Vec<PartialToolCall> }`.
5. On `Done`, assembles the final `Message` (parses accumulated tool call argument strings into `serde_json::Value`).
6. Proceeds with tool dispatch identical to the non-streaming loop.

Add `send_message_streaming(...)` and `confirm_tool_streaming(...)` wrappers that accept `chunk_tx`.

### 5g. TUI integration (`src/tui/mod.rs`)

- `spawn_send_message` and `spawn_confirm_tool`: branch on `config.streaming_enabled`. Streaming path passes `tx.clone()` as `chunk_tx` and sends `AppEvent::StreamComplete` at the end. Non-streaming path unchanged.
- `handle_app_event`: new arms for `StreamChunk` → `app.handle_stream_chunk(chunk)` and `StreamComplete` → `app.finalize_stream()` then `app.handle_outcome(...)`.

### 5h. Chat widget (`src/tui/widgets/chat.rs`)

At the end of the render loop (where the `is_loading` / "thinking..." indicator currently is):

- If `app.streaming_text` is `Some(text)`, render it with the AI prefix and a block cursor (`▌`) at the end.
- Else if `app.is_loading`, show "thinking..." (non-streaming fallback).

### 5i. Textarea style update (`src/tui/mod.rs` `update_textarea_style`)

When `streaming_text.is_some()`, show `" Streaming... "` title instead of `" Waiting for response... "`.

**Verify:** `cargo check`, run with `STREAMING=true` (default), send a message, confirm tokens appear incrementally. Run with `STREAMING=false`, confirm the old batch behavior still works. Confirm tool calls still work mid-stream (AI calls a tool, user confirms, streaming resumes).

---

## Phase 6: Context-window guard

**Files:** `src/config.rs`, `src/llm/mod.rs`, `src/app.rs`

### 6a. Config additions

- `pub context_max_tokens: usize` — from `CONTEXT_MAX_TOKENS` env var, default `128_000`
- `pub context_warn_ratio: f64` — from `CONTEXT_WARN_RATIO` env var, default `0.80`

### 6b. Token estimation in `src/llm/mod.rs`

```rust
pub fn estimate_tokens(messages: &[Message]) -> usize {
    messages.iter().map(|m| {
        let mut chars = m.content.len() + m.role.len();
        if let Some(calls) = &m.tool_calls {
            for tc in calls {
                chars += tc.function.name.len();
                chars += tc.function.arguments.to_string().len();
            }
        }
        chars / 4  // rough heuristic: 1 token ~ 4 chars
    }).sum()
}
```

### 6c. Guard checks in `src/app.rs`

In `send_message` (and `send_message_streaming`), after building the `working` messages vec and before calling the agentic loop:

- Estimate tokens via `llm::estimate_tokens(&working)`.
- If ratio >= 1.0 → return `LlmOutcome::Error` with a message telling the user to clear with Ctrl+L.
- If ratio >= `context_warn_ratio` → push a `ChatEntry` with `ChatRole::Error` (or a new `ChatRole::Warning` with yellow/dark-gray styling) warning that the conversation is ~N% of the context window.

Same checks in `confirm_tool` / `confirm_tool_streaming`.

### 6d. (Optional) `ChatRole::Warning`

Add a new role with distinct yellow styling in `chat.rs` to differentiate context warnings from errors.

**Verify:** Send many long messages until estimated tokens exceed 80% threshold, confirm warning appears. Continue past 100%, confirm the request is blocked with a clear error.

---

## .env.example updates

Add to `.env.example` at the end:

```env
# Streaming (default: true). Set to "false" to disable token-by-token streaming.
# STREAMING=true

# Context window guard (default: 128000 tokens). Set to 0 to disable.
# CONTEXT_MAX_TOKENS=128000
# CONTEXT_WARN_RATIO=0.80
```

---

## Verification checklist (end-to-end)

1. `cargo clippy -- -D warnings` — clean
2. `cargo test` — all tests pass
3. Run app with `just run-self`:
   - Ask AI to create a file → file tree refreshes automatically
   - Ask AI to make a small edit → uses `patch_file` with confirmation
   - Ask AI to create a directory → works without confirmation
   - Ask AI to search for a pattern → returns grep-like results
   - Navigate file tree: Right to expand, Left to collapse, indentation correct
   - Streaming: tokens appear live with cursor, tool calls still work
   - Send many messages until context warning appears
   - `STREAMING=false` → old batch behavior still works
