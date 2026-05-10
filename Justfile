default: help

# Show this help
help:
    @just --list --unsorted

# ── Dev ───────────────────────────────────────────────────────────────────────

# Run with a specific directory (required)
run dir:
    cargo run -- --dir {{dir}}

# Run pointing at the project itself (quick sanity check)
run-self:
    cargo run -- --dir .

# Run with Ollama instead of OpenRouter
run-ollama dir:
    cargo run -- --dir {{dir}} --provider ollama

# Run with a specific model override
run-model dir model:
    cargo run -- --dir {{dir}} --model {{model}}

# Run with fully-local llama.cpp inference (no API key or Ollama needed)
run-local dir=".":
    cargo run --features local -- --dir {{dir}} --provider local

# Same as `run` but skip all destructive-op confirmations (dangerous: AI can edit/delete/rename without prompting)
run-unsafe dir:
    cargo run -- --dir {{dir}} --skip-confirmations

run-self-unsafe:
    cargo run -- --dir . --skip-confirmations

run-ollama-unsafe dir:
    cargo run -- --dir {{dir}} --provider ollama --skip-confirmations

run-model-unsafe dir model:
    cargo run -- --dir {{dir}} --model {{model}} --skip-confirmations

# ── Build ─────────────────────────────────────────────────────────────────────

build:
    cargo build

release:
    cargo build --release

# ── Quality ───────────────────────────────────────────────────────────────────

check:
    cargo check

lint:
    cargo clippy -- -D warnings

fmt:
    cargo fmt

fmt-check:
    cargo fmt -- --check

# Run check + lint + fmt-check in one shot
ci: fmt-check lint check

# ── Misc ──────────────────────────────────────────────────────────────────────

# Local smoke check for a demo-ready checkout
doctor:
    #!/usr/bin/env sh
    set -eu
    command -v cargo >/dev/null || { echo "cargo is not installed"; exit 1; }
    command -v rustfmt >/dev/null || { echo "rustfmt is not installed; run: rustup component add rustfmt"; exit 1; }
    if [ ! -f .env ]; then
        echo "warning: .env is missing; run 'just env' before using OpenRouter"
    fi
    cargo fmt -- --check
    cargo clippy --all-targets -- -D warnings
    cargo test --no-fail-fast
    cargo build

clean:
    cargo clean

# Copy .env.example to .env if .env doesn't exist yet
env:
    #!/usr/bin/env sh
    if [ -f .env ]; then
        echo ".env already exists"
    else
        cp .env.example .env
        echo "Created .env from .env.example — fill in OPENROUTER_API_KEY"
    fi
