# foxmayn-cowork

<!-- docs/demo.gif does not exist yet. Render it with: vhs docs/demo.tape -->
![demo](docs/demo.gif)

A terminal UI for AI-assisted file management. Chat with an LLM to inspect and change files in a selected working directory while destructive operations stay behind an explicit confirmation prompt.

## Features

- File tools: list, read, create, overwrite, delete, rename, create directories, copy files, search file contents, patch files with search/replace, and extract text from PDF files.
- Bulk operations: delete or rename many explicit paths, and delete or rename files by filename regex with a single confirmation.
- Safety boundary: all tool paths are validated against the selected working directory, and paths through symlinks that leave it are rejected. Destructive operations (edit, patch, delete, rename, and bulk operations) require `y` confirmation. So does creating or copying anything under a hidden path (any component starting with `.`). `create_file` and `copy_file` never overwrite existing files.
- Agentic loops: the model can call tools over multiple rounds, then summarize what it did.
- Streaming responses: tokens render as they arrive by default, including intermediate tool-round progress.
- File tree navigation: expand/collapse subdirectories and keep expanded paths across automatic refreshes.
- Context guard: warns before the conversation approaches the configured token budget.
- Tool display verbosity: pick how detailed each Tool entry is in chat — brief action, parametrized description, or description plus a capped result snippet.
- Reasoning visibility: optionally surface the model's thinking tokens as a live indicator or a permanent dimmed transcript entry.
- Slash commands with autocomplete: `/dir`, `/model`, `/streaming`, `/thinking`, `/tool-verbosity`, `/reasoning`, `/skip-confirmations`, `/clear`, `/sessions`, `/resume`, `/exit`.
- Session persistence: conversations are saved per project to a SQLite store; resume a prior session with `/resume <id>`.
- Interactive configuration wizard: `foxmayn-cowork config` writes `~/.config/foxmayn-cowork/.env` (also runs automatically on first launch).
- Ollama preflight: refuses to launch against an Ollama model whose `capabilities` does not declare `tools`, with a clear pointer to a working alternative. If `OLLAMA_THINK` is set and the model cannot think, the setting is dropped with a warning.
- Network resilience: 15 s connect timeout, 300 s idle read timeout, and up to 2 retries on HTTP 429/502/503/504 (honors `Retry-After`).

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

Both scripts detect your OS/architecture, download the correct binary from the latest GitHub Release, and verify the SHA256 checksum before installing. A missing checksum is an error; set `FOXMAYN_COWORK_SKIP_CHECKSUM=1` to skip verification (not recommended). Set `FOXMAYN_VERSION=vX.Y.Z` to pin a version. `GITHUB_TOKEN` is optional (private repos or API rate limits).

Prebuilt binaries: x86_64 and aarch64 Linux, x86_64 and aarch64 macOS, x86_64 Windows (Windows on ARM runs the x64 build). They do **not** include the `local` provider; build from source for that (see below).

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

