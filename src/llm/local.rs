/// Local LLM inference via llama.cpp (llama-cpp-2 crate).
///
/// Enabled with `--features local` at build time.  At runtime, set
/// `PROVIDER=local` (or pass `--provider local`).
///
/// Model loading: on first run the GGUF file is downloaded from HuggingFace
/// into `~/.cache/huggingface/hub/` via the `hf-hub` crate.  Subsequent
/// launches reuse the cached file instantly.
///
/// Tool calling: tool schemas are embedded in the system prompt in ChatML
/// format.  After generation the output is scanned for a JSON object matching
/// `{"name": "…", "arguments": {…}}`; if found it is emitted as a tool call.
use std::num::NonZeroU32;
use std::path::PathBuf;
use std::sync::Arc;

use tokio::sync::mpsc::UnboundedSender;

use llama_cpp_2::context::params::LlamaContextParams;
use llama_cpp_2::llama_backend::LlamaBackend;
use llama_cpp_2::llama_batch::LlamaBatch;
use llama_cpp_2::model::params::LlamaModelParams;
#[allow(deprecated)]
use llama_cpp_2::model::{AddBos, LlamaModel, Special};
use llama_cpp_2::sampling::LlamaSampler;

use crate::config::Config;
use crate::error::AppError;
use crate::llm::types::{ChatRequest, FunctionCall, Message, StreamChunk, ToolCall};

// ── Runtime ───────────────────────────────────────────────────────────────────

/// Shared inference resources loaded once at startup.
pub struct LocalRuntime {
    backend: LlamaBackend,
    model: LlamaModel,
}

// LlamaBackend and LlamaModel are safe to share across threads: the model is
// immutable after loading, and backend wraps global C library init state.
unsafe impl Send for LocalRuntime {}
unsafe impl Sync for LocalRuntime {}

impl LocalRuntime {
    /// Download the GGUF (if not cached) and load it into memory.
    /// Blocks a thread-pool thread via `spawn_blocking` to avoid stalling the async runtime.
    pub async fn load_or_download(config: &Config) -> Result<Self, AppError> {
        let model_path = resolve_model_path(config).await?;

        let n_gpu_layers = config.local_gpu_layers;
        let path = model_path.clone();

        tokio::task::spawn_blocking(move || {
            let backend = LlamaBackend::init()
                .map_err(|e| AppError::LlmError(format!("llama backend init: {e}")))?;

            let model_params = LlamaModelParams::default()
                .with_n_gpu_layers(n_gpu_layers);

            let model = LlamaModel::load_from_file(&backend, &path, &model_params)
                .map_err(|e| AppError::LlmError(format!("Model load: {e}")))?;

            Ok(LocalRuntime { backend, model })
        })
        .await
        .map_err(|e| AppError::LlmError(format!("spawn_blocking panic: {e}")))?
    }
}

/// Resolve the model path: use `LOCAL_MODEL_PATH` directly if set, otherwise
/// download from HuggingFace Hub (cached after first run).
async fn resolve_model_path(config: &Config) -> Result<PathBuf, AppError> {
    if let Some(p) = &config.local_model_path {
        if !p.exists() {
            return Err(AppError::LlmError(format!(
                "LOCAL_MODEL_PATH does not exist: {}",
                p.display()
            )));
        }
        return Ok(p.clone());
    }

    eprintln!(
        "[local] Checking HuggingFace cache for {}/{}…",
        config.local_model_repo, config.local_model_file
    );

    let repo = config.local_model_repo.clone();
    let file = config.local_model_file.clone();

    let api = hf_hub::api::tokio::Api::new()
        .map_err(|e| AppError::LlmError(format!("HuggingFace API: {e}")))?;

    let path = api
        .model(repo)
        .get(&file)
        .await
        .map_err(|e| AppError::LlmError(format!("Model download: {e}")))?;

    Ok(path)
}

// ── Public provider interface ─────────────────────────────────────────────────

pub async fn chat(
    runtime: Arc<LocalRuntime>,
    request: &ChatRequest,
    config: &Config,
) -> Result<Message, AppError> {
    let prompt = build_prompt(request);
    let max_tokens = config.local_max_output_tokens;
    let temperature = config.local_temperature;
    let n_ctx = config.local_context_tokens;
    let n_threads = config.local_threads;
    let tools = request.tools.clone();

    let text = tokio::task::spawn_blocking(move || {
        generate_blocking(
            &runtime.backend,
            &runtime.model,
            &prompt,
            max_tokens,
            temperature,
            n_ctx,
            n_threads,
        )
    })
    .await
    .map_err(|e| AppError::LlmError(format!("spawn_blocking panic: {e}")))?;

    parse_output(text?, &tools)
}

