default: check

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
