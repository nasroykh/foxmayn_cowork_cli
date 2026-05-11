# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Commands

```bash
just check          # cargo check
just lint           # cargo clippy -- -D warnings
just fmt            # cargo fmt
just ci             # fmt-check + lint + check in one shot

just build          # debug build
just release        # release build

just run dir=.      # run with a directory (required arg)
just run-self       # run pointed at the project itself
just run-ollama dir=.   # run with Ollama provider
just run-local dir=.    # run with fully-offline local llama.cpp provider (requires --features local build)
just run-model dir=. model=qwen3:0.6b  # model override

just env            # copy .env.example → .env if missing
```

Run a single test module: `cargo test llm::types` (tests live in `src/llm/types.rs`).

### Probe subcommand

`foxmayn-cowork probe [message] [--dir <path>] [--stream]` — fires one message at Ollama without the TUI and dumps the raw HTTP request, response body, and every tool round-trip to stdout. Ollama only (hardcoded to `ollama_base_url`). Useful for isolating streaming or tool-schema bugs. Pass `--stream` to drive `llm::chat_stream` + the agentic-loop chunk assembler — the exact code path that runs in the TUI — and print each `StreamChunk::ToolCallDelta` live, which is what you want when diagnosing streaming tool-call corruption.

### Config subcommand

`foxmayn-cowork config` — interactive wizard that writes `~/.config/foxmayn-cowork/.env`. Run on first launch if no config exists and stdin is a TTY (`setup::needs_init`). Re-runnable any time to change provider, model, or API key.

## Setup

Copy `.env.example` to `.env` and fill in `OPENROUTER_API_KEY`. Ollama users set `PROVIDER=ollama` and optionally `OLLAMA_BASE_URL`. For fully-offline inference set `PROVIDER=local` and build with `cargo run --features local` (requires `cmake` on `$PATH`).

## Architecture

This is a terminal UI (TUI) app: an AI-powered file manager. The user chats with an LLM which can read/write/delete files in a chosen working directory.

### Data flow

```
crossterm events → tui/mod.rs (run_loop)
                        │
                        ├─ spawns tokio tasks → app.rs (send_message / confirm_tool)
                        │                           └─ llm::chat (openrouter / ollama)
                        │                           └─ llm::tools (dispatch_tool_call / execute_tool)
                        │
                        └─ receives AppEvent via mpsc channel → app.handle_*()
```

All async work is fire-and-forget (`tokio::spawn`). Results come back through an `mpsc::UnboundedSender<AppEvent>` channel. `App` holds no `Arc`/`Mutex` — tasks receive cloned values and return `(LlmOutcome, Vec<Message>)`.

### Key files

- `src/app/` — App module (split from monolithic `app.rs`):
  - `state.rs` — `App` struct + all `impl App` methods; `TreeEntry`, `ChatEntry`, `ChatRole`, `InputMode`, `Panel`, `PendingToolCall`
  - `events.rs` — `AppEvent`, `LlmOutcome`, `RequestId`
  - `agentic.rs` — `run_agentic_loop` (unified streaming + non-streaming; drives LLM ↔ tool loop up to `MAX_TOOL_ROUNDS`), `apply_confirmation_policy`, `format_tool_summary`
  - `entry.rs` — four public entry points: `send_message`, `send_message_streaming`, `confirm_tool`, `confirm_tool_streaming`
  - `system_prompt.rs` — `system_prompt`, `working_dir_summary`
- `src/tui/mod.rs` — terminal setup/teardown, `run_loop` event multiplexer (`tokio::select!` over crossterm events, mpsc channel, and a 10-second health-check tick).
- `src/tui/ui.rs` + `src/tui/widgets/` — ratatui rendering (chat panel, file tree, confirmation dialog).
- `src/llm/tools/` — tools module (split from monolithic `tools.rs`):
  - `schema.rs` — `tool_definitions()` (JSON schema for all 17 tools sent to the LLM)
  - `dispatch.rs` — `dispatch_tool_call` (gates destructive ops behind confirmation), `execute_tool`, `DESTRUCTIVE_OPS`
  - `validate.rs` — `validate_path_containment`, `resolve_paths`, path-safety helpers + tests
  - `descriptions.rs` — `build_description`, `brief_action` (used by chat-panel display verbosity), confirmation text helpers
