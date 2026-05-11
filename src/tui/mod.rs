use std::io;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use chrono::{Local, TimeZone, Utc};

use crossterm::{
    event::{
        DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture,
        Event, EventStream, KeyCode, KeyModifiers, MouseEventKind,
    },
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use futures::StreamExt;
use ratatui::{Terminal, backend::CrosstermBackend};
use tokio::sync::mpsc::{self, UnboundedSender};
use tui_textarea::TextArea;

use crate::app::{self, App, AppEvent, ChatEntry, ChatRole, InputMode, Panel, SlashPicker,
                  SlashPickerItem};
use crate::config::{Config, Provider, ThinkingDisplay, ToolDisplayVerbosity};
use crate::llm::types::{OllamaThink, OllamaThinkLevel, ReasoningEffort, RequestReasoning};
use crate::fs;
use crate::llm;

pub mod commands;
pub mod ui;
pub mod widgets;

pub async fn run(mut app: App) -> io::Result<()> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(
        stdout,
        EnterAlternateScreen,
        EnableMouseCapture,
        EnableBracketedPaste
    )?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    // Restore terminal on panic so the shell isn't left in raw mode
    let original_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let _ = disable_raw_mode();
        let _ = crossterm::execute!(
            io::stdout(),
            LeaveAlternateScreen,
            DisableMouseCapture,
            DisableBracketedPaste
        );
        original_hook(info);
    }));

    let result = run_loop(&mut terminal, &mut app).await;

    disable_raw_mode()?;
    execute!(
        terminal.backend_mut(),
        LeaveAlternateScreen,
        DisableMouseCapture,
        DisableBracketedPaste
    )?;
    terminal.show_cursor()?;

    result
}

async fn run_loop<B: ratatui::backend::Backend>(
    terminal: &mut Terminal<B>,
    app: &mut App,
) -> io::Result<()> {
    let (tx, mut rx) = mpsc::unbounded_channel::<AppEvent>();
    let mut events = EventStream::new();
    let mut textarea = new_textarea();

    // First health check and initial file tree load
    spawn_health_check(app, tx.clone());
    if let Some(dir) = app.working_dir.clone() {
        spawn_file_tree_load(dir, tx.clone());
    }

    // Periodic health check — first tick after 10 s so it doesn't duplicate the startup check
    let start = tokio::time::Instant::now() + Duration::from_secs(10);
    let mut health_tick = tokio::time::interval_at(start, Duration::from_secs(10));

    loop {
        update_textarea_style(&mut textarea, app);
        terminal.draw(|f| ui::render(f, app, &textarea))?;

        tokio::select! {
            maybe_event = events.next() => {
                let Some(Ok(event)) = maybe_event else { break };
                handle_crossterm_event(event, app, &mut textarea, &tx);
            }
            maybe_msg = rx.recv() => {
                let Some(msg) = maybe_msg else { break };
                handle_app_event(msg, app, &tx);
            }
            _ = health_tick.tick() => {
                spawn_health_check(app, tx.clone());
            }
        }

        if app.should_quit {
            break;
        }
    }

    Ok(())
}

// ── Event handlers ────────────────────────────────────────────────────────────

