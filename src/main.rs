mod app;
mod config;
mod error;
mod fs;
mod llm;
mod tui;

use std::path::PathBuf;

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

    if let Some(Commands::Probe { message, dir }) = cli.command {
        probe(config, message, dir).await;
        return;
    }

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

    let mut app = App::new(config, llm_runtime);

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
                   Ollama instance instead\n  \
                3. Build with --features local and set PROVIDER=local for fully\n     \
                   offline inference (no API key required)\n\n\
             See README.md for full setup instructions."
            .to_string());
    }

    if matches!(config.provider, Provider::Local) {
        #[cfg(not(feature = "local"))]
        return Err(
            "Error: PROVIDER=local requires building with the `local` feature.\n\n\
             Rebuild with:\n  cargo build --release --features local\n\n\
             Or run directly:\n  cargo run --features local -- --provider local --dir <path>"
                .to_string(),
        );
    }

    Ok(())
}

async fn probe(config: Config, message: String, dir: PathBuf) {
    use llm::tools::{dispatch_tool_call, tool_definitions};
    use llm::types::{ChatRequest, Message};

    let client = reqwest::Client::new();
    let base_path = dir.canonicalize().unwrap_or(dir.clone());

    println!("=== PROBE ===");
    println!("provider : {:?}", config.provider);
    println!("model    : {}", config.model);
    println!("think    : {:?}", config.ollama_think);
    println!("dir      : {}", base_path.display());
    println!("message  : {message}");
    println!();

    let mut messages: Vec<Message> = vec![Message::user(&message)];

    for round in 1..=10 {
        let request = ChatRequest {
            model: config.model.clone(),
            messages: messages.clone(),
            tools: tool_definitions(),
            stream: false,
            reasoning: config.openrouter_reasoning.clone(),
            think: config.ollama_think,
        };

        println!("--- REQUEST (round {round}) ---");
        println!("{}", serde_json::to_string_pretty(&request).unwrap());
        println!();

        // Send raw HTTP so we can print the body before parsing
        let url = format!("{}/api/chat", config.ollama_base_url);
        let raw = client
            .post(&url)
            .json(&request)
            .send()
            .await
            .expect("HTTP request failed");

        let status = raw.status();
        let body = raw.text().await.unwrap_or_default();

        println!("--- RESPONSE (round {round}) status={status} ---");
        // Pretty-print if valid JSON, raw otherwise
        match serde_json::from_str::<serde_json::Value>(&body) {
            Ok(v) => println!("{}", serde_json::to_string_pretty(&v).unwrap()),
            Err(_) => println!("{body}"),
        }
        println!();

        if !status.is_success() {
            println!("ERROR: non-2xx status, stopping.");
            break;
        }

        // Parse into our types
        #[derive(serde::Deserialize)]
        struct OllamaResp {
            message: OllamaMsg,
        }
        #[derive(serde::Deserialize)]
        struct OllamaMsg {
            role: String,
            #[serde(default)]
            content: String,
            tool_calls: Option<Vec<serde_json::Value>>,
        }

        let parsed: OllamaResp = match serde_json::from_str(&body) {
            Ok(p) => p,
            Err(e) => {
                println!("PARSE ERROR: {e}");
                break;
            }
        };

        let tool_calls = parsed.message.tool_calls.unwrap_or_default();
        println!("content      : {:?}", parsed.message.content);
        println!("tool_calls # : {}", tool_calls.len());
        println!();

        // Push assistant message
        let assistant_msg = Message {
            role: parsed.message.role,
            content: parsed.message.content.clone(),
            tool_calls: if tool_calls.is_empty() {
                None
            } else {
                Some(
                    tool_calls
                        .iter()
                        .map(|tc| {
                            let name = tc
                                .get("function")
                                .and_then(|f| f.get("name"))
                                .and_then(|n| n.as_str())
                                .unwrap_or("")
                                .to_string();
                            let arguments = tc
                                .get("function")
                                .and_then(|f| f.get("arguments"))
                                .cloned()
                                .unwrap_or(serde_json::Value::Object(Default::default()));
                            llm::types::ToolCall {
                                id: None,
                                function: llm::types::FunctionCall { name, arguments },
                            }
                        })
                        .collect(),
                )
            },
            name: None,
            tool_call_id: None,
        };
        messages.push(assistant_msg.clone());

        if tool_calls.is_empty() {
            println!("=== DONE (no tool calls) ===");
            if parsed.message.content.trim().is_empty() {
                println!("WARNING: empty content + no tool calls -> this is the bug");
            }
            break;
        }

        // Execute tools
        for tc_val in &tool_calls {
            let name = tc_val
                .get("function")
                .and_then(|f| f.get("name"))
                .and_then(|n| n.as_str())
                .unwrap_or("?");
            let arguments = tc_val
                .get("function")
                .and_then(|f| f.get("arguments"))
                .cloned()
                .unwrap_or_default();

            println!("--- TOOL CALL: {name} ---");
            println!("args: {arguments}");

            let fc = llm::types::FunctionCall {
                name: name.to_string(),
                arguments,
            };
            match dispatch_tool_call(&fc, &base_path).await {
                Ok(r) => {
                    println!(
                        "result: {}",
                        r.result.as_deref().unwrap_or("<needs confirmation>")
                    );
                    let tool_result = Message::tool_result(
                        &fc.name,
                        r.result.as_deref().unwrap_or("requires_confirmation: true"),
                        None,
                    );
                    messages.push(tool_result);
                }
                Err(e) => {
                    println!("tool error: {e}");
                    messages.push(Message::tool_result(&fc.name, format!("Error: {e}"), None));
                }
            }
            println!();
        }
    }
}