/// Streaming variant.  Local inference is synchronous so we run it in a
/// blocking task and emit the result as a single set of chunks so the existing
/// streaming assembler in `app.rs` works without modification.
pub async fn chat_stream(
    runtime: Arc<LocalRuntime>,
    request: &ChatRequest,
    config: &Config,
    tx: UnboundedSender<StreamChunk>,
) -> Result<(), AppError> {
    let prompt = build_prompt(request);
    let max_tokens = config.local_max_output_tokens;
    let temperature = config.local_temperature;
    let n_ctx = config.local_context_tokens;
    let n_threads = config.local_threads;
    let tools = request.tools.clone();

    tokio::spawn(async move {
        let result = tokio::task::spawn_blocking(move || {
            generate_blocking(
                &runtime.backend,
                &runtime.model,
                &prompt,
                max_tokens,
                temperature,
                n_ctx,
                n_threads,
            )
        })
        .await;

        let text = match result {
            Err(e) => {
                let _ = tx.send(StreamChunk::Error(format!("spawn_blocking panic: {e}")));
                return;
            }
            Ok(Err(e)) => {
                let _ = tx.send(StreamChunk::Error(e.to_string()));
                return;
            }
            Ok(Ok(t)) => t,
        };

        match parse_output(text, &tools) {
            Err(e) => {
                let _ = tx.send(StreamChunk::Error(e.to_string()));
            }
            Ok(msg) => {
                if let Some(calls) = msg.tool_calls {
                    for (i, tc) in calls.iter().enumerate() {
                        let _ = tx.send(StreamChunk::ToolCallDelta {
                            index: i,
                            id: tc.id.clone(),
                            name: Some(tc.function.name.clone()),
                            arguments_fragment: tc.function.arguments.to_string(),
                        });
                    }
                } else if !msg.content.is_empty() {
                    let _ = tx.send(StreamChunk::ContentDelta(msg.content));
                }
                let _ = tx.send(StreamChunk::Done);
            }
        }
    });

    Ok(())
}

pub fn health_check(runtime: Option<&Arc<LocalRuntime>>) -> bool {
    runtime.is_some()
}

// ── Prompt rendering (ChatML / Qwen format) ───────────────────────────────────

/// Render the `ChatRequest` into a raw prompt string using ChatML tokens.
///
/// ChatML is natively understood by Qwen2.5 (the default model) and is
/// supported by most modern instruction-tuned GGUF models.
fn build_prompt(request: &ChatRequest) -> String {
    let mut prompt = String::new();

    let tool_block = if !request.tools.is_empty() {
        let schemas: Vec<serde_json::Value> = request
            .tools
            .iter()
            .map(|t| {
                serde_json::json!({
                    "name": t.function.name,
                    "description": t.function.description,
                    "parameters": t.function.parameters,
                })
            })
            .collect();
        format!(
            "\n\nYou have access to the following tools. To call a tool, respond ONLY \
             with a JSON object — no other text:\n\
             {{\"name\": \"<tool_name>\", \"arguments\": {{…}}}}\n\n\
             Available tools:\n{}",
            serde_json::to_string_pretty(&schemas).unwrap_or_default()
        )
    } else {
        String::new()
    };

    for msg in &request.messages {
        match msg.role.as_str() {
            "system" => {
                prompt.push_str("<|im_start|>system\n");
                prompt.push_str(&msg.content);
                prompt.push_str(&tool_block);
                prompt.push_str("\n<|im_end|>\n");
            }
            "user" => {
                prompt.push_str("<|im_start|>user\n");
                prompt.push_str(&msg.content);
                prompt.push_str("\n<|im_end|>\n");
            }
            "assistant" => {
                prompt.push_str("<|im_start|>assistant\n");
                if let Some(calls) = &msg.tool_calls {
                    for tc in calls {
                        let tc_json = serde_json::json!({
                            "name": tc.function.name,
                            "arguments": tc.function.arguments,
                        });
                        prompt.push_str(&tc_json.to_string());
                        prompt.push('\n');
                    }
                } else {
                    prompt.push_str(&msg.content);
                }
                prompt.push_str("\n<|im_end|>\n");
            }
            "tool" => {
                prompt.push_str("<|im_start|>tool\n");
                if let Some(name) = &msg.name {
                    prompt.push_str(&format!("Result of {name}:\n"));
                }
                prompt.push_str(&msg.content);
                prompt.push_str("\n<|im_end|>\n");
            }
            _ => {}
        }
    }

    // Open the assistant turn so the model continues from here.
    prompt.push_str("<|im_start|>assistant\n");
    prompt
}