fn handle_crossterm_event(
    event: Event,
    app: &mut App,
    textarea: &mut TextArea,
    tx: &UnboundedSender<AppEvent>,
) {
    match event {
        Event::Mouse(mouse) => {
            match mouse.kind {
                MouseEventKind::ScrollUp => match app.focused_panel {
                    Panel::Chat => app.scroll_chat_up(),
                    Panel::FileTree => app.scroll_tree_up(),
                },
                MouseEventKind::ScrollDown => match app.focused_panel {
                    Panel::Chat => app.scroll_chat_down(),
                    Panel::FileTree => app.scroll_tree_down(),
                },
                _ => {}
            }
            return;
        }
        Event::Paste(text) => {
            // Paste only makes sense into the chat input; ignore otherwise.
            if app.input_mode == InputMode::Editing && app.focused_panel == Panel::Chat {
                let mut first = true;
                for line in text.split('\n') {
                    if !first {
                        textarea.insert_newline();
                    }
                    textarea.insert_str(line);
                    first = false;
                }
            }
            return;
        }
        Event::Key(_) => {}
        _ => return,
    }

    let Event::Key(key) = event else { return };

    // Global quit
    if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
        app.should_quit = true;
        return;
    }

    match app.input_mode {
        InputMode::Confirming => match key.code {
            KeyCode::Char('y') | KeyCode::Char('Y') => {
                app.is_loading = true;
                spawn_confirm_tool(app, tx.clone(), true);
                // Dismiss the dialog immediately — don't wait for the LLM to finish.
                app.input_mode = InputMode::Editing;
                app.pending_confirmation = None;
            }
            KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Esc => {
                spawn_confirm_tool(app, tx.clone(), false);
                app.input_mode = InputMode::Editing;
                app.pending_confirmation = None;
            }
            _ => {}
        },

        InputMode::Editing => match key.code {
            // Esc: dismiss picker → dismiss completions → cancel in-flight request.
            KeyCode::Esc if app.slash_picker.is_some() => {
                app.slash_picker = None;
            }
            KeyCode::Esc if !app.slash_completions.is_empty() => {
                app.slash_completions.clear();
                app.slash_selected = 0;
            }
            KeyCode::Esc if app.is_loading || app.streaming_text.is_some() => {
                app.cancel_request();
            }
            // Plain Enter submits; Shift/Alt+Enter inserts a newline so the
            // user can compose multi-paragraph prompts.
            KeyCode::Enter
                if !key
                    .modifiers
                    .intersects(KeyModifiers::SHIFT | KeyModifiers::ALT) =>
            {
                // File tree panel: Enter toggles directory expand/collapse
                if app.focused_panel == Panel::FileTree {
                    if let Some(path) = app.toggle_expand() {
                        spawn_subdir_load(path, tx.clone());
                    }
                    return;
                }
                // If the picker is open, Enter selects the highlighted item and executes.
                if app.slash_picker.is_some() && app.focused_panel == Panel::Chat {
                    if let Some(picker) = app.slash_picker.take()
                        && let Some(item) = picker.items.get(picker.selected)
                    {
                        let full_cmd = format!("{} {}", picker.command, item.value);
                        *textarea = new_textarea();
                        dispatch_slash_command(&full_cmd, app, tx);
                    }
                    return;
                }
                // If the completions popup is open, Enter accepts the highlighted entry.
                // Picker-capable commands with no arg open the picker instead of completing.
                // Other arg commands complete into the textarea; arg-less commands execute.
                if !app.slash_completions.is_empty() && app.focused_panel == Panel::Chat {
                    if let Some(&cmd_idx) = app.slash_completions.get(app.slash_selected) {
                        let cmd = &commands::COMMANDS[cmd_idx];
                        if cmd.has_picker {
                            // Open the picker (dispatch will populate slash_picker).
                            let name = cmd.name.to_string();
                            app.slash_completions.clear();
                            app.slash_selected = 0;
                            *textarea = new_textarea();
                            dispatch_slash_command(&name, app, tx);
                        } else if cmd.has_arg {
                            // Complete into the textarea so the user can type the arg.
                            let completion = format!("{} ", cmd.name);
                            *textarea = new_textarea();
                            textarea.insert_str(&completion);
                            app.update_slash_completions(&completion);
                        } else {
                            // Execute the arg-less command directly.
                            let name = cmd.name.to_string();
                            app.slash_completions.clear();
                            app.slash_selected = 0;
                            *textarea = new_textarea();
                            dispatch_slash_command(&name, app, tx);
                        }
                    }
                    return;
                }
                // Chat panel: Enter submits the message
                let text = textarea.lines().join("\n");
                let text = text.trim().to_string();
                if text.is_empty() || app.is_loading {
                    return;
                }
                app.slash_completions.clear();
                app.slash_selected = 0;
                *textarea = new_textarea();
                if text.starts_with('/') {
                    dispatch_slash_command(&text, app, tx);
                } else {
                    app.begin_send(&text);
                    spawn_send_message(app, tx.clone(), text);
                }
            }
            KeyCode::Right if app.focused_panel == Panel::FileTree => {
                if let Some(path) = app.toggle_expand() {
                    spawn_subdir_load(path, tx.clone());
                }
            }
            KeyCode::Left if app.focused_panel == Panel::FileTree => {
                let idx = app.file_tree_scroll;
                let should_collapse = app
                    .file_tree
                    .get(idx)
                    .is_some_and(|e| e.is_dir && e.expanded);
                if should_collapse {
                    app.collapse_dir(idx);
                } else {
                    app.jump_to_parent();
                }
            }
            KeyCode::Char('l') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                app.clear_conversation();
            }
            // Tab: accept highlighted completion if popup is open, else switch panel.
            KeyCode::Tab
                if !app.slash_completions.is_empty()
                    && app.focused_panel == Panel::Chat =>
            {
                if let Some(&cmd_idx) = app.slash_completions.get(app.slash_selected) {
                    let cmd = &commands::COMMANDS[cmd_idx];
                    let completion = if cmd.has_arg {
                        format!("{} ", cmd.name)
                    } else {
                        cmd.name.to_string()
                    };
                    *textarea = new_textarea();
                    textarea.insert_str(&completion);
                    app.update_slash_completions(&completion);
                }
            }
            KeyCode::Tab => {
                app.focused_panel = match app.focused_panel {
                    Panel::Chat => Panel::FileTree,
                    Panel::FileTree => Panel::Chat,
                };
            }
            // Up/Down: navigate picker first, then completions, then chat scroll.
            KeyCode::Up
                if app.slash_picker.is_some() && app.focused_panel == Panel::Chat =>
            {
                if let Some(p) = app.slash_picker.as_mut() {
                    p.select_prev();
                }
            }
            KeyCode::Down
                if app.slash_picker.is_some() && app.focused_panel == Panel::Chat =>
            {
                if let Some(p) = app.slash_picker.as_mut() {
                    p.select_next();
                }
            }
            KeyCode::Up
                if !app.slash_completions.is_empty()
                    && app.focused_panel == Panel::Chat =>
            {
                app.slash_select_prev();
            }
            KeyCode::Down
                if !app.slash_completions.is_empty()
                    && app.focused_panel == Panel::Chat =>
            {
                app.slash_select_next();
            }
            KeyCode::Up => match app.focused_panel {
                Panel::Chat => app.scroll_chat_up(),
                Panel::FileTree => app.scroll_tree_up(),
            },
            KeyCode::Down => match app.focused_panel {
                Panel::Chat => app.scroll_chat_down(),
                Panel::FileTree => app.scroll_tree_down(),
            },
            _ => {
                textarea.input(key);
                let current = textarea.lines().join("\n");
                app.update_slash_completions(&current);
            }
        },
    }
}

