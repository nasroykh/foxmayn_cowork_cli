# Changelog

All notable changes to this project are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [1.0.0] - 2026-10-28

### Added

- `--version` flag.
- Prebuilt binaries for aarch64 Linux and aarch64 macOS. Windows on ARM uses the x64 build.
- Confirmation for creating or copying files and directories under hidden paths.
- Scrollable confirmation dialog with an "...and N more" marker, and a red CONFIRMATIONS OFF badge.
- `FOXMAYN_VERSION` pin and `FOXMAYN_COWORK_SKIP_CHECKSUM=1` override in the install scripts; `GITHUB_TOKEN` is optional.
- `just test` and `just audit` recipes; `just ci` now runs tests.
- CI: test matrix, MSRV (1.89) check, `local` feature check and `cargo audit`.
- Changelog and security policy.

### Changed

- Config is read only from `~/.config/foxmayn-cowork/.env`. A `.env` in the current directory or its parents is ignored. `just run` still loads the repo `.env`.
- `--provider openrouter` is the canonical name; `open-router` still works.
- `OLLAMA_THINK` is no longer sent by default. If set and the model cannot think, it is dropped with a warning.
- A saved `/model` is stored per provider and no longer overrides an explicit `--model`.
- Per-project storage key is now a readable path tail plus a hash of the canonical path.
- Per-project session databases moved to the new collision-free key, and sessions saved by 0.x are not migrated: `/sessions` does not list them. The old files stay on disk in the `projects/` folder under the data directory (`~/.local/share/foxmayn-cowork/` on Linux) and can be deleted.
- HTTP: 15 s connect timeout, 300 s idle read timeout, up to 2 retries on 429/502/503/504 honoring `Retry-After`.
- Upgraded ratatui 0.30, crossterm 0.29, ratatui-textarea (replaces tui-textarea), reqwest 0.13, rusqlite 0.41, pdf-extract 0.12, hf-hub 1.0, dirs 7.
- Release workflow is gated on checks and pins actions by SHA.

### Fixed

- Slash commands with arguments now run on Enter; key release events are ignored.
- Every tool call in a multi-call turn is answered exactly once. Declining a confirmation answers the rest as not executed.
- Chat rows wrap exactly and scroll is clamped.
- Ctrl+L, `/clear`, `/dir` and `/resume` cancel any in-flight request.
- SIGTERM and SIGHUP restore the terminal and quit cleanly.
- Streaming robustness for broken or partial responses.
- `local` provider: prompt size is validated, tokens are decoded statefully, ChatML is sanitized.
- Model listing for Ollama times out instead of hanging.
- Install scripts fail closed on a missing checksum and handle PATH updates safely.

### Security

- `cargo audit` went from 17 vulnerabilities to 0, including lopdf RUSTSEC-2026-0187 via pdf-extract 0.12.
- Search and find do not follow symlinks. Paths through symlinks that leave the working directory are rejected.
- `create_file` and `copy_file` refuse to overwrite; `rename_file` refuses an existing destination. The working directory cannot be deleted or renamed.
- Config file is written with mode 0600 in a 0700 directory. A repo-local `.env` can no longer change provider, base URL or confirmation policy.
- Search skips files over 5 MB and binary files; tool results sent to the model are capped at 100,000 characters.
