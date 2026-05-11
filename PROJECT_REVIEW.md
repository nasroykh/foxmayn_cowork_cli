# Project review — foxmayn_cowork_cli

Audit date: 2026-05-11  
**Bugs 1.1–1.7 fixed on 2026-05-11.** See individual items below for what changed.
Scope: full source tree under `src/`, plus `Cargo.toml`, `Justfile`, `.env.example`, `CLAUDE.md`, `README.md`.
Build status: `cargo check`, `cargo clippy -- -D warnings`, and `cargo test` all pass cleanly. Seven unit tests pass.

This is an honest, harsh review. The project is in good shape overall — small (~5.9k LOC), clean module boundaries, idiomatic Rust, no `unsafe` outside the unavoidable `LocalRuntime` Send/Sync impls, no `unwrap` panics on hot paths, proper tokio fire-and-forget model. There are still several real issues worth fixing.

Findings are ordered by severity, not by where I found them.

---

## 1. Real bugs (fix soon)

### 1.1 `edit_file` silently creates files that don't exist
`src/fs.rs:79-82`. The function is documented (in the tool description sent to the LLM and in `fs.rs`) as **overwriting an existing file** and the LLM is told to use `create_file` for new files. The implementation just calls `tokio::fs::write`, which creates the file if it's missing. This means:
- The LLM can call `edit_file new_file.txt` and the file is created — circumventing the explicit `create_file` contract.
- The confirmation popup says "Overwrite file X", but X may not exist, so the user sees the wrong description.

Fix: assert `fs::try_exists(&path).await?` is true and return `AppError::ToolValidation` otherwise.

### 1.2 In-flight LLM requests are not aborted when superseded
`src/tui/mod.rs:465,476,515,533`. Each spawn site does `app.current_request = Some(h)` without first aborting the previous handle. The only place `abort()` is called is in `App::cancel_request`. As a result:
- If the user submits a second prompt before the first finishes, the first task continues running — making API calls, doing FS operations, costing OpenRouter credits. Its `StreamChunk` / `StreamComplete` events are silently dropped because `is_active_request` rejects the stale `request_id`, but the work is still performed.
- Same applies to `confirm_tool` superseding a `send_message`.
- `set_working_dir` (`app.rs:545`) also doesn't cancel the in-flight request, so an old task can write its result back to the new conversation's mpsc channel (those events are also gated, but again the work happens).

Fix: extract a helper that calls `self.current_request.take().map(|h| h.abort())` and call it from every spawn site and from `set_working_dir`. Note this still has a small TOCTOU window for spawned tool tasks already past `dispatch_tool_call`; that's acceptable but the API-call waste is the real win.

### 1.3 `delete_many` is not atomic and gives misleading errors mid-failure
`src/fs.rs:94-106`. The bulk-rename path validates the full plan upfront (existence of sources, conflict-free destinations) before doing any work. `delete_many` just iterates and deletes one by one. If the third path fails (e.g. permission denied), the first two are already gone and the LLM sees `Error: Filesystem error: ...` — but the chat tool entry says `Deleted N items` was the intent. The user has no way to know which subset was destroyed.

Fix options:
- Cheap: pre-validate all paths exist before starting, then attempt deletions; report how many succeeded before the first failure.
- Better: same pre-validation, plus return a structured result like `Deleted 2 of 3 items; failed on '<path>': <err>`.

### 1.4 `.env.example` documents a non-existent default
`.env.example:15` — “Ollama only: /api/chat `think` (thinking models). Default: low.”
`src/config.rs:193-208` — `ollama_think_from_env` returns `None` when the env var is unset. There is no `low` default. This is doc drift that will mislead anyone setting up Ollama with a thinking model.

Fix: either change the comment to "Default: omitted (no `think` field sent)", or actually default to `Low` when `PROVIDER=ollama`. Both are defensible; pick one.

### 1.5 `CLAUDE.md` claims `just check` is the default Just recipe
`CLAUDE.md` says `just check          # cargo check (default)`. The actual `Justfile:1` has `default: help`. Trivial, but it's a project-instructions file claiming to be authoritative.