fn dispatch_slash_command(text: &str, app: &mut App, tx: &UnboundedSender<AppEvent>) {
    let (cmd, arg) = text
        .split_once(' ')
        .map_or((text, ""), |(c, a)| (c, a.trim()));

    match cmd {
        "/clear" => app.clear_conversation(),
        "/exit" => app.should_quit = true,
        "/dir" => {
            if arg.is_empty() {
                push_error(app, "Usage: /dir <path>");
            } else {
                let dir = PathBuf::from(arg);
                app.set_working_dir(dir.clone());
                spawn_file_tree_load(dir, tx.clone());
                spawn_health_check(app, tx.clone());
            }
        }
        "/model" => {
            if arg.is_empty() {
                match app.config.provider {
                    Provider::OpenRouter => {
                        open_model_picker(app, openrouter_model_list());
                    }
                    Provider::Ollama => {
                        spawn_fetch_ollama_models(app, tx.clone());
                    }
                    Provider::Local => {
                        push_info(app, &format!("Current model: {}", app.config.model));
                    }
                }
            } else {
                mutate_config(app, |c| c.model = arg.to_string());
                persist_setting(app, "model", arg);
                push_info(app, &format!("Model set to: {arg}"));
            }
        }
        "/skip-confirmations" => {
            let next = !app.config.skip_confirmations;
            mutate_config(app, |c| c.skip_confirmations = next);
            // Intentionally not persisted — always starts safe on next launch.
            push_info(
                app,
                &format!(
                    "Confirmations {} (session only)",
                    if next { "disabled" } else { "enabled" }
                ),
            );
        }
        "/streaming" => {
            let next = !app.config.streaming_enabled;
            mutate_config(app, |c| c.streaming_enabled = next);
            persist_setting(app, "streaming_enabled", if next { "true" } else { "false" });
            push_info(
                app,
                &format!("Streaming {}", if next { "on" } else { "off" }),
            );
        }
        "/thinking" => {
            if arg.is_empty() {
                let current = match app.config.thinking_display {
                    ThinkingDisplay::Off => "off",
                    ThinkingDisplay::Inline => "inline",
                    ThinkingDisplay::Full => "full",
                };
                open_static_picker(app, "/thinking", current);
            } else {
                match arg.parse::<ThinkingDisplay>() {
                    Ok(mode) => {
                        mutate_config(app, |c| c.thinking_display = mode);
                        persist_setting(app, "thinking_display", arg);
                        push_info(app, &format!("Thinking display set to: {arg}"));
                    }
                    Err(_) => push_error(app, "Usage: /thinking <off|inline|full>"),
                }
            }
        }
        "/tool-verbosity" => {
            if arg.is_empty() {
                let current = match app.config.tool_display_verbosity {
                    ToolDisplayVerbosity::Default => "default",
                    ToolDisplayVerbosity::Minimal => "minimal",
                    ToolDisplayVerbosity::Full => "full",
                };
                open_static_picker(app, "/tool-verbosity", current);
            } else {
                match arg.parse::<ToolDisplayVerbosity>() {
                    Ok(mode) => {
                        mutate_config(app, |c| c.tool_display_verbosity = mode);
                        persist_setting(app, "tool_display_verbosity", arg);
                        push_info(app, &format!("Tool verbosity set to: {arg}"));
                    }
                    Err(_) => push_error(app, "Usage: /tool-verbosity <default|minimal|full>"),
                }
            }
        }
        "/reasoning" => {
            if arg.is_empty() {
                open_reasoning_picker(app);
            } else {
                apply_reasoning(app, arg);
            }
        }
        "/sessions" => {
            let sessions = app.list_sessions();
            if sessions.is_empty() {
                if app.storage.project.is_none() {
                    push_error(app, "No project open. Use /dir <path> first.");
                } else {
                    push_info(app, "No sessions yet for this project.");
                }
            } else {
                let mut lines =
                    vec!["Recent sessions — use /resume <id> to continue:".to_string()];
                for s in sessions {
                    lines.push(format!(
                        "[{}]  {}  {}",
                        s.id,
                        format_timestamp(s.started_at),
                        s.title
                    ));
                }
                push_info(app, &lines.join("\n"));
            }
        }
        "/resume" => {
            if arg.is_empty() {
                // No id given — open the interactive picker.
                let sessions = app.list_sessions();
                if sessions.is_empty() {
                    if app.storage.project.is_none() {
                        push_error(app, "No project open. Use /dir <path> first.");
                    } else {
                        push_info(app, "No sessions yet for this project.");
                    }
                } else {
                    app.slash_picker = Some(crate::app::SlashPicker {
                        command: "/resume",
                        items: sessions
                            .into_iter()
                            .map(|s| crate::app::SlashPickerItem {
                                display: format!(
                                    "[{}]  {}  {}",
                                    s.id,
                                    format_timestamp(s.started_at),
                                    s.title
                                ),
                                value: s.id.to_string(),
                            })
                            .collect(),
                        selected: 0,
                    });
                }
            } else {
                match arg.parse::<i64>() {
                    Ok(id) => {
                        if app.resume_session(id) {
                            push_info(app, &format!("Session #{id} resumed."));
                        } else {
                            push_error(
                                app,
                                &format!("Session #{id} not found. Use /sessions to list available sessions."),
                            );
                        }
                    }
                    Err(_) => push_error(app, "Usage: /resume <session-id>"),
                }
            }
        }
        _ => push_error(app, &format!("Unknown command: {cmd}")),
    }
}