- `src/llm/openrouter.rs` / `src/llm/ollama.rs` — provider-specific HTTP clients; `llm::mod.rs` dispatches to the right one based on `Config::provider`.
- `src/llm/runtime.rs` — `LlmRuntime` (cheaply-cloneable handle holding `reqwest::Client` and, when `--features local`, an `Arc<LocalRuntime>`); built once at startup in `main.rs` and cloned into every spawned task.
- `src/llm/local.rs` — `--features local` only; `LocalRuntime` (llama.cpp backend + model loaded once), `chat` / `chat_stream` (run inference in `spawn_blocking`, bridge result to `StreamChunk` channel), `build_prompt` (ChatML format), `parse_output` (JSON tool-call detection).
- `src/config.rs` — `Config::from_env()` + `with_overrides()`, reasoning/think env var parsing, display-verbosity env vars (`TOOL_DISPLAY_VERBOSITY`, `THINKING_DISPLAY`), local model env vars (`LOCAL_MODEL_REPO`, `LOCAL_MODEL_FILE`, `LOCAL_MODEL_PATH`, `LOCAL_CONTEXT_TOKENS`, `LOCAL_GPU_LAYERS`, `LOCAL_THREADS`, `LOCAL_MAX_OUTPUT_TOKENS`, `LOCAL_TEMPERATURE`).
- `src/setup.rs` — `config_path` (`~/.config/foxmayn-cowork/.env`), `needs_init`, `run_wizard` (interactive provider/model/API-key prompts; password input via `rpassword`).
- `src/storage.rs` — SQLite-backed persistent storage. `Storage` opens a global DB plus a per-project DB keyed by `sanitize_path(working_dir)`. `apply_saved_settings` layers saved settings over the env-derived `Config` at startup. `SessionSummary` powers `/sessions` / `/resume`.
- `src/fs.rs` — async file-system operations called by tools; includes `read_pdf` (extracts text via `pdf-extract`, 50 MB cap, runs in `spawn_blocking`).

### Tool confirmation flow

`DESTRUCTIVE_OPS` (in `src/llm/tools/dispatch.rs`) currently covers: `delete_file`, `delete_many`, `delete_matching`, `edit_file`, `patch_file`, `rename_file`, `rename_many`, `rename_matching`. When the LLM calls one, `dispatch_tool_call` returns `requires_confirmation: true` without executing. The TUI enters `InputMode::Confirming` and shows a confirmation widget. Pressing `y` calls `confirm_tool → execute_tool`; `n`/Esc cancels and returns `"Operation cancelled."` as a plain assistant message. With `--skip-confirmations` / `SKIP_CONFIRMATIONS=true`, `apply_confirmation_policy` executes the call inline and tags the description as `"… (confirmation skipped)"`.

Read-only tools (`list_files`, `read_file`, `read_pdf`, `find_files`, `search_in_files`) and pure-creation tools (`create_file`, `create_directory`, `copy_file`) never require confirmation.

### LLM providers

Three providers are supported, all dispatched through `llm::mod.rs` via `&LlmRuntime`:

