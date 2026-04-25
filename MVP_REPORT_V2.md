# foxmayn-cowork — MVP Verification Report V2

**Date:** 2026-04-25
**Codebase state:** 3,411 lines of Rust across 16 source files, edition 2024, commit `23e9c4a` ("Enhance streaming capabilities and configuration options").
**Scope of this report:** Independent verification of MVP completion after `MVP_COMPLETION_PLAN.md` was implemented end-to-end. Static analysis + build verification only — interactive TUI testing was deferred to the user.

---

## 0. TL;DR

> **Verdict: ~92% MVP. The plan was implemented faithfully and builds cleanly, but three issues introduced (or left) by the implementation will be obvious to any first-time user and should be fixed before claiming "100% MVP."**

| Layer | Status |
|---|---|
| **Code compiles, lints, tests** | ✅ All green (`cargo check`, `cargo clippy --all-targets -- -D warnings`, `cargo test` — 3/3, `cargo build --release`) |
| **MVP_COMPLETION_PLAN Phases 1–6** | ✅ All six phases land. One small plan deviation (no `--streaming` CLI flag — env var only). |
| **Original 7 gaps from MVP_REPORT.md** | ✅ All addressed |
| **New issues found in this review** | ⚠️ **3 MVP-blocking** (P0), **5 MVP-polish** (P1), **8 Beta** (P2) — see §4 |

The plan-defined definition of MVP is **complete**. By a stricter "demo to a stranger and have them not flinch" definition, three regressions/gaps stand out:

1. **Auto-refresh collapses every expanded subdirectory** — Phase 1 fixed the original "tree never updates" bug by re-loading from scratch, which loses all expansion state. Users who expanded `src/` to find a file see it close every time the AI does anything.
2. **Multi-line input is impossible** — `Enter` always submits, paste events are dropped by the event handler. Users cannot compose multi-paragraph prompts.
3. **Streaming loses intermediate AI text in multi-round agentic loops** — text streamed in earlier rounds (e.g. "Let me check that file first…") is replaced by only the final round's content when `StreamComplete` fires.

Each of these is a narrow fix (~30 lines). Fixing all three would put the app at a confident 100%.

---

## 1. Verification methodology

What was verified:

- **Static analysis** of all 16 Rust files (full read-through)
- `cargo check` — clean
- `cargo clippy --all-targets -- -D warnings` — clean
- `cargo clippy --release -- -W clippy::pedantic` — 43 warnings, all cast-precision or `needless_pass_by_value` (no correctness issues)
- `cargo test --no-fail-fast` — 3 passed (`llm::types::wire_format_tests::*`), 0 failed
- `cargo build --release` — produced a 7.2 MB Mach-O arm64 binary at `target/release/foxmayn_cowork_cli`
- `cargo fmt --check` — could not run; rustfmt component not installed in the active toolchain (advisory only — see §5.B.4)
- Cross-reference against `MVP_REPORT.md` and `MVP_COMPLETION_PLAN.md`
- Cross-reference against `.claude/skills/rust-best-practices/SKILL.md` and `.claude/skills/rust-async-patterns/SKILL.md`

What was **not** verified (out of scope per user instruction — left for user TUI testing):

- End-to-end LLM round-trips against OpenRouter or Ollama
- Live streaming behavior with real network latency
- TUI rendering under different terminal sizes / Unicode-narrow terminals
- Actual file-system mutations under concurrent operations
- Behavior with extremely large files (5 MB read cap aside) or deep directory trees

---

## 2. Build & test output (raw)

```
$ cargo check
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 1.21s

$ cargo clippy --all-targets -- -D warnings
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 1.04s

$ cargo test --no-fail-fast
running 3 tests
test llm::types::wire_format_tests::ollama_think_serializes_like_api_docs   ... ok
test llm::types::wire_format_tests::openrouter_reasoning_effort_is_lowercase ... ok
test llm::types::wire_format_tests::openrouter_reasoning_summary_is_lowercase ... ok

test result: ok. 3 passed; 0 failed; 0 ignored

$ cargo build --release
    Finished `release` profile [optimized] target(s) in 34.71s
```

Tests cover only `llm/types.rs` wire-format serialization. There are **no tests for** `fs.rs`, `tools.rs`, `app.rs`, or the streaming parsers — see §4.B.6.

---

## 3. Phase-by-phase verification

