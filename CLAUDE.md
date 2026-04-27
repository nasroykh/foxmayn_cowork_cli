# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Commands

```bash
just check          # cargo check (default)
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

`foxmayn-cowork probe [message] [--dir <path>]` — fires one message at Ollama without the TUI and dumps the raw HTTP request, response body, and every tool round-trip to stdout. Ollama only (hardcoded to `ollama_base_url`). Useful for isolating streaming or tool-schema bugs.

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

- `src/app.rs` — `App` struct (all UI state), `run_agentic_loop` (drives LLM ↔ tool loop up to 10 rounds), `send_message` / `confirm_tool` (entry points for async tasks), `LlmOutcome` / `AppEvent` enums.
- `src/tui/mod.rs` — terminal setup/teardown, `run_loop` event multiplexer (`tokio::select!` over crossterm events, mpsc channel, and a 10-second health-check tick).
- `src/tui/ui.rs` + `src/tui/widgets/` — ratatui rendering (chat panel, file tree, confirmation dialog).
- `src/llm/tools.rs` — tool definitions sent to the LLM (`list_files`, `read_file`, `read_pdf`, `create_file`, `edit_file`, `delete_file`, and bulk/regex variants), `dispatch_tool_call` (gates destructive ops behind confirmation), `execute_tool` (executes after confirmation), path containment validation.
- `src/llm/openrouter.rs` / `src/llm/ollama.rs` — provider-specific HTTP clients; `llm::mod.rs` dispatches to the right one based on `Config::provider`.
- `src/llm/runtime.rs` — `LlmRuntime` (cheaply-cloneable handle holding `reqwest::Client` and, when `--features local`, an `Arc<LocalRuntime>`); built once at startup in `main.rs` and cloned into every spawned task.
- `src/llm/local.rs` — `--features local` only; `LocalRuntime` (llama.cpp backend + model loaded once), `chat` / `chat_stream` (run inference in `spawn_blocking`, bridge result to `StreamChunk` channel), `build_prompt` (ChatML format), `parse_output` (JSON tool-call detection).
- `src/config.rs` — `Config::from_env()` + `with_overrides()`, reasoning/think env var parsing, local model env vars (`LOCAL_MODEL_REPO`, `LOCAL_MODEL_FILE`, `LOCAL_MODEL_PATH`, `LOCAL_CONTEXT_TOKENS`, `LOCAL_GPU_LAYERS`, `LOCAL_THREADS`, `LOCAL_MAX_OUTPUT_TOKENS`, `LOCAL_TEMPERATURE`).
- `src/fs.rs` — async file-system operations called by tools; includes `read_pdf` (extracts text via `pdf-extract`, 50 MB cap, runs in `spawn_blocking`).

### Tool confirmation flow

`delete_file` and `edit_file` are in `DESTRUCTIVE_OPS`. When the LLM calls one, `dispatch_tool_call` returns `requires_confirmation: true` without executing. The TUI enters `InputMode::Confirming` and shows a confirmation widget. Pressing `y` calls `confirm_tool → execute_tool`; `n`/Esc cancels and returns `"Operation cancelled."` as a plain assistant message.

`read_pdf` is non-destructive and never requires confirmation.

### LLM providers

Three providers are supported, all dispatched through `llm::mod.rs` via `&LlmRuntime`:

- `openrouter` — HTTP; sends `reasoning` (effort + summary verbosity). Default model `google/gemini-2.5-flash-lite` with `reasoning.effort: "minimal"`.
- `ollama` — HTTP; sends `think` (bool or `high`/`medium`/`low`). Default endpoint `http://localhost:11434`.
- `local` — embedded llama.cpp via the `llama-cpp-2` crate; only available when built with `--features local`. Tool schemas are injected into the system prompt in ChatML format; tool calls are detected by scanning the output for `{"name": …, "arguments": …}`. Inference runs in `tokio::task::spawn_blocking`. The model is loaded once at startup (`LocalRuntime`) and shared across requests via `Arc`. Each request creates a fresh `LlamaContext`. Requires `cmake` at build time.

### Keyboard shortcuts (runtime)

| Key | Action |
|-----|--------|
| Enter | Send message |
| Ctrl+C | Quit |
| Ctrl+L | Clear conversation |
| Tab | Switch focus: Chat ↔ File Tree |
| ↑ / ↓ | Scroll focused panel |
| → / Enter (tree) | Expand selected directory |
| ← (tree) | Collapse directory / jump to parent |
| `/dir <path>` | Change working directory |
| y / n / Esc | Confirm / cancel destructive tool (Confirming mode) |