/// Clone the config, apply `f`, and swap the Arc so the next spawned task picks up the change.
fn mutate_config(app: &mut App, f: impl FnOnce(&mut Config)) {
    let mut c = (*app.config).clone();
    f(&mut c);
    app.config = Arc::new(c);
}

fn push_info(app: &mut App, msg: &str) {
    app.chat_messages.push(ChatEntry {
        role: ChatRole::Tool,
        content: msg.to_string(),
    });
}

fn push_error(app: &mut App, msg: &str) {
    app.chat_messages.push(ChatEntry {
        role: ChatRole::Error,
        content: msg.to_string(),
    });
}

/// Build a picker from a command's `static_options`, marking `current` with `(active)`.
/// Preselects the current option so the user can see where they are immediately.
fn open_static_picker(app: &mut App, cmd_name: &'static str, current: &str) {
    let Some(cmd) = commands::COMMANDS.iter().find(|c| c.name == cmd_name) else {
        return;
    };
    let selected = cmd
        .static_options
        .iter()
        .position(|&o| o == current)
        .unwrap_or(0);
    app.slash_picker = Some(crate::app::SlashPicker {
        command: cmd_name,
        items: cmd
            .static_options
            .iter()
            .map(|&opt| crate::app::SlashPickerItem {
                display: if opt == current {
                    format!("{opt}  (active)")
                } else {
                    opt.to_string()
                },
                value: opt.to_string(),
            })
            .collect(),
        selected,
    });
}

