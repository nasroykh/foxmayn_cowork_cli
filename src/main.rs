mod app;
mod config;
mod error;
mod fs;
mod llm;
mod tui;

use std::path::PathBuf;

use clap::Parser;

use app::App;
use config::{Config, Provider};

#[derive(Parser)]
#[command(
    name = "foxmayn-cowork",
    about = "Local AI assistant for file management"
)]
struct Cli {
    /// Working directory to open at startup
    #[arg(short, long)]
    dir: Option<PathBuf>,

    /// LLM provider to use
    #[arg(long)]
    provider: Option<config::Provider>,

    /// Model name override
    #[arg(long)]
    model: Option<String>,

    /// Enable or disable streaming responses (overrides STREAMING env var)
    #[arg(long)]
    streaming: Option<bool>,

    /// Skip all destructive-operation confirmations. Dangerous: the AI can edit/delete/rename immediately.
    #[arg(long, action = clap::ArgAction::SetTrue)]
    skip_confirmations: bool,
}

#[tokio::main]
async fn main() {
    dotenvy::dotenv().ok();

    let cli = Cli::parse();
    let config = Config::from_env().with_overrides(
        cli.provider,
        cli.model,
        cli.streaming,
        cli.skip_confirmations,
    );

    if let Err(msg) = preflight(&config) {
        eprintln!("{msg}");
        std::process::exit(1);
    }
    if config.skip_confirmations {
        eprintln!(
            "WARNING: destructive-operation confirmations are disabled. The AI can edit, delete, rename, and bulk-operate without prompting."
        );
    }

    let mut app = App::new(config);

    if let Some(dir) = cli.dir {
        app.set_working_dir(dir);
    }

    if let Err(e) = tui::run(app).await {
        eprintln!("Fatal error: {e}");
        std::process::exit(1);
    }
}

/// Validate runtime preconditions before launching the TUI.
fn preflight(config: &Config) -> Result<(), String> {
    if matches!(config.provider, Provider::OpenRouter)
        && config
            .openrouter_api_key
            .as_deref()
            .unwrap_or("")
            .is_empty()
    {
        return Err("Error: OPENROUTER_API_KEY is not set.\n\n\
             Either:\n  \
                1. Add it to .env (run `just env` to copy from .env.example,\n     \
                   then fill in OPENROUTER_API_KEY=…)\n  \
                2. Set PROVIDER=ollama (or pass --provider ollama) to use a local\n     \
                   Ollama instance instead\n\n\
             See README.md for full setup instructions."
            .to_string());
    }
    Ok(())
}