// ── Core generation loop ──────────────────────────────────────────────────────

/// Run the llama.cpp token-generation loop synchronously.
/// Must be called via `tokio::task::spawn_blocking`.
fn generate_blocking(
    backend: &LlamaBackend,
    model: &LlamaModel,
    prompt: &str,
    max_tokens: usize,
    temperature: f32,
    n_ctx: u32,
    n_threads: Option<usize>,
) -> Result<String, AppError> {
    let ctx_size = NonZeroU32::new(n_ctx).unwrap_or(NonZeroU32::new(8192).unwrap());

    let mut ctx_params = LlamaContextParams::default().with_n_ctx(Some(ctx_size));
    if let Some(t) = n_threads {
        ctx_params = ctx_params
            .with_n_threads(t as i32)
            .with_n_threads_batch(t as i32);
    }

    let mut ctx = model
        .new_context(backend, ctx_params)
        .map_err(|e| AppError::LlmError(format!("Context creation: {e}")))?;

    let tokens = model
        .str_to_token(prompt, AddBos::Always)
        .map_err(|e| AppError::LlmError(format!("Tokenization: {e}")))?;

    if tokens.is_empty() {
        return Err(AppError::LlmError("Empty token list after tokenization".into()));
    }

    let n_prompt = tokens.len() as i32;
    let mut batch = LlamaBatch::new(tokens.len(), 1);

    for (i, &token) in tokens.iter().enumerate() {
        let is_last = i == tokens.len() - 1;
        batch
            .add(token, i as i32, &[0], is_last)
            .map_err(|e| AppError::LlmError(format!("Batch.add: {e}")))?;
    }

    ctx.decode(&mut batch)
        .map_err(|e| AppError::LlmError(format!("Prompt decode: {e}")))?;

    // Build sampler chain ending with a token-selection sampler.
    let mut sampler = if temperature > 0.0 {
        LlamaSampler::chain(
            [
                LlamaSampler::top_k(40),
                LlamaSampler::top_p(0.95, 1),
                LlamaSampler::temp(temperature),
                LlamaSampler::dist(1234),
            ],
            false,
        )
    } else {
        LlamaSampler::greedy()
    };

    let mut output = String::new();
    let mut n_cur = n_prompt;

    for _ in 0..max_tokens {
        let new_token = sampler.sample(&ctx, batch.n_tokens() - 1);
        sampler.accept(new_token);

        if model.is_eog_token(new_token) {
            break;
        }

        #[allow(deprecated)]
        let piece = model
            .token_to_str(new_token, Special::Tokenize)
            .map_err(|e| AppError::LlmError(format!("token_to_str: {e}")))?;

        // Stop on ChatML end-of-turn markers emitted as text.
        if piece.contains("<|im_end|>") || piece.contains("<|endoftext|>") {
            break;
        }

        output.push_str(&piece);

        batch.clear();
        batch
            .add(new_token, n_cur, &[0], true)
            .map_err(|e| AppError::LlmError(format!("Batch.add: {e}")))?;
        n_cur += 1;

        ctx.decode(&mut batch)
            .map_err(|e| AppError::LlmError(format!("Token decode: {e}")))?;
    }

    Ok(output.trim().to_string())
}

// ── Output parsing ────────────────────────────────────────────────────────────

/// Detect tool calls in the generated text.
///
/// If the trimmed output is a JSON object with `"name"` matching a known tool
/// and an `"arguments"` field, it is returned as a `Message` with `tool_calls`
/// set.  Otherwise the text is returned as a plain assistant message.
fn parse_output(text: String, tools: &[crate::llm::types::Tool]) -> Result<Message, AppError> {
    let trimmed = text.trim();

    if trimmed.starts_with('{') {
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(trimmed) {
            if let (Some(name), Some(arguments)) = (
                v.get("name").and_then(|n| n.as_str()),
                v.get("arguments"),
            ) {
                if tools.iter().any(|t| t.function.name == name) {
                    return Ok(Message {
                        role: "assistant".into(),
                        content: String::new(),
                        tool_calls: Some(vec![ToolCall {
                            id: Some("local_call_0".into()),
                            function: FunctionCall {
                                name: name.to_string(),
                                arguments: arguments.clone(),
                            },
                        }]),
                        name: None,
                        tool_call_id: None,
                    });
                }
            }
        }
    }

    if trimmed.is_empty() {
        return Err(AppError::LlmError(
            "Model returned an empty response. Try a larger model or rephrase your request.".into(),
        ));
    }

    Ok(Message {
        role: "assistant".into(),
        content: trimmed.to_string(),
        tool_calls: None,
        name: None,
        tool_call_id: None,
    })
}