fn persist_setting(app: &mut App, key: &str, value: &str) {
    if let Err(e) = app.storage.save_setting(key, value) {
        push_error(app, &format!("Warning: setting not persisted: {e}"));
    }
}

fn format_timestamp(ts: i64) -> String {
    Utc.timestamp_opt(ts, 0)
        .single()
        .map(|utc| utc.with_timezone(&Local).format("%Y-%m-%d %H:%M").to_string())
        .unwrap_or_else(|| ts.to_string())
}

fn openrouter_model_list() -> Vec<String> {
    vec![
        "google/gemini-2.5-flash-lite".into(),
        "google/gemini-3.1-flash-lite".into(),
        "deepseek/deepseek-v4-flash".into(),
        "deepseek/deepseek-v4-pro".into(),
        "google/gemma-4-26b-a4b-it".into(),
        "google/gemma-4-31b-it".into(),
        "minimax/minimax-m2.7".into(),
        "mistralai/mistral-small-2603".into(),
    ]
}

fn open_model_picker(app: &mut App, models: Vec<String>) {
    let current = app.config.model.clone();
    let selected = models.iter().position(|m| m == &current).unwrap_or(0);
    app.slash_picker = Some(SlashPicker {
        command: "/model",
        items: models
            .into_iter()
            .map(|m| {
                let display = if m == current {
                    format!("{m}  (active)")
                } else {
                    m.clone()
                };
                SlashPickerItem { display, value: m }
            })
            .collect(),
        selected,
    });
}

fn open_reasoning_picker(app: &mut App) {
    let (options, current): (&[&str], &str) = match app.config.provider {
        Provider::OpenRouter => (
            &["off", "minimal", "none", "low", "medium", "high", "xhigh"],
            openrouter_reasoning_current(&app.config),
        ),
        Provider::Ollama => (
            &["off", "low", "medium", "high"],
            ollama_think_current(&app.config),
        ),
        Provider::Local => {
            push_info(app, "Reasoning is not configurable for the local provider.");
            return;
        }
    };
    let selected = options.iter().position(|&o| o == current).unwrap_or(0);
    app.slash_picker = Some(SlashPicker {
        command: "/reasoning",
        items: options
            .iter()
            .map(|&opt| SlashPickerItem {
                display: if opt == current {
                    format!("{opt}  (active)")
                } else {
                    opt.to_string()
                },
                value: opt.to_string(),
            })
            .collect(),
        selected,
    });
}

fn apply_reasoning(app: &mut App, arg: &str) {
    match app.config.provider {
        Provider::OpenRouter => {
            if arg == "off" {
                mutate_config(app, |c| c.openrouter_reasoning = None);
                persist_setting(app, "openrouter_reasoning", "off");
                push_info(app, "OpenRouter reasoning disabled.");
            } else {
                match arg.parse::<ReasoningEffort>() {
                    Ok(effort) => {
                        let summary = app
                            .config
                            .openrouter_reasoning
                            .as_ref()
                            .and_then(|r| r.summary);
                        mutate_config(app, move |c| {
                            c.openrouter_reasoning =
                                Some(RequestReasoning { effort: Some(effort), summary });
                        });
                        persist_setting(app, "openrouter_reasoning", arg);
                        push_info(app, &format!("OpenRouter reasoning set to: {arg}"));
                    }
                    Err(_) => {
                        push_error(app, "Usage: /reasoning <off|minimal|none|low|medium|high|xhigh>");
                    }
                }
            }
        }
        Provider::Ollama => {
            if arg == "off" {
                mutate_config(app, |c| c.ollama_think = None);
                persist_setting(app, "ollama_think", "off");
                push_info(app, "Ollama thinking disabled.");
            } else {
                match arg.parse::<OllamaThink>() {
                    Ok(think) => {
                        mutate_config(app, move |c| c.ollama_think = Some(think));
                        persist_setting(app, "ollama_think", arg);
                        push_info(app, &format!("Ollama think set to: {arg}"));
                    }
                    Err(_) => {
                        push_error(app, "Usage: /reasoning <off|low|medium|high>");
                    }
                }
            }
        }
        Provider::Local => {
            push_info(app, "Reasoning is not configurable for the local provider.");
        }
    }
}