| Phase | Topic | Status | Notes |
|---|---|---|---|
| **1** | Auto-refresh file tree after LLM responses | ✅ Implemented | `tui/mod.rs:202` & `:216` — checks `LlmOutcome::Complete` for both batch and streaming paths. **Caveat:** discards expansion state (see §4.A.1). |
| **2** | `create_directory`, `copy_file`, `search_in_files` | ✅ Implemented | `fs.rs:80–157`, all wired into `tools.rs:101–141` and `dispatch_tool_call` `known_tools` array (`tools.rs:220–231`). `regex = "1"` added to `Cargo.toml`. None in `DESTRUCTIVE_OPS`. |
| **3** | `patch_file` (search & replace) | ✅ Implemented | `fs.rs:159` (0/1/N matching), `tools.rs:142` (tool def), `tools.rs:161` (in `DESTRUCTIVE_OPS`), system prompt updated at `app.rs:994`. |
| **4** | Expandable subdirectory navigation | ✅ Implemented | `TreeEntry` at `app.rs:14`, `toggle_expand`/`collapse_dir`/`handle_subdir_loaded`/`jump_to_parent` at `app.rs:309–363`, keyboard shortcuts at `tui/mod.rs:163–177`, indented widget render at `tui/widgets/file_tree.rs:47–71`. |
| **5** | Streaming responses | ✅ Implemented | `StreamChunk` at `llm/types.rs:147`. SSE parser at `openrouter.rs:104–155`, NDJSON parser at `ollama.rs:67–120`. `run_agentic_loop_streaming` at `app.rs:585`. UI integration at `tui/mod.rs:213–223`, streaming render at `chat.rs:101–134`. **Plan deviation:** no `--streaming` CLI flag; env var `STREAMING` only. |
| **6** | Context-window guard | ✅ Implemented | `estimate_tokens` at `llm/mod.rs:45`, `check_context` at `tui/mod.rs:272`, `ChatRole::Warning` at `app.rs:60` and rendered in `chat.rs:66–72`. **Plan deviation:** check lives in the spawn site (`tui/mod.rs`) rather than in `send_message`/`confirm_tool` per the plan, so it counts `conversation` only — the system prompt (~175 tokens) is excluded from the estimate. Functionally fine. |

### 3.1 Plan deviations summary

Two minor deviations from `MVP_COMPLETION_PLAN.md`. Neither is functionally damaging:

| # | Deviation | Plan said | Implemented | Impact |
|---|---|---|---|---|
| D1 | `--streaming` CLI flag | "Add `--streaming` CLI flag to `Cli` struct in `main.rs`" | Only env var `STREAMING` exists — `Cli` in `main.rs:20–32` only has `--dir`/`--provider`/`--model` | Can't toggle streaming per-invocation. Use `STREAMING=false cargo run --` instead. **Trivial to add.** |
| D2 | Context guard location | "In `send_message` (and `send_message_streaming`), after building the `working` messages vec" | Lives in `tui/mod.rs::check_context`, runs against `app.conversation` (system prompt excluded) | Tiny token undercount (~175 tokens). Functionally fine. |

---

## 4. Independent analysis — issues neither prior report flagged

This is a fresh review of the **current code** (not against the plan). Items are tagged by severity:

- **P0** — affects the headline feature in a way a first-time user will hit immediately
- **P1** — visible UX rough edge
- **P2** — Beta-readiness; not MVP-blocking

### 4.A — P0 (MVP-blocking, in my assessment)

#### 4.A.1 — Auto-refresh wipes file-tree expansion state

`tui/mod.rs:209` and `:220` re-issue `spawn_file_tree_load(dir, tx.clone())` after every successful LLM round. That triggers `App::handle_file_tree` (`app.rs:239–255`), which **rebuilds `self.file_tree` from scratch**, mapping every entry to `TreeEntry::from_file_entry(e, 0)` — `expanded: false` (`app.rs:33`).

Result: every time the AI does anything, every directory the user expanded snaps closed and `file_tree_scroll` resets to 0 (`app.rs:246`). For a project with a deep `src/` tree, the user can't even see the new file the AI just created without re-expanding manually — which defeats the whole point of Phase 1.

**Fix sketch:** before calling `spawn_file_tree_load`, snapshot `expanded` paths into a set; in `handle_file_tree`, after rebuilding, re-spawn `spawn_subdir_load` for each previously expanded dir. Or implement a less destructive "merge" that preserves expansion+scroll. ~30 lines.

#### 4.A.2 — Single-line input only; paste events dropped