### 1.6 `find_files` returns at most `max_matches + 1` before erroring
`src/fs.rs:246-250`. The check is `if matches.len() > max_matches` after the push, so the 201st match is pushed before the error fires. The error message says "more than 200 files" — fine in spirit, but the cap is documented as 200 in `tool_definitions` (`tools.rs:8`) and the off-by-one means an internal vector grows one beyond the advertised cap. Trivial, but it's the kind of thing a careful reviewer notices.

Fix: `if matches.len() >= max_matches { return Err(…) }` before the push.

### 1.7 `AppError::OllamaRequest` is misnamed
`src/error.rs:7`. The `#[from] reqwest::Error` variant is named `OllamaRequest`, but it's used by OpenRouter, `hf_hub`, and every other HTTP call in the project. The user-facing error string ("Request failed: ...") is generic and fine; the variant name is misleading for anyone reading the error enum.

Fix: rename to `Http` or `Request`. Mechanical change.

---

## 2. Design smells worth fixing

### 2.1 Massive duplication between streaming and non-streaming code paths
`src/app.rs` contains four near-identical entry points:
- `send_message` / `send_message_streaming`
- `confirm_tool` / `confirm_tool_streaming`

And two near-identical agentic loops:
- `run_agentic_loop` (158 lines)
- `run_agentic_loop_streaming` (216 lines)

The only meaningful difference is "stream chunks live via `tx`" vs "wait for the whole assistant message". This is the biggest concrete refactor opportunity in the codebase: every time a tool-call or error-handling fix is made, it has to be done twice and the two implementations will drift.

Two possible approaches:
- Push the streaming flag into a single loop and feed the channel only when streaming is on. The assembly of `Message` from streamed `ToolCallDelta`s can be a separate helper that's a no-op in the non-streaming case (since the non-streaming `chat` already returns a fully-formed message).
- Have only the streaming path, and in non-streaming mode collect chunks into a synchronous accumulator before returning. This is what most LLM CLIs do; the non-streaming code path becomes essentially "spawn the streaming loop, drain the channel, return". Net code reduction is roughly 200 lines.

### 2.2 `app.rs` is 1448 lines and mixes too many concerns
It contains: the `App` struct + UI state mutators, file-tree mutation, event enums, agentic loop driver, system-prompt builder, tool-error formatter, working-directory summarizer, and 4 entry points. Reasonable split:
- `app/state.rs` — `App`, `ChatEntry`, `TreeEntry`, `InputMode`, `Panel`, scroll/expand methods.
- `app/events.rs` — `AppEvent`, `LlmOutcome`, `RequestId`, `handle_outcome`, `handle_stream_chunk`, `finalize_*`.
- `app/agentic.rs` — `run_agentic_loop[_streaming]`, helpers, `format_tool_error`.
- `app/system_prompt.rs` — `system_prompt`, `working_dir_summary`.
- `app/entry.rs` — `send_message[_streaming]`, `confirm_tool[_streaming]`.

This pairs naturally with fix 2.1.

### 2.3 `tools.rs` is 968 lines and could be split similarly
It currently holds: tool schemas (~250 lines), `dispatch_tool_call`, `execute_tool`, confirmation-description builder, brief/full description renderers, path validation, and tests. Splitting into:
- `tools/schema.rs` — `tool_definitions`, schemas only.
- `tools/dispatch.rs` — `dispatch_tool_call`, `execute_tool`, the `known_tools` list (currently duplicated implicitly).
- `tools/validate.rs` — `validate_path_containment`, `extract_*` helpers, the tests.
- `tools/descriptions.rs` — `build_description`, `brief_action`, `build_confirmation_description`, `truncate_for_display`, `preview_*`.

would make each file small enough that the schema and the dispatch can be read side by side without scrolling.

### 2.4 `dispatch_tool_call` validates tool names twice
`src/llm/tools.rs:481-505` has an explicit `known_tools` array and rejects unknown names with a list. Then `execute_tool` ends with `_ => Err(AppError::ToolValidation("Unknown tool '{}'"))`. Both branches are reachable in principle but never hit together — pick one. Recommendation: drop the `known_tools` allowlist and let `execute_tool`'s match be the single source of truth. The match itself enforces completeness.