fn openrouter_reasoning_current(config: &Config) -> &'static str {
    match config.openrouter_reasoning.as_ref().and_then(|r| r.effort) {
        None => "off",
        Some(ReasoningEffort::XHigh) => "xhigh",
        Some(ReasoningEffort::High) => "high",
        Some(ReasoningEffort::Medium) => "medium",
        Some(ReasoningEffort::Low) => "low",
        Some(ReasoningEffort::Minimal) => "minimal",
        Some(ReasoningEffort::None) => "none",
    }
}

fn ollama_think_current(config: &Config) -> &'static str {
    match config.ollama_think {
        None | Some(OllamaThink::OnOff(false)) => "off",
        Some(OllamaThink::OnOff(true)) | Some(OllamaThink::Level(OllamaThinkLevel::High)) => {
            "high"
        }
        Some(OllamaThink::Level(OllamaThinkLevel::Medium)) => "medium",
        Some(OllamaThink::Level(OllamaThinkLevel::Low)) => "low",
    }
}

fn spawn_fetch_ollama_models(app: &App, tx: UnboundedSender<AppEvent>) {
    let base_url = app.config.ollama_base_url.clone();
    let client = app.llm_runtime.http_client.clone();
    tokio::spawn(async move {
        let models = fetch_ollama_models(&client, &base_url).await;
        let _ = tx.send(AppEvent::ModelsLoaded(models));
    });
}

async fn fetch_ollama_models(client: &reqwest::Client, base_url: &str) -> Vec<String> {
    #[derive(serde::Deserialize)]
    struct TagsResponse {
        models: Vec<OllamaModel>,
    }
    #[derive(serde::Deserialize)]
    struct OllamaModel {
        name: String,
    }

    let url = format!("{base_url}/api/tags");
    match client.get(&url).send().await {
        Ok(resp) => match resp.json::<TagsResponse>().await {
            Ok(data) => data.models.into_iter().map(|m| m.name).collect(),
            Err(_) => Vec::new(),
        },
        Err(_) => Vec::new(),
    }
}

fn handle_app_event(event: AppEvent, app: &mut App, tx: &UnboundedSender<AppEvent>) {
    match event {
        AppEvent::LlmResponse {
            request_id,
            outcome,
            conversation,
        } => {
            if request_id.is_some_and(|id| !app.is_active_request(id)) {
                return;
            }
            // Refresh the file tree after any successful LLM response so newly
            // created/deleted/renamed files appear without a manual /dir reload.
            let should_refresh = matches!(outcome, app::LlmOutcome::Complete { .. });
            app.current_request = None;
            app.active_request_id = None;
            app.handle_outcome(outcome, conversation);
            if should_refresh {
                app.save_current_session();
                if let Some(dir) = app.working_dir.clone() {
                    app.prepare_refresh();
                    spawn_file_tree_load(dir, tx.clone());
                }
            }
        }
        AppEvent::StreamChunk { request_id, chunk } => {
            if !app.is_active_request(request_id) {
                return;
            }
            app.handle_stream_chunk(&chunk);
        }
        AppEvent::StreamComplete {
            request_id,
            outcome,
            conversation,
        } => {
            if !app.is_active_request(request_id) {
                return;
            }
            app.finalize_stream();
            let should_refresh = matches!(outcome, app::LlmOutcome::Complete { .. });
            app.current_request = None;
            app.active_request_id = None;
            app.handle_outcome(outcome, conversation);
            if should_refresh {
                app.save_current_session();
                if let Some(dir) = app.working_dir.clone() {
                    app.prepare_refresh();
                    spawn_file_tree_load(dir, tx.clone());
                }
            }
        }
        AppEvent::IntermediateAssistant {
            request_id,
            content,
        } => {
            if !app.is_active_request(request_id) {
                return;
            }
            // Flush the live streaming buffer to a permanent chat entry so
            // subsequent rounds don't overwrite this round's text. Promote any thinking
            // collected during this round first so it appears above the assistant turn.
            app.finalize_thinking_for_round();
            app.streaming_text = None;
            if !content.trim().is_empty() {
                app.chat_messages.push(app::ChatEntry {
                    role: app::ChatRole::Assistant,
                    content,
                });
            }
        }
        AppEvent::IntermediateTool {
            request_id,
            name,
            result,
        } => {
            if !app.is_active_request(request_id) {
                return;
            }
            // For rounds that produced only tool calls (no assistant text) the thinking buffer
            // hasn't been finalized yet — do it here. Idempotent if already done.
            app.finalize_thinking_for_round();
            // `result` is already a complete `[name] ...` line built by `format_tool_summary`.
            // Fall back to a bare `[name]` if it somehow arrived empty.
            let display = if result.is_empty() {
                format!("[{name}]")
            } else {
                result
            };
            app.chat_messages.push(app::ChatEntry {
                role: app::ChatRole::Tool,
                content: display,
            });
        }
        AppEvent::ContextWarning(msg) => {
            app.chat_messages.push(app::ChatEntry {
                role: app::ChatRole::Warning,
                content: msg,
            });
        }
        AppEvent::HealthCheckResult(ok) => {
            app.handle_health(ok);
        }
        AppEvent::FileTreeLoaded(result) => {
            app.handle_file_tree(result);
            restore_pending_expansions(app, tx);
            app.restore_pending_scroll();
        }
        AppEvent::SubdirLoaded {
            parent_path,
            result,
        } => {
            app.handle_subdir_loaded(parent_path, result);
            restore_pending_expansions(app, tx);
            app.restore_pending_scroll();
        }
        AppEvent::ModelsLoaded(models) => {
            if models.is_empty() {
                app.chat_messages.push(app::ChatEntry {
                    role: app::ChatRole::Error,
                    content: "Could not fetch models from Ollama. Is it running?".into(),
                });
            } else {
                open_model_picker(app, models);
            }
        }
    }
}

