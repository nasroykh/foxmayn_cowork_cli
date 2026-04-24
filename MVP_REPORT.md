# foxmayn-cowork — MVP Analysis Report

**Date:** 2026-04-24  
**Codebase state:** ~800 lines of Rust across 12 source files, edition 2024, no open compiler warnings.

---

## 1. Is this ready as an MVP?

**Not yet — it is close, but has one functional gap that would confuse any first-time user.**

The core loop works end-to-end: user types a request → the LLM calls tools → files are actually created, edited, deleted, or renamed on disk. The two-provider design (OpenRouter + Ollama), the confirmation flow for destructive operations, and the agentic tool-call loop up to 10 rounds are all solid foundations.

However, **the file tree panel does not refresh after AI operations.** The tree is loaded once at startup and again only when the user runs `/dir`. So if the assistant creates three new files, the tree still shows the old state. For an app whose headline feature is AI file management, this is a first-impression-breaking omission — the user cannot see that anything happened without leaving and re-entering the directory.

Everything else needed for an MVP is present or is a polish issue rather than a functional one.

---

## 2. What about the main feature? (File management through AI conversation)

The feature is **architecturally complete but operationally incomplete.** Here is a breakdown:

### What works well
- All six file operations (`list_files`, `read_file`, `create_file`, `edit_file`, `delete_file`, `rename_file`) execute correctly against the real filesystem.
- The path-containment validator in `tools.rs` prevents the AI from escaping the working directory, which is the most important safety invariant for this kind of tool.
- Destructive operations (`edit_file`, `delete_file`) correctly require a `[y/n]` confirmation before any bytes are touched.
- The multi-step agentic loop (up to 10 rounds) handles tasks like "delete all `.log` files" correctly: the AI lists files, acts on one, then waits for the next message, just as the system prompt instructs.

### What is incomplete

| Gap | Impact |
|-----|--------|
| **File tree not refreshed after AI operations** | High — the main visual feedback mechanism is stale after every operation |
| **`edit_file` overwrites the entire file** | Medium — the AI must read the file, modify it mentally, and re-send the full content. On any file > a few hundred lines this is error-prone and expensive in tokens |
| **No `create_directory` tool** | Medium — dirs are created as a side-effect of `create_file` (via `create_dir_all`) but cannot be created standalone |
| **File tree is single-level only** | Medium — the left panel shows only the root of the working directory; subdirectories cannot be expanded in the UI |
| **No streaming** (`stream: false` hardcoded) | Medium — responses arrive all at once; on slower models or large tasks the UI appears frozen for many seconds behind the "thinking..." spinner |
| **No token / context-window management** | Low-Medium — the conversation grows unboundedly until the model silently starts degrading or returns errors |
| **No `copy_file` or `search_in_files` (grep) tool** | Low — missing basic file-management primitives |

---

## 3. Features needed to become a complete MVP

These are the minimum changes before calling this a finished MVP:

1. **Auto-refresh the file tree after every LLM response** — when `AppEvent::LlmResponse` is received and the outcome is `Complete`, re-run `spawn_file_tree_load` against the current `working_dir`. One-liner in `handle_app_event` in `tui/mod.rs`.

2. **`create_directory` tool** — expose `tokio::fs::create_dir_all` as a first-class tool so users can ask "create a `tests/` folder" without needing a workaround.

3. **Subdirectory expansion in the file tree** — the left panel should allow navigating into subdirectories (even a simple toggle with Enter/Space on a selected directory), otherwise the file tree is decorative for any non-trivial project.

4. **Streaming responses** — the `stream` field already exists in `ChatRequest`. Implementing server-sent event (SSE) streaming for OpenRouter and Ollama would make the app feel live rather than frozen-then-done. This is the single biggest UX improvement available.

5. **Context window guard** — track approximate token count (or message count) and warn the user when the conversation is getting long, with a suggestion to press `Ctrl+L` to clear.

---

## 4. Remaining tasks for a public Beta

Beyond the MVP fixes above, reaching a Beta that can be handed to an outside audience requires:

### Installation & distribution
- `cargo install` or pre-built binaries (GitHub Releases, with a `release` CI workflow)
- A short "install in one command" story in the README
- First-run experience: detect a missing `OPENROUTER_API_KEY` at startup and print a human-readable message instead of failing silently or with a raw HTTP 401 body

### UX & polish
- **Streaming** (already listed, worth repeating — it is the biggest perceived-quality gap)
- Render the file tree hierarchically with expand/collapse on directories
- Show relative paths in the file tree (not the full absolute path in the chat panel title)
- Wrap long lines in the chat panel without cutting off content
- Loading indicator with elapsed time ("thinking... 4s") so users know the app is alive
- Mouse support (scroll) — not required but expected by most terminal users today

### Reliability & safety
- Graceful recovery when the LLM returns a malformed tool call (currently surfaced as an error but the conversation is left in a recoverable state — worth a dedicated test)
- Rate-limit and timeout handling for the HTTP client (`reqwest::Client` currently has no timeout set)
- Maximum file size enforcement at the `create_file` / `edit_file` level, not just at `read_file` — the AI can currently write arbitrarily large files

### Quality assurance
- Integration tests that exercise the full tool dispatch pipeline with a mock HTTP server
- A smoke-test binary that validates the `.env` config and provider connectivity (`just doctor`)

### Documentation
- Animated GIF or screenshot in the README showing the chat → file change → tree refresh loop
- A short user guide covering the `/dir` command, confirmation flow, and provider switching

---

## Summary

```
MVP completeness:  ████████░░  ~80%
Main feature:      ████████░░  works, missing tree refresh + streaming
Beta readiness:    █████░░░░░  ~50%
```

The codebase is clean, well-structured, and the hard architectural work (agentic loop, dual-provider abstraction, confirmation gate, path sandboxing) is done correctly. The remaining gap to MVP is small and mostly concentrated in the file tree refresh bug. The gap to a public Beta is mainly distribution, streaming, and UX polish — none of it requires rethinking the architecture.
