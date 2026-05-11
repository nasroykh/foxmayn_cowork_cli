mod app;
mod config;
mod error;
mod fs;
mod llm;
mod setup;
mod storage;
mod tui;

use std::path::PathBuf;
use std::sync::Arc;

use clap::{Parser, Subcommand};

use app::App;
use config::{Config, Provider};
use llm::runtime::LlmRuntime;

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

    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(Subcommand)]
enum Commands {
    /// Send one message and print raw request/response + tool results. No TUI.
    Probe {
        /// Message to send (default: "list all .md files")
        #[arg(default_value = "list all .md files")]
        message: String,
        /// Working directory for tool execution
        #[arg(short, long, default_value = ".")]
        dir: PathBuf,
    },
    /// Configure the AI provider and API keys interactively.
    Config,
}

#[tokio::main]
async fn main() {
    // CWD/.env takes precedence (dev workflow with `just run`).
    let loaded_from_cwd = dotenvy::dotenv().is_ok();

    let cli = Cli::parse();

    // `foxmayn-cowork config`: load existing config for defaults, run wizard, exit.
    if matches!(cli.command, Some(Commands::Config)) {
        if !loaded_from_cwd {
            if let Some(p) = setup::config_path() {
                dotenvy::from_path(&p).ok();
            }
        }
        let current = Config::from_env();
        if let Err(e) = setup::run_wizard(Some(&current)) {
            eprintln!("Config error: {e}");
            std::process::exit(1);
        }
        return;
    }

    // First-time setup: run wizard when no config file exists and stdin is a TTY.
    if !loaded_from_cwd && setup::needs_init() {
        if let Err(e) = setup::run_wizard(None) {
            eprintln!("Setup error: {e}");
            std::process::exit(1);
        }
    }

    // Load the config-dir .env (written by wizard or pre-existing).
    if !loaded_from_cwd {
        if let Some(p) = setup::config_path() {
            dotenvy::from_path(&p).ok();
        }
    }

    let base_config = Config::from_env().with_overrides(
        cli.provider,
        cli.model,
        cli.streaming,
        cli.skip_confirmations,
    );

    if let Some(Commands::Probe { message, dir }) = cli.command {
        probe(base_config, message, dir).await;
        return;
    }

    // Open persistent storage and layer saved settings on top of env/CLI config.
    let storage = storage::Storage::open();
    let config = storage::apply_saved_settings(base_config, &storage);

    if let Err(msg) = preflight(&config) {
        eprintln!("{msg}");
        std::process::exit(1);
    }
    if config.skip_confirmations {
        eprintln!(
            "WARNING: destructive-operation confirmations are disabled. The AI can edit, delete, rename, and bulk-operate without prompting."
        );
    }

    let llm_runtime = match LlmRuntime::build(&config).await {
        Ok(r) => r,
        Err(e) => {
            eprintln!("Failed to initialize LLM runtime: {e}");
            std::process::exit(1);
        }
    };

    let mut app = App::new(Arc::new(config), llm_runtime, storage);

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
                1. Set it in ~/.config/foxmayn-cowork/.env:\n     \
                   OPENROUTER_API_KEY=sk-or-…\n  \
                2. Export it in your shell profile:\n     \
                   export OPENROUTER_API_KEY=sk-or-…\n  \
                3. Set PROVIDER=ollama (or pass --provider ollama) to use a local\n     \
                   Ollama instance instead\n  \
                4. Set PROVIDER=local for fully offline inference (no API key required)\n\n\
             See README.md for full setup instructions."
            .to_string());
    }

    // Provider::Local validation moved to LlmRuntime::build so there is a single
    // check point and no cfg gates needed here.

    Ok(())
}

async fn probe(config: Config, message: String, dir: PathBuf) {
    use llm::tools::{dispatch_tool_call, execute_tool, tool_definitions};
    use llm::types::{ChatRequest, Message};

    let base_path = dir.canonicalize().unwrap_or(dir.clone());

    println!("=== PROBE ===");
    println!("provider : {:?}", config.provider);
    println!("model    : {}", config.model);
    println!("think    : {:?}", config.ollama_think);
    println!("dir      : {}", base_path.display());
    println!("message  : {message}");
    println!();

    // Probe always targets Ollama regardless of config.provider (intentional — documented).
    let ollama_config = Config {
        provider: config::Provider::Ollama,
        ..config.clone()
    };

    let runtime = match LlmRuntime::build(&ollama_config).await {
        Ok(r) => r,
        Err(e) => {
            println!("ERROR: failed to build LLM runtime: {e}");
            return;
        }
    };

    let mut messages: Vec<Message> = vec![Message::user(&message)];

    for round in 1..=llm::MAX_TOOL_ROUNDS {
        let request = ChatRequest {
            model: ollama_config.model.clone(),
            messages: messages.clone(),
            tools: tool_definitions(),
            stream: false,
            reasoning: ollama_config.openrouter_reasoning.clone(),
            think: ollama_config.ollama_think,
        };

        println!("--- REQUEST (round {round}) ---");
        println!("{}", serde_json::to_string_pretty(&request).unwrap());
        println!();

        let assistant_msg = match llm::chat(&runtime, &request, &ollama_config).await {
            Ok(msg) => msg,
            Err(e) => {
                println!("ERROR: {e}");
                break;
            }
        };

        // Pretty-print the returned message as JSON for inspection
        println!("--- RESPONSE (round {round}) ---");
        match serde_json::to_string_pretty(&assistant_msg) {
            Ok(s) => println!("{s}"),
            Err(_) => println!("{assistant_msg:?}"),
        }
        println!();

        let has_tool_calls = assistant_msg
            .tool_calls
            .as_ref()
            .is_some_and(|c| !c.is_empty());

        println!("content      : {:?}", assistant_msg.content);
        println!(
            "tool_calls # : {}",
            assistant_msg.tool_calls.as_ref().map_or(0, Vec::len)
        );
        println!();

        messages.push(assistant_msg.clone());

        if !has_tool_calls {
            println!("=== DONE (no tool calls) ===");
            if assistant_msg.content.trim().is_empty() {
                println!("WARNING: empty content + no tool calls -> this is the bug");
            }
            break;
        }

        // Execute tools
        for tc in assistant_msg.tool_calls.unwrap_or_default() {
            println!("--- TOOL CALL: {} ---", tc.function.name);
            println!("args: {}", tc.function.arguments);

            match dispatch_tool_call(&tc.function, &base_path).await {
                Ok(r) if r.requires_confirmation => {
                    println!("result: <needs confirmation — executing unconditionally in probe>");
                    match execute_tool(&tc.function, &base_path).await {
                        Ok(output) => {
                            println!("executed: {output}");
                            messages.push(Message::tool_result(
                                &tc.function.name,
                                &output,
                                tc.id.clone(),
                            ));
                        }
                        Err(e) => {
                            println!("tool error: {e}");
                            messages.push(Message::tool_result(
                                &tc.function.name,
                                format!("Error: {e}"),
                                tc.id.clone(),
                            ));
                        }
                    }
                }
                Ok(r) => {
                    let result_str = r.result.as_deref().unwrap_or("");
                    println!("result: {result_str}");
                    messages.push(Message::tool_result(
                        &tc.function.name,
                        result_str,
                        tc.id.clone(),
                    ));
                }
                Err(e) => {
                    println!("tool error: {e}");
                    messages.push(Message::tool_result(
                        &tc.function.name,
                        format!("Error: {e}"),
                        tc.id.clone(),
                    ));
                }
            }
            println!();
        }
    }
}
