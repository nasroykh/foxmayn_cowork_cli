# foxmayn-cowork

A terminal UI for AI-assisted file management. Chat with an LLM to inspect and change files in a selected working directory while destructive operations stay behind an explicit confirmation prompt.

## Features

- File tools: list, read, create, overwrite, delete, rename, create directories, copy files, search file contents, and patch files with search/replace.
- Bulk operations: delete or rename many explicit paths, and delete or rename files by filename regex with a single confirmation.
- Safety boundary: all tool paths are validated against the selected working directory; destructive operations, including rename/move and bulk operations, require `y` confirmation.
- Agentic loops: the model can call tools over multiple rounds, then summarize what it did.
- Streaming responses: tokens render as they arrive by default, including intermediate tool-round progress.
- File tree navigation: expand/collapse subdirectories and keep expanded paths across automatic refreshes.
- Context guard: warns before the conversation approaches the configured token budget.

## Providers

Supports **OpenRouter** (default) and **Ollama**. The default model is `google/gemini-2.5-flash-lite`.

## Setup

```bash
just env
# Fill in OPENROUTER_API_KEY, or set PROVIDER=ollama for a local Ollama server.
just run dir=/path/to/dir
```

Ollama example:

```bash
just run-ollama dir=/path/to/dir
```

## Usage

Type a message and press **Enter** to send. Use **Shift+Enter** or **Alt+Enter** for a newline; multi-line paste is supported. Press **Esc** to cancel an in-flight request.

Use `/dir <path>` to switch the working directory at any time. The file tree refreshes after successful AI operations.

Destructive operations ask for confirmation by default. To give the AI full write/delete/rename power without prompts, launch with `--skip-confirmations` or set `SKIP_CONFIRMATIONS=true`. This is intentionally dangerous; use it only inside a directory you are comfortable letting the model mutate.

Keyboard shortcuts:

| Key | Action |
| --- | --- |
| `Enter` | Send chat message, or expand/collapse the selected tree directory |
| `Shift+Enter` / `Alt+Enter` | Insert newline in the chat input |
| `Esc` | Cancel in-flight request, or reject a pending destructive operation |
| `Ctrl+C` | Quit |
| `Ctrl+L` | Clear conversation |
| `Tab` | Switch focus between chat and file tree |
| `Up` / `Down` | Scroll focused panel |
| `Right` | Expand selected tree directory |
| `Left` | Collapse selected tree directory or jump to parent |
| `y` / `n` | Approve or reject a pending destructive operation |

## Configuration

Environment variables are read from `.env` when present. CLI flags override matching environment values.

| Variable / Flag | Default | Description |
| --- | --- | --- |
| `PROVIDER`, `--provider` | `openrouter` | `openrouter` or `ollama` |
| `MODEL`, `--model` | `google/gemini-2.5-flash-lite` | Model name |
| `OPENROUTER_API_KEY` | none | Required for OpenRouter |
| `OLLAMA_BASE_URL` | `http://localhost:11434` | Ollama endpoint |
| `OPENROUTER_BASE_URL` | `https://openrouter.ai/api/v1` | OpenRouter-compatible endpoint |
| `STREAMING`, `--streaming` | `true` | Enable token-by-token streaming |
| `SKIP_CONFIRMATIONS`, `--skip-confirmations` | `false` | Dangerous: execute destructive tools without confirmation |
| `CONTEXT_MAX_TOKENS` | `128000` | Estimated conversation token ceiling; `0` disables the guard |
| `CONTEXT_WARN_RATIO` | `0.80` | Fraction of max tokens that triggers a warning |
| `OPENROUTER_REASONING_EFFORT` | `minimal` | `xhigh`, `high`, `medium`, `low`, `minimal`, `none`, or `off` |
| `OPENROUTER_REASONING_SUMMARY` | none | `auto`, `concise`, or `detailed` |
| `OLLAMA_THINK` | `low` | `true`, `false`, `high`, `medium`, `low`, or `off` |

## Development

```bash
just check      # cargo check
just lint       # cargo clippy -- -D warnings
just fmt        # cargo fmt
just ci         # fmt-check + lint + check
just doctor     # smoke-check toolchain, config, tests, and build
```
