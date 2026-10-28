use std::io::{self, IsTerminal, Write};
use std::path::{Path, PathBuf};

use crate::config::Config;

pub fn config_path() -> Option<PathBuf> {
    dirs::config_dir().map(|d| d.join("foxmayn-cowork").join(".env"))
}

/// True when first-run setup should run: no global config file, no provider/key already supplied
/// through the environment (shell, CI, or the Justfile's `.env`), and stdin is interactive.
pub fn needs_init() -> bool {
    let env_set = |k: &str| std::env::var_os(k).is_some_and(|v| !v.is_empty());
    needs_init_with(
        io::stdin().is_terminal(),
        config_path().is_some_and(|p| p.exists()),
        env_set("OPENROUTER_API_KEY") || env_set("PROVIDER"),
    )
}

fn needs_init_with(is_tty: bool, config_exists: bool, env_configured: bool) -> bool {
    is_tty && !config_exists && !env_configured
}

/// Write `content` to `path`, owner-read/write only on unix. An existing file with looser
/// permissions is tightened before the secret is written into it.
fn write_private(path: &Path, content: &str) -> io::Result<()> {
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    let mut file = opts.open(path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
    }
    file.write_all(content.as_bytes())
}

/// Create the config directory, owner-only on unix.
fn create_config_dir(dir: &Path) -> io::Result<()> {
    let mut builder = std::fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder.create(dir)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

/// First 8 characters of an existing key, for the "[current: …]" hint (char-safe).
fn key_hint(key: &str) -> String {
    if key.chars().count() > 8 {
        format!("{}…", key.chars().take(8).collect::<String>())
    } else {
        "(set)".to_string()
    }
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
            let hint = existing.map(key_hint);

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
        create_config_dir(parent)?;
    }

    write_private(&cfg_path, &(lines.join("\n") + "\n"))?;

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wizard_only_when_nothing_configured_and_interactive() {
        assert!(needs_init_with(true, false, false));
        assert!(!needs_init_with(false, false, false));
        assert!(!needs_init_with(true, true, false));
        assert!(!needs_init_with(true, false, true));
    }

    #[test]
    fn key_hint_is_char_safe() {
        assert_eq!(key_hint("short"), "(set)");
        assert_eq!(key_hint("sk-or-v1-abcdef"), "sk-or-v1…");
        // Byte 8 falls inside a multi-byte char: must not panic.
        assert_eq!(key_hint("abcdefgéééé"), "abcdefgé…");
    }

    #[cfg(unix)]
    #[test]
    fn config_file_is_private_even_when_rewritten() {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!("fcw-setup-test-{}", std::process::id()));
        create_config_dir(&dir).unwrap();
        let mode = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(&dir), 0o700);

        let file = dir.join(".env");
        std::fs::write(&file, "old").unwrap();
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o644)).unwrap();
        write_private(&file, "OPENROUTER_API_KEY=x\n").unwrap();
        assert_eq!(mode(&file), 0o600);
        assert_eq!(
            std::fs::read_to_string(&file).unwrap(),
            "OPENROUTER_API_KEY=x\n"
        );
        let _ = std::fs::remove_file(&file);
        let _ = std::fs::remove_dir(&dir);
    }
}