/// After a tree (re)load, re-expand any directories that were expanded before
/// the refresh. Each newly expanded dir triggers its own subdir load, which
/// recursively cascades through nested expansions.
fn restore_pending_expansions(app: &mut App, tx: &UnboundedSender<AppEvent>) {
    let ready = app.drain_ready_pending_expansions();
    for path in ready {
        if app.mark_expanded(&path) {
            spawn_subdir_load(path, tx.clone());
        }
    }
}

// ── Async task spawners ───────────────────────────────────────────────────────

fn spawn_health_check(app: &App, tx: UnboundedSender<AppEvent>) {
    let runtime = app.llm_runtime.clone();
    let config = app.config.clone();
    tokio::spawn(async move {
        let ok = llm::health_check(&runtime, &config).await;
        let _ = tx.send(AppEvent::HealthCheckResult(ok));
    });
}

fn spawn_file_tree_load(dir: PathBuf, tx: UnboundedSender<AppEvent>) {
    tokio::spawn(async move {
        let result = fs::list_files(dir.to_string_lossy().into_owned()).await;
        let _ = tx.send(AppEvent::FileTreeLoaded(result));
    });
}

fn spawn_subdir_load(path: String, tx: UnboundedSender<AppEvent>) {
    tokio::spawn(async move {
        let result = fs::list_files(path.clone()).await;
        let _ = tx.send(AppEvent::SubdirLoaded {
            parent_path: path,
            result,
        });
    });
}

/// Check estimated token count and emit warnings/errors before the LLM call.
/// Returns `false` if the call should be aborted (context full).
fn check_context(
    config: &crate::config::Config,
    conversation: &[crate::llm::types::Message],
    tx: &UnboundedSender<AppEvent>,
) -> bool {
    if config.context_max_tokens == 0 {
        return true;
    }
    let estimated = llm::estimate_tokens(conversation);
    let ratio = estimated as f64 / config.context_max_tokens as f64;
    if ratio >= 1.0 {
        let _ = tx.send(AppEvent::LlmResponse {
            request_id: None,
            outcome: app::LlmOutcome::Error {
                message: format!(
                    "Conversation is too long (~{estimated} tokens, limit {}). \
                     Press Ctrl+L to clear and start fresh.",
                    config.context_max_tokens
                ),
            },
            conversation: conversation.to_vec(),
        });
        return false;
    }
    if ratio >= config.context_warn_ratio {
        let pct = (ratio * 100.0).round() as usize;
        let _ = tx.send(AppEvent::ContextWarning(format!(
            "Context is ~{pct}% full (~{estimated} / {} estimated tokens). \
             Consider pressing Ctrl+L to clear soon.",
            config.context_max_tokens
        )));
    }
    true
}