### 2.5 `probe` subcommand reimplements the Ollama wire parser
`src/main.rs:197-255` defines its own `OllamaResp` / `OllamaMsg` types, then manually rebuilds a `Message` from the parsed JSON. This duplicates the logic in `src/llm/ollama.rs::normalize`. Also, `probe`:
- Is hardcoded to Ollama (`config.ollama_base_url`) even when `PROVIDER=openrouter`. This is documented in CLAUDE.md as deliberate, but the help text reads as if `--provider` flows through.
- `--dir` shadows the top-level `Cli::dir` because `Commands::Probe::dir` is defined separately. Running `cargo run -- --dir foo probe ...` ignores `foo`. Either remove the top-level `dir` when in `Probe` mode or remove `Probe`'s own `dir`.
- `.expect("HTTP request failed")` will panic on a transient network error. Fine for a debug command, but `?` + an `eprintln!` would be friendlier.

If probe is meant to be the long-term debugging path, it should: (a) reuse the real provider dispatcher (`llm::chat`), and (b) just pretty-print the raw response without re-parsing it. As written it's a parallel implementation that will rot.

### 2.6 `Config` is `Clone`d into every spawned task
`Config` has ~30 fields, several of which are `String`. Each `tokio::spawn` site clones it. Switching to `Arc<Config>` makes spawn sites cheaper and removes the implicit mental cost of "is this clone expensive?" The same applies to `Vec<Message>` (conversation history can be 100s of KB after a long session) — but that one is mutated by the task, so `Arc` is harder there.

### 2.7 `Provider::Local` import gating in `mod.rs` is awkward
`src/llm/mod.rs:7-11` and `src/llm/runtime.rs` are sprinkled with `#[cfg(feature = "local")]` on imports, fields, and the entire `local::*` module. Workable, but `Provider::Local` is always defined while its handler is conditional. A cleaner pattern is to keep `Provider::Local` always reachable and let the dispatcher return an `AppError` at runtime if the feature is off — which is already what `llm::chat` does. So the cfg gates on the `Provider` import in `runtime.rs:8` could go away if `LlmRuntime::build` checked `matches!(config.provider, Provider::Local)` unconditionally and returned the "rebuild with --features local" error there instead of in `main::preflight`. One source of truth.

### 2.8 Reading the working-directory listing on every turn is wasteful when the dir is large
`app.rs:706-734`'s `working_dir_summary` is called on every `send_message` and every `confirm_tool`. It re-reads the root dir, sorts entries, and trims to 40. For a deep project this is cheap, but it also adds 40 lines × ~30 chars = ~1.2 KB of tokens to every request. For a 10-round agentic conversation, the listing is in the prompt once (system message); the issue is mainly that on each NEW user turn the listing is rebuilt and the previous one is discarded. That's actually intentional — file state may have changed — so keep it. Just be aware that this is a small but real per-turn cost. Optionally: cache + invalidate on `FileTreeLoaded`.

---

## 3. Smaller code-quality observations