`tui/mod.rs:138` intercepts every `KeyCode::Enter` — modifier-blind — and submits the message. `tui-textarea` never gets a chance to insert a newline. Worse, `tui/mod.rs:99–115` matches only `Event::Mouse` and `Event::Key`; **`Event::Paste`** (and bracketed-paste sequences from `crossterm`'s `event-stream` feature) hits the catch-all `_ => return`. Multi-line paste is silently swallowed.

For a chat-based AI app where prompts are often multi-paragraph (system instructions, code snippets, error logs), this is a real friction point. Common fix:

```rust
KeyCode::Enter
    if !key.modifiers.contains(KeyModifiers::SHIFT)
    && !key.modifiers.contains(KeyModifiers::ALT) =>
{
    /* submit */
}
KeyCode::Enter => { textarea.input(key); /* newline */ }
```

…and add an `Event::Paste(text)` arm that calls `textarea.insert_str(&text)`.

#### 4.A.3 — Streaming loses intermediate AI text across agentic rounds

`run_agentic_loop_streaming` (`app.rs:585`) loops up to 10 rounds. On each round it forwards `ContentDelta` chunks via `tx.send(AppEvent::StreamChunk(...))` (`app.rs:627`). `App::handle_stream_chunk` (`app.rs:170`) appends to `streaming_text` — **and never resets between rounds**. Only the final `AppEvent::StreamComplete` calls `finalize_stream()` (`app.rs:179`), which clears the buffer entirely; `handle_outcome` then writes only the **last** round's `assistant_message` to `chat_messages`.

User-visible effect: in a flow like

> Round 1 (streamed): "I'll list the files first…" → tool_call(`list_files`)
> Round 2 (streamed): "Found 5 files. Reading main.rs…" → tool_call(`read_file`)
> Round 3 (streamed): "Here's the summary…"

…the user sees a growing concatenated bubble, then at the very end the bubble vanishes and is replaced by **only** "Here's the summary…". Tool descriptions and tool results are never surfaced as chat entries either.

This is uniquely bad in streaming mode because non-streaming has nothing visible to lose during multi-round work. The streaming version promises live progress and then yanks it back.

**Fix sketch:** when the streaming loop detects `has_tool_calls` and is about to start a new round, send a new event (e.g. `AppEvent::FlushStreamingAsAssistant`) that flushes `streaming_text` into `chat_messages` as a `ChatRole::Assistant` entry, then `streaming_text = None`. Optionally also push tool descriptions as `ChatRole::Tool` entries between rounds. ~40 lines.

### 4.B — P1 (MVP-polish, important)

#### 4.B.1 — Missing-API-key first-run experience

`config::Config::from_env` (`config.rs:109`) reads `OPENROUTER_API_KEY` into `Option<String>`. If unset, the app launches normally; the user types a message, and `openrouter::chat` returns "OpenRouter returned 401: …" via `LlmError`. The chat shows that raw HTTP body. For a tool whose first-run experience is critical, a startup check ("`OPENROUTER_API_KEY` not set — see README §Setup") would be a much friendlier failure mode.

`MVP_REPORT.md §4` flagged this as a Beta concern; I'd argue it's MVP — the very first interaction can fail cryptically.

#### 4.B.2 — `patch_file` confirmation hides the diff

`tools.rs::build_description` (line 408) produces:

```
Patch file src/foo.rs (search & replace)
```

The user is asked to approve a destructive op without seeing **what** is being searched and replaced. For `delete_file` and `edit_file` (full overwrite), the file path alone is informative; for `patch_file` it isn't. A two-line description with truncated `search` and `replace` strings would be a small but meaningful upgrade for the most modern of the destructive tools.

#### 4.B.3 — No request cancellation

There's no Esc handler in `InputMode::Editing` to cancel an in-flight request. If the user submits a prompt and the LLM hangs (slow Ollama model, network latency, long agentic chain), they're stuck watching `Streaming…` or `Waiting for response…` until it completes or the process is killed. Pairing this with §4.B.4 (no HTTP timeout) makes accidental Ollama runs especially painful.

#### 4.B.4 — `reqwest::Client::new()` has no timeout

`app.rs:143` constructs the HTTP client with no `timeout()` builder call. A hung server (Ollama on a model that hasn't been pulled yet, OpenRouter under regional issues) blocks the spawned task indefinitely. A `Client::builder().timeout(Duration::from_secs(120)).build()` would be a single-line fix.

#### 4.B.5 — `MAX_READ_BYTES = 5 MB` is hard-coded and silently caps the AI's view

`fs.rs:7` rejects reads above 5 MB with a `ToolValidation` error. Not configurable, no chunking. For projects with large generated files (build logs, lockfiles, JSON datasets), the AI literally can't see them. Either expose `MAX_READ_BYTES` via env var or add a `read_file_range(path, start, end)` tool.

#### 4.B.6 — Zero coverage on the security-critical path

`tools.rs::validate_path_containment` (line 179) is the **single safety boundary** preventing the LLM from writing outside the working directory. It uses `canonicalize` only when the path exists; non-existent paths are checked by simple `starts_with`. There are **no tests** for `..` rejection, symlink escapes, or relative-vs-absolute path mixing.

For an MVP that's about to be put in front of users, "the AI can write anywhere on the filesystem" is the kind of bug you really want a test suite to catch ahead of you. Even three integration-style tests in `tools.rs` would close most of the risk.

### 4.C — P2 (Beta-readiness)

#### 4.C.1 — `MVP_REPORT.md §4` items still open

Out of the seven Beta-readiness items in the original report, only one (the file-tree refresh, which was an MVP item) was addressed. Still open:

- `cargo install` / pre-built binaries
- Loading indicator with elapsed time
- Mouse click-to-focus (mouse scroll **is** implemented at `tui/mod.rs:99–110`)
- Integration tests with a mock HTTP server
- `just doctor` smoke-test binary
- Animated GIF / screenshot in README
- User guide

#### 4.C.2 — README is now significantly stale

The README does not document any of the Phase 2–6 features:

- No mention of `create_directory`, `copy_file`, `search_in_files`, `patch_file`
- No mention of streaming or the `STREAMING` env var
- No mention of `CONTEXT_MAX_TOKENS` / `CONTEXT_WARN_RATIO`
- File tree navigation (Right/Left/Enter) is documented in `CLAUDE.md` but not the user-facing README

#### 4.C.3 — `Cargo.toml` lacks publishable metadata

```toml
[package]
name = "foxmayn_cowork_cli"
version = "0.1.0"
edition = "2024"
```

No `description`, `authors`, `license`, `repository`, or `readme` fields. Required for any future `cargo publish` or `cargo install`.

#### 4.C.4 — Two ~600-line agentic loops are 90% duplicated

`run_agentic_loop` (`app.rs:442`) and `run_agentic_loop_streaming` (`app.rs:585`) share most logic — tool dispatch, conversation tracking, error handling, max-rounds. A trait or generic over the LLM call site would eliminate the duplication. Not urgent, but the duplication will rot if either path gets a bugfix and the other is forgotten.

#### 4.C.5 — `AppError::OllamaRequest` is misnamed

`error.rs:7` declares `OllamaRequest(#[from] reqwest::Error)`, but the `#[from]` makes this variant the auto-conversion target for **any** `reqwest::Error`, including OpenRouter ones. Misleading when reading errors. Renaming to `Http` or `Provider` would fix it.

#### 4.C.6 — Stream parsers silently swallow malformed JSON lines

Both `openrouter.rs:131` (`let Ok(resp) = serde_json::from_str::<SseResponse>(data) else { continue };`) and `ollama.rs:89` (`let Ok(parsed) = serde_json::from_str::<NdjsonLine>(&line) else { continue };`) drop unparsable lines without logging. SSE/NDJSON streams legitimately contain unrelated frames (role-only deltas, keepalives), so silent skipping is partially correct — but a malformed-on-purpose response would be invisible. Even a `tracing::warn!` would help future debugging.

#### 4.C.7 — Pedantic clippy hot spots

`cargo clippy -- -W clippy::pedantic` produces 43 warnings (default clippy is clean). Notable repeating patterns:

- `as` casts that could lose precision (`tui/mod.rs:281`, `:296` for the context-warn percentage)
- `needless_pass_by_value` on `handle_crossterm_event(event: Event, …)` (`tui/mod.rs:93`)
- `cast_possible_truncation` on chat scroll math (`chat.rs:167`)

None affect correctness in practice (the values are small), but a `#[expect(clippy::cast_precision_loss, reason = "…")]` with a justification (per `rust-best-practices` Chapter 1) would be cleaner than implicit `as`.

#### 4.C.8 — No `cargo fmt` toolchain in CI-like config

`cargo fmt --check` fails locally with "rustfmt is not installed for the toolchain." `Justfile` defines `fmt-check` and `ci`, so any contributor running `just ci` will get the same failure. Add `rustfmt` to `rust-toolchain.toml` or document `rustup component add rustfmt` in the README.

---

## 5. Recommendations to reach a confident "100% MVP"

Concrete next steps in priority order. Time estimates are conservative and assume a Rust-comfortable engineer working in this codebase.

### 5.A — Must-fix to claim 100% (≈ 2–3 hours total)

1. **Preserve tree expansion across auto-refresh** (§4.A.1) — snapshot expanded paths before reload, re-issue `spawn_subdir_load` for each after the root reloads. ~30 LOC, `app.rs` + `tui/mod.rs`.
2. **Allow multi-line input** (§4.A.2) — `KeyCode::Enter` modifier-aware split, plus an `Event::Paste(s)` arm that calls `textarea.insert_str(&s)`. ~10 LOC, `tui/mod.rs`.
3. **Flush streaming text between agentic rounds** (§4.A.3) — new `AppEvent::FlushStreamingAsAssistant` (or similar), emitted just before `working.push(assistant_msg)` in `run_agentic_loop_streaming`. ~40 LOC, `app.rs` + `tui/mod.rs`.

### 5.B — Should-fix before any external user demo (≈ 1–2 hours)

4. **Friendly missing-API-key message at startup** (§4.B.1) — check in `main.rs` after `Config::from_env`, exit with a one-paragraph message if `Provider::OpenRouter` and key is empty.
5. **HTTP client timeout** (§4.B.4) — single line in `App::new`: `reqwest::Client::builder().timeout(Duration::from_secs(120)).build().unwrap()`.
6. **`patch_file` confirmation shows search/replace** (§4.B.2) — extend `build_description` for `patch_file`.
7. **Add `--streaming` CLI flag** (D1) — close the plan deviation.

### 5.C — Nice-to-have for a pre-Beta polish pass

8. **README refresh** covering all Phase 2–6 features.
9. **Three smoke tests** for `validate_path_containment` (`..` rejection, absolute-path-in-base, absolute-path-outside-base).
10. **Esc-to-cancel** during loading/streaming.
11. **Dedup the two agentic loops** (§4.C.4).

---

## 6. Updated MVP / Beta progress

```
Plan-defined MVP completion:    ██████████  100% (all 6 phases land cleanly)
Independent MVP assessment:     █████████░  ~92% (3 P0 issues introduced)
Beta-readiness:                 █████░░░░░  ~50%  (mostly unchanged from V1 §4)
```

Compared to V1's `~80% MVP / ~50% Beta`, the implementation moved the codebase strongly forward on MVP completion. The remaining ~8% is concentrated in §4.A — three narrow regressions or gaps that the plan didn't anticipate but that surface as soon as the app is used for more than a single round-trip. They're each small, well-isolated, and can be fixed in a single afternoon.

---

## 7. What V1 got right that holds

- Path-containment validator at `tools.rs:179` is still the right safety invariant
- Confirmation flow for destructive ops (`delete_file`, `edit_file`, now also `patch_file`) is correct
- The agentic loop's max-10-round cap continues to bound runaway tool-call chains
- The fire-and-forget `tokio::spawn` + `mpsc` pattern (no `Arc<Mutex<App>>`) is clean and matches `rust-async-patterns/SKILL.md` recommendations
- Provider abstraction (OpenRouter `reasoning` vs Ollama `think`) is well-modeled; the wire-format tests at `llm/types.rs:248` are exactly the right shape

---

## Appendix A — File map

```
src/main.rs            50  CLI parsing, runtime entry
src/config.rs         151  Config, env var parsing
src/error.rs           17  AppError enum
src/fs.rs             177  Tool implementations against the filesystem
src/llm/mod.rs         69  Provider dispatch, token estimator, health check
src/llm/types.rs      298  Wire types (Message, ChatRequest, StreamChunk, …) + 3 tests
src/llm/tools.rs      414  Tool defs, dispatch, path containment
src/llm/openrouter.rs 232  OpenAI-compatible JSON + SSE streaming
src/llm/ollama.rs     176  Ollama JSON + NDJSON streaming
src/app.rs           1004  App state, agentic loops (×2), system prompt
src/tui/mod.rs        416  Event loop, spawners, context guard, key handling
src/tui/ui.rs          68  Layout
src/tui/widgets/chat.rs         175  Chat panel + streaming render
src/tui/widgets/file_tree.rs     83  Tree panel with indented expand/collapse
src/tui/widgets/confirmation.rs  78  y/n popup
─────────────────────────
Total:               3411
```

## Appendix B — Cross-reference to MVP_REPORT.md §3 checklist

| # | MVP_REPORT.md item | Status |
|---|---|---|
| 1 | Auto-refresh file tree | ✅ Done — but see §4.A.1 (expansion state lost on refresh) |
| 2 | `create_directory` | ✅ Done |
| 3 | Subdirectory expansion | ✅ Done |
| 4 | Streaming responses | ✅ Done — but see §4.A.3 (intermediate text lost) |
| 5 | Context window guard | ✅ Done |
| (extras from §2 of V1) | `copy_file`, `search_in_files`, `patch_file` | ✅ All implemented (the plan rolled these into Phase 2 + 3) |
