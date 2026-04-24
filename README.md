# foxmayn-cowork

A terminal UI for AI-assisted file management. Chat with an LLM to read, create, edit, rename, and delete files in a chosen working directory. Destructive operations require explicit confirmation before execution.

## Providers

Supports **OpenRouter** (default) and **Ollama**. The default model is `google/gemini-2.5-flash-lite`.

## Setup

```bash
just env                  # creates .env from .env.example
# fill in OPENROUTER_API_KEY (or set PROVIDER=ollama)
just run dir=/path/to/dir
```

## Usage

Type a message and press **Enter** to send. Use `/dir <path>` to switch the working directory at any time. Press **Tab** to toggle focus between the chat and file tree panels, **↑/↓** to scroll, and **Ctrl+L** to clear the conversation.

## Configuration

All options are set via environment variables (see `.env.example`):

| Variable | Default | Description |
|----------|---------|-------------|
| `PROVIDER` | `openrouter` | `openrouter` or `ollama` |
| `MODEL` | `google/gemini-2.5-flash-lite` | Model name |
| `OPENROUTER_API_KEY` | — | Required for OpenRouter |
| `OLLAMA_BASE_URL` | `http://localhost:11434` | Ollama endpoint |
| `OPENROUTER_REASONING_EFFORT` | `minimal` | `xhigh`, `high`, `medium`, `low`, `minimal`, `none`, or `off` |
| `OLLAMA_THINK` | `low` | `true`, `false`, `high`, `medium`, `low`, or `off` |
