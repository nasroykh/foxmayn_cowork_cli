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
        /// Use the streaming chat path (chat_stream + assembler) instead of one-shot chat.
        #[arg(long, action = clap::ArgAction::SetTrue)]
        stream: bool,
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
        if !loaded_from_cwd && let Some(p) = setup::config_path() {
            dotenvy::from_path(&p).ok();
        }
        let current = Config::from_env();
        if let Err(e) = setup::run_wizard(Some(&current)) {
            eprintln!("Config error: {e}");
            std::process::exit(1);
        }
        return;
    }

    // First-time setup: run wizard when no config file exists and stdin is a TTY.
    if !loaded_from_cwd
        && setup::needs_init()
        && let Err(e) = setup::run_wizard(None)
    {
        eprintln!("Setup error: {e}");
        std::process::exit(1);
    }

    // Load the config-dir .env (written by wizard or pre-existing).
    if !loaded_from_cwd && let Some(p) = setup::config_path() {
        dotenvy::from_path(&p).ok();
    }

    let base_config = Config::from_env().with_overrides(
        cli.provider,
        cli.model,
        cli.streaming,
        cli.skip_confirmations,
    );

    if let Some(Commands::Probe {
        message,
        dir,
        stream,
    }) = cli.command
    {
        probe(base_config, message, dir, stream).await;
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

    if matches!(config.provider, Provider::Ollama) {
        match llm::ollama::model_supports_tools(
            &llm_runtime.http_client,
            &config.ollama_base_url,
            &config.model,
        )
        .await
        {
            Ok(true) => {}
            Ok(false) => {
                eprintln!(
                    "Error: Ollama model '{}' does not declare tool-calling support.\n\n\
                     This app drives a tool-calling agent loop; running against a model whose\n\
                     chat template does not render `role: tool` messages will silently break\n\
                     after the first tool call (the model loses sight of the tool result and\n\
                     the original user request).\n\n\
                     Pick a model whose `capabilities` includes `tools` — e.g. `qwen2.5:7b`,\n\
                     `llama3.1:8b`, `mistral-nemo`. Run `ollama show <model>` to verify, or\n\
                     re-run `foxmayn-cowork config` to change the model.",
                    config.model
                );
                std::process::exit(1);
            }
            Err(e) => {
                eprintln!(
                    "Warning: could not verify tool-calling support for Ollama model '{}': {e}\n\
                     Proceeding anyway — if the model misbehaves after tool calls, switch to a\n\
                     tool-capable model (e.g. qwen2.5:7b, llama3.1:8b).",
                    config.model
                );
            }
        }
    }

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

async fn probe(config: Config, message: String, dir: PathBuf, stream: bool) {
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
            stream,
            reasoning: ollama_config.openrouter_reasoning.clone(),
            think: ollama_config.ollama_think,
        };

        println!("--- REQUEST (round {round}) ---");
        println!("{}", serde_json::to_string_pretty(&request).unwrap());
        println!();

        let assistant_msg = if stream {
            match probe_stream_one(&runtime, &request, &ollama_config).await {
                Ok(msg) => msg,
                Err(e) => {
                    println!("ERROR: {e}");
                    break;
                }
            }
        } else {
            match llm::chat(&runtime, &request, &ollama_config).await {
                Ok(msg) => msg,
                Err(e) => {
                    println!("ERROR: {e}");
                    break;
                }
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

/// Streaming probe path: drive `llm::chat_stream`, print chunks live, and assemble the same
/// final `Message` the agentic loop would build. Mirrors `app::agentic::run_agentic_loop`'s
/// streaming branch so the probe exercises the exact codepath that runs in the TUI.
async fn probe_stream_one(
    runtime: &LlmRuntime,
    request: &llm::types::ChatRequest,
    config: &Config,
) -> Result<llm::types::Message, error::AppError> {
    use llm::types::{FunctionCall, Message, StreamChunk, ToolCall};
    use tokio::sync::mpsc;

    struct Partial {
        id: Option<String>,
        name: Option<String>,
        args: String,
    }

    let (tx, mut rx) = mpsc::unbounded_channel::<StreamChunk>();
    llm::chat_stream(runtime, request, config, tx).await?;

    let mut content = String::new();
    let mut partials: Vec<Partial> = Vec::new();

    println!("--- STREAM CHUNKS ---");
    while let Some(chunk) = rx.recv().await {
        match chunk {
            StreamChunk::ContentDelta(t) => {
                print!("{t}");
                use std::io::Write;
                std::io::stdout().flush().ok();
                content.push_str(&t);
            }
            StreamChunk::ThinkingDelta(t) => {
                eprintln!("[thinking] {t}");
            }
            StreamChunk::ToolCallDelta {
                index,
                id,
                name,
                arguments_fragment,
            } => {
                while partials.len() <= index {
                    partials.push(Partial {
                        id: None,
                        name: None,
                        args: String::new(),
                    });
                }
                let p = &mut partials[index];
                if id.is_some() {
                    p.id = id;
                }
                if name.is_some() {
                    p.name = name;
                }
                p.args.push_str(&arguments_fragment);
                println!(
                    "[tool-delta] idx={index} name={:?} args_so_far={:?}",
                    p.name, p.args
                );
            }
            StreamChunk::Done => break,
            StreamChunk::Error(e) => {
                return Err(error::AppError::LlmError(format!("stream error: {e}")));
            }
        }
    }
    println!();
    println!("--- /STREAM CHUNKS ---");

    let tool_calls = if partials.is_empty() {
        None
    } else {
        let mut out = Vec::with_capacity(partials.len());
        for p in partials {
            let name = p.name.clone().unwrap_or_default();
            let arguments: serde_json::Value = serde_json::from_str(&p.args).map_err(|e| {
                error::AppError::LlmError(format!(
                    "tool arguments parse failed for '{name}': {e} (raw={:?})",
                    p.args
                ))
            })?;
            out.push(ToolCall {
                id: p.id,
                r#type: "function".into(),
                function: FunctionCall { name, arguments },
            });
        }
        Some(out)
    };

    Ok(Message {
        role: "assistant".into(),
        content,
        tool_calls,
        name: None,
        tool_call_id: None,
    })
}