# or install into ~/.cargo/bin with local inference
cargo install --path . --features local
```

Requires Rust 1.89 or newer. The `local` feature also needs `cmake` and `clang`.

Run `foxmayn-cowork --version` to print the version.

## Providers

| Provider | Requires | Notes |
| --- | --- | --- |
| `openrouter` (default) | `OPENROUTER_API_KEY` | Cloud; default model `google/gemini-2.5-flash-lite` |
| `ollama` | Running Ollama server | Local server; any model pulled via `ollama pull` |
| `local` | Build with `--features local` | Fully offline; downloads a GGUF from HuggingFace on first run |

## Setup

The first time you launch `foxmayn-cowork`, an interactive wizard runs if no config file exists, neither `PROVIDER` nor `OPENROUTER_API_KEY` is set in the environment, and stdin is a terminal. It writes `~/.config/foxmayn-cowork/.env` (file mode `0600`, directory `0700` on unix). Re-run it any time with:

```bash
foxmayn-cowork config
```

The app reads only that file. It does not read a `.env` from the current directory or its parents. Real environment variables override the file.

To configure manually, or for development from a clone (the Justfile has `set dotenv-load`, so `just run` loads the repo-root `.env`):

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

Type `/` in the chat input to open the slash-command autocomplete; arrow keys + Enter pick a command. Commands typed with arguments run on Enter. `/clear`, `/dir` and `/resume` (and `Ctrl+L`) cancel any in-flight request. Available commands:

| Command | Effect |
| --- | --- |
| `/dir <path>` | Switch the working directory |
| `/model [name]` | Change model (opens a picker if no name given); saved per provider |
| `/streaming` | Toggle token streaming |
| `/thinking <off\|inline\|full>` | Reasoning visibility |
| `/tool-verbosity <default\|minimal\|full>` | Tool entry detail |
| `/reasoning <effort>` | OpenRouter reasoning effort |
| `/skip-confirmations` | Toggle the destructive-op confirmation gate **for this session only** (never persisted) |
| `/sessions` | List saved sessions for the current project |
| `/resume <id>` | Reload a prior session |
| `/clear` | Clear the conversation |
| `/exit` | Quit |

The file tree refreshes after successful AI operations.

Destructive operations ask for confirmation by default. When the model sends several tool calls in one turn, the first call that needs confirmation pauses the rest. If you decline, that call and all remaining calls are answered as not executed, and no further model request is made. A long confirmation can be scrolled with `Up` / `Down` / `PgUp` / `PgDn` / `Home` / `End`; hidden lines show as `…and N more`.

To give the AI full write/delete/rename power without prompts, launch with `--skip-confirmations` or set `SKIP_CONFIRMATIONS=true`. This is intentionally dangerous; use it only inside a directory you are comfortable letting the model mutate. While confirmations are off, a red `CONFIRMATIONS OFF` badge shows in the chat panel.

Keyboard shortcuts:

| Key | Action |
| --- | --- |
| `Enter` | Send chat message, or expand/collapse the selected tree directory |
| `Shift+Enter` / `Alt+Enter` | Insert newline in the chat input |
| `Esc` | Cancel in-flight request, or reject a pending destructive operation |
| `Ctrl+C` | Quit (`SIGTERM` / `SIGHUP` also quit cleanly) |
| `Ctrl+L` | Clear conversation |
| `Tab` | Switch focus between chat and file tree |
| `Up` / `Down` | Scroll focused panel |
| `Right` | Expand selected tree directory |
| `Left` | Collapse selected tree directory or jump to parent |
| `y` / `n` | Approve or reject a pending operation |
| `Up` / `Down` / `PgUp` / `PgDn` / `Home` / `End` | Scroll the confirmation dialog while it is open |

## Configuration

Settings come from the real environment and `~/.config/foxmayn-cowork/.env` (the environment wins). CLI flags override both. A model chosen with `/model` is saved per provider and does not override an explicit `--model`. `.env.example` lists every variable.

| Variable / Flag | Default | Description |
| --- | --- | --- |
| `PROVIDER`, `--provider` | `openrouter` | `openrouter`, `ollama`, or `local` (`open-router` is accepted as an alias) |
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
| `OLLAMA_THINK` | unset (not sent) | `true`, `false`, `high`, `medium`, `low`, or `off`. Dropped with a warning if the model lacks the `thinking` capability |
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

## Tool limits

- Search and find do not follow symlinks. Delete and rename on a symlink act on the link. The working directory itself cannot be deleted or renamed.
- `rename_file` refuses an existing destination.
- Search skips files over 5 MB, binary files, hidden directories, and `node_modules`, `target`, `__pycache__`, `dist`, `build`, `vendor`, `venv`.
- Tool results sent to the model are capped at 100,000 characters.

## Debug / probe mode

The `probe` subcommand sends a single message directly to the Ollama HTTP API and prints the raw JSON request, response, and any tool round-trips to stdout without starting the TUI. Useful for diagnosing provider issues or iterating on tool schemas. Pass `--stream` to exercise the streaming chunk-assembler — the same code path the TUI uses — and print each tool-call delta as it arrives.

```bash
foxmayn-cowork probe "list all .md files"                       # non-streaming
foxmayn-cowork probe "summarise report.pdf" --dir /path/to/dir
foxmayn-cowork probe --stream "what does CLAUDE.md say?"        # streaming
```

## Development

```bash
just check      # cargo check
just lint       # cargo clippy --all-targets -- -D warnings
just fmt        # cargo fmt
just test       # cargo test
just audit      # cargo audit (needs: cargo install cargo-audit)
just ci         # fmt-check + lint + check + test
just doctor     # smoke-check toolchain, config, tests, and build
```

## License

MIT. See [LICENSE](LICENSE).