- **`fs.rs:190-193 / 240-243`**: directory-skip allowlist is hardcoded to `node_modules` and `target`. No `.git`, `dist`, `build`, `.venv`, `__pycache__`, `vendor`. For a tool meant to work in arbitrary user directories, this list will silently scan huge irrelevant trees. Hidden dirs (`.*`) are skipped, which catches `.git`, but a Python project's `__pycache__` is not hidden.
- **`fs.rs:177-201` `search_in_files`** loads every file fully into memory via `read_to_string`. There's a 5 MB cap on `read_file` but no cap here. A 500 MB log file in the search root will eat half a gig of RAM. Stream line-by-line, or at minimum cap on `metadata.len()` first.
- **`runtime.rs:25-28`**: `reqwest::Client::builder()...build().unwrap_or_default()`. If TLS init fails this silently swaps in a default client without the configured connect timeout. Use `.expect("reqwest client construction")` or propagate as `AppError`.
- **`reqwest::Client`** has no request timeout, only `connect_timeout`. A stalled provider can block forever. Add `.timeout(Duration::from_secs(60))` on the builder — streaming requests still work; the timeout applies to overall request, not to the streaming body, in `reqwest` 0.12.
- **`validate_path_containment`** calls `base_path.canonicalize()` on every path validation. For `delete_many`/`rename_many` with many paths, that's many syscalls returning the same answer. Pass a pre-canonicalized base through, or memoize.
- **`config.rs:122-130` `env_bool`** swallows unknown values silently (returns default). Other env parsers in this file `eprintln!` and fall back. Inconsistent.
- **`config.rs:298-312`**: in `with_overrides`, when the user changes `--provider`, the reasoning/think env vars are re-read. That's correct, but it means CLI-level provider switches behave subtly differently from the env-only path: e.g. `OPENROUTER_REASONING_EFFORT=high PROVIDER=ollama` followed by `--provider openrouter` re-reads the env var; setting only `PROVIDER=openrouter` from the start does the same; but if you set `--provider openrouter` and the same CLI run had previously assigned `openrouter_reasoning` from a different provider's env, this branch fires. Not a bug, but it's nontrivial. Worth a one-line comment.
- **`render` in `chat.rs:210-223`** computes line-wrap counts manually and feeds the result back into the scroll offset. `Paragraph` already wraps, so the offset semantics depend on this hand-rolled count matching ratatui's wrapper. As long as the wrap policy is `Wrap { trim: false }` and there are no tabs/double-width chars, this works; if ratatui's wrap changes, your scroll will drift silently. Consider using ratatui's `ScrollbarState` or `paragraph.line_count()` if available.
- **`brief_action`** is exposed as `pub fn` but only used inside the crate. Keep it crate-private.
- **`probe`'s `for round in 1..=10`** duplicates `MAX_TOOL_ROUNDS = 10` from `app.rs:775`. Promote to a `pub const` in `llm/mod.rs`.
- **Tests are concentrated in two files** (`tools.rs` path containment, `types.rs` wire format). The streaming SSE/NDJSON parsers, the bulk-rename validator, `format_tool_error`'s heuristics, `delete_many` failure modes — none have tests.

---

## 4. What's good (so it doesn't regress)

- Clear separation between provider-specific HTTP code and the shared dispatcher.
- Path containment validation is real (canonicalises, checks `..` components, rejects absolute paths outside the base) and has unit tests.
- Destructive-op gating is consistent: a static `DESTRUCTIVE_OPS` list drives both `dispatch_tool_call` and the system-prompt enumeration.
- The agentic loop has a hard round cap and a sensible empty-response error.
- Streaming and non-streaming paths both honor `cancel_request` and ignore stale events via `is_active_request`.
- Optional `local` feature is properly fenced; the build works without `cmake` when the feature is off.
- `--skip-confirmations` prints a loud warning, and the per-tool result is tagged `(confirmation skipped)` so the chat transcript is honest.
- `Cargo.toml` is lean — no unused dependencies, no `default-features = true` cargo cult.

---

## 5. Recommended priority order

1. Fix `edit_file` existence check (§1.1) — silent contract violation.
2. Abort in-flight requests on supersede (§1.2) — waste and correctness.
3. Add HTTP request timeout (§3) — one line, prevents hangs.
4. Make `delete_many` pre-validate paths (§1.3) — destructive correctness.
5. Documentation fixes (§1.4, §1.5) — five-minute changes.
6. Merge streaming / non-streaming agentic loops (§2.1) — biggest payoff for future maintenance.
7. Split `app.rs` and `tools.rs` into submodules (§2.2, §2.3).
8. Tighten `find_files` cap off-by-one (§1.6) and rename `AppError::OllamaRequest` (§1.7).
9. Cap `search_in_files` per-file size (§3) and broaden directory-skip list (§3).
10. Reuse provider dispatcher in `probe` (§2.5) instead of the parallel implementation.

Items 1–5 are small, mostly under 30 LOC each. Items 6–7 are the real refactor — together maybe a half-day of focused work and ~300 LOC net reduction.
