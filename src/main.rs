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
    version,
    about = "Local AI assistant for file management"
)]
struct Cli {
    /// Working directory to open at startup
    #[arg(short, long, global = true)]
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
    /// Send one message and print raw request/response + tool results. No TUI. Tools that need
    /// confirmation are never executed. Uses the top-level `--dir` (default: current directory).
    #[command(hide = true)]
    Probe {
        /// Message to send (default: "list all .md files")
        #[arg(default_value = "list all .md files")]
        message: String,
        /// Use the streaming chat path (chat_stream + assembler) instead of one-shot chat.
        #[arg(long, action = clap::ArgAction::SetTrue)]
        stream: bool,
    },
    /// Configure the AI provider and API keys interactively.
    Config,
}

/// Load the global config file (`~/.config/foxmayn-cowork/.env`). Variables already present in
/// the real environment win. The working directory's `.env` is deliberately never read: a
/// repository you open must not be able to change provider, base URL or confirmation policy.
/// (Developers get a project `.env` through the Justfile's `set dotenv-load`.)
fn load_global_env() {
    if let Some(p) = setup::config_path()
        && p.is_file()
    {
        let _ = dotenvy::from_path(&p);
    }
}

/// The wizard just wrote `MODEL` to the config file. Drop remembered `/model` choices so the
/// new value is not shadowed by an older saved one.
fn forget_saved_models() {
    if let Err(e) = storage::Storage::open().clear_saved_models() {
        eprintln!("Warning: could not clear the saved model choice: {e}");
    }
}

#[tokio::main]
async fn main() {
    let cli = Cli::parse();

    load_global_env();

    // `foxmayn-cowork config`: load existing config for defaults, run wizard, exit.
    if matches!(cli.command, Some(Commands::Config)) {
        // Show the model that is actually in use (a saved `/model` choice beats the .env one).
        let (current, _) =
            storage::apply_saved_settings(Config::from_env(), &storage::Storage::open());
        if let Err(e) = setup::run_wizard(Some(&current)) {
            eprintln!("Config error: {e}");
            std::process::exit(1);
        }
        forget_saved_models();
        return;
    }

    // First-time setup: nothing configured anywhere and stdin is a TTY.
    if setup::needs_init() {
        if let Err(e) = setup::run_wizard(None) {
            eprintln!("Setup error: {e}");
            std::process::exit(1);
        }
        forget_saved_models();
        load_global_env();
    }

    if let Some(Commands::Probe { message, stream }) = cli.command {
        let config = Config::from_env().with_overrides(
            cli.provider,
            cli.model,
            cli.streaming,
            cli.skip_confirmations,
        );
        let dir = cli.dir.unwrap_or_else(|| PathBuf::from("."));
        if let Err(e) = probe(config, message, dir, stream).await {
            eprintln!("probe failed: {e}");
            std::process::exit(1);
        }
        return;
    }

    // Layering, lowest to highest: env, saved `/…` settings, explicit CLI flags.
    let mut storage = storage::Storage::open();
    let mut startup_warnings: Vec<String> = Vec::new();
    startup_warnings.extend(storage.init_warning.take());
    let env_config = Config::from_env().with_overrides(cli.provider, None, None, false);
    let (saved_config, saved_warning) = storage::apply_saved_settings(env_config, &storage);
    startup_warnings.extend(saved_warning);
    let mut config =
        saved_config.with_overrides(None, cli.model, cli.streaming, cli.skip_confirmations);

    if let Err(msg) = preflight(&config) {
        eprintln!("{msg}");
        std::process::exit(1);
    }
    if config.skip_confirmations {
        let warning = "Destructive-operation confirmations are disabled. The AI can edit, delete, \
                       rename, and bulk-operate without prompting.";
        eprintln!("WARNING: {warning}");
        startup_warnings.push(warning.to_string());
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
                let warning = format!(
                    "Could not verify tool-calling support for Ollama model '{}': {e}. \
                     If the model misbehaves after tool calls, switch to a tool-capable \
                     model (e.g. qwen2.5:7b, llama3.1:8b).",
                    config.model
                );
                eprintln!("Warning: {warning}");
                startup_warnings.push(warning);
            }
        }

        // Non-thinking models answer HTTP 400 to any `think` value; drop it rather than fail.
        if config.ollama_think.is_some()
            && let Ok(false) = llm::ollama::model_supports_thinking(
                &llm_runtime.http_client,
                &config.ollama_base_url,
                &config.model,
            )
            .await
        {
            let warning = format!(
                "Ollama model '{}' does not support thinking; ignoring OLLAMA_THINK.",
                config.model
            );
            eprintln!("Warning: {warning}");
            startup_warnings.push(warning);
            config.ollama_think = None;
        }
    }

    let mut app = App::new(Arc::new(config), llm_runtime, storage);

    if let Some(dir) = cli.dir
        && let Err(e) = app.set_working_dir(dir)
    {
        eprintln!("Error: {e}");
        std::process::exit(1);
    }
    // stderr is hidden by the alternate screen, so repeat startup warnings in the chat.
    for warning in startup_warnings {
        app.chat_messages.push(app::ChatEntry {
            role: app::ChatRole::Warning,
            content: warning,
        });
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

async fn probe(config: Config, message: String, dir: PathBuf, stream: bool) -> Result<(), String> {
    use llm::tools::{dispatch_tool_call, tool_definitions};
    use llm::types::{ChatRequest, Message};

    let base_path = dir
        .canonicalize()
        .map_err(|e| format!("cannot open directory '{}': {e}", dir.display()))?;

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

    let runtime = LlmRuntime::build(&ollama_config)
        .await
        .map_err(|e| format!("failed to build LLM runtime: {e}"))?;

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
        println!(
            "{}",
            serde_json::to_string_pretty(&request)
                .map_err(|e| format!("could not serialize request: {e}"))?
        );
        println!();

        let assistant_msg = if stream {
            probe_stream_one(&runtime, &request, &ollama_config)
                .await
                .map_err(|e| e.to_string())?
        } else {
            llm::chat(&runtime, &request, &ollama_config)
                .await
                .map_err(|e| e.to_string())?
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

        // Run read-only tools; tools that need confirmation are described, never executed.
        for tc in assistant_msg.tool_calls.unwrap_or_default() {
            println!("--- TOOL CALL: {} ---", tc.function.name);
            println!("args: {}", tc.function.arguments);

            let result_str = match dispatch_tool_call(&tc.function, &base_path).await {
                Ok(r) if r.requires_confirmation => {
                    println!("{}", r.description);
                    println!("not executed (requires confirmation)");
                    "Not executed: this operation requires user confirmation, which is unavailable in probe."
                        .to_string()
                }
                Ok(r) => {
                    let result_str = r.result.unwrap_or_default();
                    println!("result: {result_str}");
                    result_str
                }
                Err(e) => {
                    println!("tool error: {e}");
                    format!("Error: {e}")
                }
            };
            messages.push(Message::tool_result(
                &tc.function.name,
                result_str,
                tc.id.clone(),
            ));
            println!();
        }
    }
    Ok(())
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
