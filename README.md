# foxmayn-cowork

A terminal UI for AI-assisted file management. Chat with an LLM to inspect and change files in a selected working directory while destructive operations stay behind an explicit confirmation prompt.

## Features

- File tools: list, read, create, overwrite, delete, rename, create directories, copy files, search file contents, patch files with search/replace, and extract text from PDF files.
- Bulk operations: delete or rename many explicit paths, and delete or rename files by filename regex with a single confirmation.
- Safety boundary: all tool paths are validated against the selected working directory; destructive operations, including rename/move and bulk operations, require `y` confirmation.
- Agentic loops: the model can call tools over multiple rounds, then summarize what it did.
- Streaming responses: tokens render as they arrive by default, including intermediate tool-round progress.
- File tree navigation: expand/collapse subdirectories and keep expanded paths across automatic refreshes.
- Context guard: warns before the conversation approaches the configured token budget.
- Tool display verbosity: pick how detailed each Tool entry is in chat — brief action, parametrized description, or description plus a capped result snippet.
- Reasoning visibility: optionally surface the model's thinking tokens as a live indicator or a permanent dimmed transcript entry.

## Install

**Linux & macOS:**

```bash
curl -fsSL https://raw.githubusercontent.com/nasroykh/foxmayn_cowork_cli/main/install.sh | sh
```

Installs to `/usr/local/bin` (or `~/.local/bin` as a fallback).

**Windows (PowerShell):**

```powershell
powershell -ExecutionPolicy Bypass -Command "irm https://raw.githubusercontent.com/nasroykh/foxmayn_cowork_cli/main/install.ps1 | iex"
```

Both scripts detect your OS/architecture, download the correct binary from the latest GitHub Release, and verify the SHA256 checksum before installing.

**Manually** — download a pre-built binary from the [Releases page](https://github.com/nasroykh/foxmayn_cowork_cli/releases).

**From source:**

```bash
git clone https://github.com/nasroykh/foxmayn_cowork_cli.git
cd foxmayn_cowork_cli
cargo build --release                           # OpenRouter / Ollama
cargo build --release --features local          # + offline local inference (CPU)
cargo build --release --features local-metal    # + Metal GPU (macOS)
cargo build --release --features local-cuda     # + CUDA GPU (NVIDIA)
# binary: target/release/foxmayn-cowork
```

## Providers

| Provider | Requires | Notes |
| --- | --- | --- |
| `openrouter` (default) | `OPENROUTER_API_KEY` | Cloud; default model `google/gemini-2.5-flash-lite` |
| `ollama` | Running Ollama server | Local server; any model pulled via `ollama pull` |
| `local` | Build with `--features local` | Fully offline; downloads a GGUF from HuggingFace on first run |

## Setup

**OpenRouter (default):**

```bash
just env
# Fill in OPENROUTER_API_KEY in .env
just run dir=/path/to/dir
```

**Ollama:**

```bash
just run-ollama dir=/path/to/dir
```

**Local (fully offline, no API key or Ollama needed):**

```bash
# One-time: install cmake (required to compile llama.cpp)
brew install cmake          # macOS
sudo apt install cmake      # Linux

just run-local dir=/path/to/dir
```

On first run the default model (`Qwen/Qwen2.5-1.5B-Instruct-GGUF`) is downloaded from HuggingFace into `~/.cache/huggingface/hub/` and reused on every subsequent launch. Set `LOCAL_MODEL_PATH` to use a GGUF file you already have on disk. GPU offload is off by default; set `LOCAL_GPU_LAYERS` to a non-zero value and build with `--features local-metal` (macOS) or `--features local-cuda` (NVIDIA) to enable it.

**From source with local inference:**

```bash
cargo build --release --features local
# binary: target/release/foxmayn-cowork
PROVIDER=local ./target/release/foxmayn-cowork --dir /path/to/dir
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
| `PROVIDER`, `--provider` | `openrouter` | `openrouter`, `ollama`, or `local` |
| `MODEL`, `--model` | `google/gemini-2.5-flash-lite` | Model name (OpenRouter/Ollama) |
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
| `TOOL_DISPLAY_VERBOSITY` | `default` | `default` (name + brief action), `minimal` (name + description with args), or `full` (name + description + capped result snippet) |
| `THINKING_DISPLAY` | `off` | `off` (discard), `inline` (live `[Thinking… (N chars)]` indicator), or `full` (stream reasoning live and keep it in chat). Requires a thinking model with reasoning enabled via `OPENROUTER_REASONING_EFFORT` / `OLLAMA_THINK` |

**Local provider variables** (only relevant when `PROVIDER=local`, build with `--features local`):

| Variable | Default | Description |
| --- | --- | --- |
| `LOCAL_MODEL_REPO` | `Qwen/Qwen2.5-1.5B-Instruct-GGUF` | HuggingFace repo to download from |
| `LOCAL_MODEL_FILE` | `qwen2.5-1.5b-instruct-q4_k_m.gguf` | Filename inside the repo |
| `LOCAL_MODEL_PATH` | none | Use a local GGUF file directly; skips HuggingFace download |
| `LOCAL_CONTEXT_TOKENS` | `8192` | Context window size in tokens |
| `LOCAL_GPU_LAYERS` | `0` | Layers to offload to GPU (requires `local-metal` or `local-cuda` feature) |
| `LOCAL_THREADS` | auto | Thread count for inference |
| `LOCAL_MAX_OUTPUT_TOKENS` | `1024` | Maximum tokens per response |
| `LOCAL_TEMPERATURE` | `0.1` | Sampling temperature (`0.0` = greedy) |

## Debug / probe mode

The `probe` subcommand sends a single message directly to the Ollama HTTP API and prints the raw JSON request, response, and any tool round-trips to stdout without starting the TUI. Useful for diagnosing provider issues or iterating on tool schemas.

```bash
foxmayn-cowork probe "list all .md files"          # uses cwd, default message
foxmayn-cowork probe "summarise report.pdf" --dir /path/to/dir
```

## Development

```bash
just check      # cargo check
just lint       # cargo clippy -- -D warnings
just fmt        # cargo fmt
just ci         # fmt-check + lint + check
just doctor     # smoke-check toolchain, config, tests, and build
```
