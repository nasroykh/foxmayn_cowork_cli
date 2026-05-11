use std::io::{self, IsTerminal, Write};
use std::path::PathBuf;

use crate::config::Config;

pub fn config_path() -> Option<PathBuf> {
    dirs::config_dir().map(|d| d.join("foxmayn-cowork").join(".env"))
}

/// True when the config file is absent and stdin is interactive (skips CI / piped runs).
pub fn needs_init() -> bool {
    io::stdin().is_terminal() && config_path().map(|p| !p.exists()).unwrap_or(false)
}

// ── Prompt helpers ────────────────────────────────────────────────────────────

fn read_line() -> io::Result<Option<String>> {
    let mut buf = String::new();
    let n = io::stdin().read_line(&mut buf)?;
    if n == 0 {
        Ok(None) // EOF
    } else {
        Ok(Some(buf.trim().to_string()))
    }
}

fn prompt(label: &str, default: &str) -> io::Result<String> {
    print!("{} [{}]: ", label, default);
    io::stdout().flush()?;
    Ok(match read_line()? {
        Some(s) if !s.is_empty() => s,
        _ => default.to_string(),
    })
}

fn prompt_choice(label: &str, options: &[&str], default: usize) -> io::Result<usize> {
    println!("{}", label);
    for (i, opt) in options.iter().enumerate() {
        let tag = if i == default { " (default)" } else { "" };
        println!("  [{}] {}{}", i + 1, opt, tag);
    }
    print!("\nChoice [{}]: ", default + 1);
    io::stdout().flush()?;
    Ok(match read_line()? {
        Some(s) if !s.is_empty() => match s.parse::<usize>() {
            Ok(n) if n >= 1 && n <= options.len() => n - 1,
            _ => {
                println!("Invalid — using default.");
                default
            }
        },
        _ => default,
    })
}

// ── Wizard ────────────────────────────────────────────────────────────────────

pub fn run_wizard(current: Option<&Config>) -> io::Result<()> {
    let is_init = current.is_none();

    if is_init {
        println!("╔══════════════════════════════════════╗");
        println!("║   foxmayn-cowork — first-time setup  ║");
        println!("╚══════════════════════════════════════╝");
        println!();
    } else {
        println!("foxmayn-cowork config");
        println!();
    }

    let default_provider = current
        .map(|c| match c.provider {
            crate::config::Provider::OpenRouter => 0,
            crate::config::Provider::Ollama => 1,
            crate::config::Provider::Local => 2,
        })
        .unwrap_or(0);

    let provider_idx = prompt_choice(
        "Provider:",
        &[
            "OpenRouter  – cloud inference, requires an API key",
            "Ollama      – local inference, no API key needed",
            "Local       – offline llama.cpp (requires --features local build)",
        ],
        default_provider,
    )?;

    let mut lines: Vec<String> = Vec::new();
    lines.push(format!(
        "PROVIDER={}",
        ["openrouter", "ollama", "local"][provider_idx]
    ));

    println!();

    match provider_idx {
        0 => {
            // OpenRouter
            let existing = current.and_then(|c| c.openrouter_api_key.as_deref());
            let hint = existing.map(|k| {
                if k.len() > 8 {
                    format!("{}…", &k[..8])
                } else {
                    "(set)".to_string()
                }
            });

            print!("OpenRouter API key");
            if let Some(ref h) = hint {
                print!(" [current: {}]", h);
            }
            print!(" (hidden): ");
            io::stdout().flush()?;

            let typed = rpassword::read_password()?;
            let typed = typed.trim().to_string();
            let key = if typed.is_empty() {
                existing.unwrap_or("").to_string()
            } else {
                typed
            };
            if !key.is_empty() {
                lines.push(format!("OPENROUTER_API_KEY={}", key));
            }

            let default_model = current
                .map(|c| c.model.as_str())
                .unwrap_or("google/gemini-2.5-flash-lite");
            let model = prompt("Model", default_model)?;
            if model != "google/gemini-2.5-flash-lite" {
                lines.push(format!("MODEL={}", model));
            }
        }

        1 => {
            // Ollama
            let default_url = current
                .map(|c| c.ollama_base_url.as_str())
                .unwrap_or("http://localhost:11434");
            let url = prompt("Ollama base URL", default_url)?;
            if url != "http://localhost:11434" {
                lines.push(format!("OLLAMA_BASE_URL={}", url));
            }

            let default_model = current.map(|c| c.model.as_str()).unwrap_or("qwen3:0.6b");
            let model = prompt("Model", default_model)?;
            lines.push(format!("MODEL={}", model));
        }

        2 => {
            // Local
            println!("Local inference uses embedded llama.cpp.");
            println!(
                "Advanced settings (model repo, GPU layers, etc.) can be added to the config\n\
                 file after setup."
            );
        }

        _ => unreachable!(),
    }

    // Write the config file
    let cfg_path = match config_path() {
        Some(p) => p,
        None => {
            eprintln!("Could not determine config directory.");
            return Ok(());
        }
    };

    if let Some(parent) = cfg_path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    std::fs::write(&cfg_path, lines.join("\n") + "\n")?;

    println!();
    println!("Config saved to {}", cfg_path.display());
    if is_init {
        println!("Launching foxmayn-cowork...");
        println!();
    } else {
        println!("Run 'foxmayn-cowork' to start.");
    }

    Ok(())
}