fn spawn_send_message(app: &mut App, tx: UnboundedSender<AppEvent>, text: String) {
    app.abort_in_flight();

    let runtime = app.llm_runtime.clone();
    let config = app.config.clone();
    let conversation = app.conversation.clone();
    let working_dir = app.working_dir.clone();

    if !check_context(&config, &conversation, &tx) {
        return;
    }

    let request_id = app.allocate_request_id();
    if config.streaming_enabled {
        let tx2 = tx.clone();
        let h = tokio::spawn(async move {
            let (outcome, conv) = app::send_message_streaming(
                runtime,
                config,
                conversation,
                working_dir,
                text,
                request_id,
                tx2.clone(),
            )
            .await;
            let _ = tx2.send(AppEvent::StreamComplete {
                request_id,
                outcome,
                conversation: conv,
            });
        });
        app.current_request = Some(h);
    } else {
        let h = tokio::spawn(async move {
            let (outcome, conv) =
                app::send_message(runtime, config, conversation, working_dir, text).await;
            let _ = tx.send(AppEvent::LlmResponse {
                request_id: Some(request_id),
                outcome,
                conversation: conv,
            });
        });
        app.current_request = Some(h);
    }
}

fn spawn_confirm_tool(app: &mut App, tx: UnboundedSender<AppEvent>, approved: bool) {
    let Some(pending) = app.pending_confirmation.clone() else {
        return;
    };

    app.abort_in_flight();

    let runtime = app.llm_runtime.clone();
    let config = app.config.clone();
    let working_dir = app.working_dir.clone();
    let conversation = app.conversation.clone();

    // Only check context when the user approves (cancellation never hits the LLM).
    if approved && !check_context(&config, &conversation, &tx) {
        return;
    }

    let request_id = app.allocate_request_id();
    if config.streaming_enabled {
        let tx2 = tx.clone();
        let h = tokio::spawn(async move {
            let (outcome, conv) = app::confirm_tool_streaming(
                runtime,
                config,
                working_dir,
                conversation,
                pending,
                approved,
                request_id,
                tx2.clone(),
            )
            .await;
            let _ = tx2.send(AppEvent::StreamComplete {
                request_id,
                outcome,
                conversation: conv,
            });
        });
        app.current_request = Some(h);
    } else {
        let h = tokio::spawn(async move {
            let (outcome, conv) = app::confirm_tool(
                runtime,
                config,
                working_dir,
                conversation,
                pending,
                approved,
            )
            .await;
            let _ = tx.send(AppEvent::LlmResponse {
                request_id: Some(request_id),
                outcome,
                conversation: conv,
            });
        });
        app.current_request = Some(h);
    }
}

// ── Textarea helpers ──────────────────────────────────────────────────────────

fn new_textarea() -> TextArea<'static> {
    let mut ta = TextArea::default();
    ta.set_block(
        ratatui::widgets::Block::default()
            .borders(ratatui::widgets::Borders::ALL)
            .title(" Message "),
    );
    ta
}

fn update_textarea_style(textarea: &mut TextArea, app: &App) {
    use ratatui::{
        style::Style,
        widgets::{Block, Borders},
    };

    let (title, style) = if app.streaming_text.is_some() {
        (
            " Streaming... — Esc to cancel ",
            Style::default().fg(ratatui::style::Color::DarkGray),
        )
    } else if app.is_loading {
        (
            " Waiting for response — Esc to cancel ",
            Style::default().fg(ratatui::style::Color::DarkGray),
        )
    } else if app.input_mode == InputMode::Confirming {
        (
            " Press [y] to confirm or [n] to cancel ",
            Style::default().fg(ratatui::style::Color::Yellow),
        )
    } else if app.working_dir.is_none() {
        (
            " Type /dir <path> to open a directory ",
            Style::default().fg(ratatui::style::Color::DarkGray),
        )
    } else {
        (
            " Message — Enter to send · Ctrl+L clear · Tab switch panel ",
            Style::default(),
        )
    };

    textarea.set_block(
        Block::default()
            .borders(Borders::ALL)
            .title(title)
            .border_style(style),
    );
}