- `openrouter` — HTTP; sends `reasoning` (effort + summary verbosity). Default model `google/gemini-2.5-flash-lite` with `reasoning.effort: "minimal"`. Outgoing requests go through `to_openrouter_body` which converts each `tool_calls[].function.arguments` from a JSON object to a JSON string (OpenAI/Google AI Studio reject the object form). Streamed `delta.reasoning` is forwarded as `StreamChunk::ThinkingDelta`.
- `ollama` — HTTP; sends `think` (bool or `high`/`medium`/`low`). Default endpoint `http://localhost:11434`. Keeps `arguments` as a JSON object on the wire. Streamed `message.thinking` is forwarded as `StreamChunk::ThinkingDelta`. At startup, `main.rs` calls `ollama::model_supports_tools` (POST `/api/show`, check `capabilities` includes `"tools"`) and refuses to launch with a clear error when the model is not tool-capable; network errors degrade to a warning. The streaming NDJSON parser (`process_ndjson_stream`) buffers the most recent `tool_calls` array across lines and emits it once at `done` — this is correct whether the model emits tool calls on `done:false` only (qwen3 thinking), `done:true` only, or both, and it preserves Ollama-provided tool-call ids when present.
- `local` — embedded llama.cpp via the `llama-cpp-2` crate; only available when built with `--features local`. Tool schemas are injected into the system prompt in ChatML format; tool calls are detected by scanning the output for `{"name": …, "arguments": …}`. Inference runs in `tokio::task::spawn_blocking`. The model is loaded once at startup (`LocalRuntime`) and shared across requests via `Arc`. Each request creates a fresh `LlamaContext`. Requires `cmake` at build time.

### Tool / thinking display

Two env vars control how live activity is rendered in the chat panel:

- `TOOL_DISPLAY_VERBOSITY` (`default` | `minimal` | `full`) — handled by `format_tool_summary` in `src/app/agentic.rs`, used by both streaming (`AppEvent::IntermediateTool`) and non-streaming (`LlmOutcome::Complete.tool_results`) paths so they emit identical `[name] …` lines. `default` uses `brief_action(tool_name)` from `llm/tools/descriptions.rs`; `minimal` uses `build_description`; `full` appends a result snippet capped at `TOOL_DISPLAY_FULL_RESULT_CAP`.
- `THINKING_DISPLAY` (`off` | `inline` | `full`) — `App::thinking_text: Option<String>` accumulates `StreamChunk::ThinkingDelta` fragments; `App::finalize_thinking_for_round` is called at every round boundary (`IntermediateAssistant`, `IntermediateTool`, `StreamComplete`) and is idempotent. `inline` shows a dimmed `[Thinking… (N chars)]` line above the streaming buffer and discards on finalize; `full` streams reasoning live and pushes a permanent `ChatRole::Thinking` entry on finalize.

### Slash commands

Dispatched by `dispatch_slash_command` in `src/tui/mod.rs`. Typing `/` in the chat input opens an autocomplete popup; arrow keys + Enter pick a command. Some commands open a secondary picker (e.g. `/model` with no arg lists models from the provider; `/thinking` / `/tool-verbosity` / `/reasoning` show a static picker of valid values).

| Command | Action |
|---------|--------|
| `/clear` | Clear conversation |
| `/exit` | Quit |
| `/dir <path>` | Change working directory |
| `/model [name]` | Switch model (opens picker if no arg) — persisted via `storage::persist_setting` |
| `/streaming` | Toggle streaming responses — persisted |
| `/thinking <off\|inline\|full>` | Set `THINKING_DISPLAY` — persisted |
| `/tool-verbosity <default\|minimal\|full>` | Set `TOOL_DISPLAY_VERBOSITY` — persisted |
| `/reasoning <…>` | Set OpenRouter reasoning effort — persisted |
| `/skip-confirmations` | Toggle confirmation gate **for the current session only** (not persisted — always starts safe) |
| `/sessions` | List recent sessions for this project (from `storage`) |
| `/resume <id>` | Reload a prior session's conversation |

### Keyboard shortcuts (runtime)

| Key | Action |
|-----|--------|
| Enter | Send message / confirm slash-command pick / expand tree node |
| Ctrl+C | Quit |
| Ctrl+L | Clear conversation |
| Tab | Switch focus: Chat ↔ File Tree |
| ↑ / ↓ | Scroll focused panel / move within slash-command popup |
| → / Enter (tree) | Expand selected directory |
| ← (tree) | Collapse directory / jump to parent |
| Esc | Close slash popup / cancel in-flight request / reject confirmation |
| y / n / Esc | Confirm / cancel destructive tool (Confirming mode) |
