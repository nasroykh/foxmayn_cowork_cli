use std::io;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use chrono::{Local, TimeZone, Utc};

use crossterm::{
    cursor::Show,
    event::{
        DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture,
        Event, EventStream, KeyCode, KeyEventKind, KeyModifiers, MouseEventKind,
    },
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use futures::StreamExt;
use ratatui::{Terminal, backend::CrosstermBackend};
use ratatui_textarea::TextArea;
use tokio::sync::mpsc::{self, UnboundedSender};

use crate::app::{
    self, App, AppEvent, ChatEntry, ChatRole, InputMode, Panel, SlashPicker, SlashPickerItem,
};
use crate::config::{Config, Provider, ThinkingDisplay, ToolDisplayVerbosity};
use crate::fs;
use crate::llm;
use crate::llm::types::{OllamaThink, OllamaThinkLevel, ReasoningEffort, RequestReasoning};
use crate::storage;

pub mod commands;
pub mod ui;
pub mod widgets;

/// Minimum time between frames; stream chunks arriving faster are coalesced into one draw.
const MIN_FRAME: Duration = Duration::from_millis(16);
/// Health check cadence. Kept slow: the OpenRouter check is an authenticated request.
const HEALTH_INTERVAL: Duration = Duration::from_secs(60);

/// Restores the terminal on drop so every exit path (early `?`, error, unwind) leaves the
/// shell usable.
struct TerminalGuard;

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        restore_terminal();
    }
}

fn restore_terminal() {
    let _ = disable_raw_mode();
    let _ = execute!(
        io::stdout(),
        LeaveAlternateScreen,
        DisableMouseCapture,
        DisableBracketedPaste,
        Show
    );
}

/// Restore the terminal before the default hook prints, but only for panics on the main
/// thread. Spawned tasks run on worker threads: a panic there must not tear the screen down
/// while the app keeps running; `spawn_request` reports it as a chat error instead.
fn install_panic_hook() {
    let original_hook = std::panic::take_hook();
    let main_thread = std::thread::current().id();
    std::panic::set_hook(Box::new(move |info| {
        if std::thread::current().id() == main_thread {
            restore_terminal();
            original_hook(info);
        }
    }));
}

pub async fn run(mut app: App) -> io::Result<()> {
    install_panic_hook();
    enable_raw_mode()?;
    let _guard = TerminalGuard;
    execute!(
        io::stdout(),
        EnterAlternateScreen,
        EnableMouseCapture,
        EnableBracketedPaste
    )?;
    let mut terminal = Terminal::new(CrosstermBackend::new(io::stdout()))?;

    run_loop(&mut terminal, &mut app).await
}

async fn run_loop<B: ratatui::backend::Backend<Error = io::Error>>(
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

    // Periodic health check — first tick after one interval so it doesn't duplicate the startup check
    let start = tokio::time::Instant::now() + HEALTH_INTERVAL;
    let mut health_tick = tokio::time::interval_at(start, HEALTH_INTERVAL);

    let mut shutdown = Box::pin(shutdown_signal());
    let mut dirty = true;
    let mut last_draw: Option<tokio::time::Instant> = None;

    loop {
        // Coalesce redraws: a stream chunk or key burst only marks the frame dirty, and the
        // draw happens at most once per MIN_FRAME.
        let next_draw = last_draw.map_or_else(tokio::time::Instant::now, |t| t + MIN_FRAME);
        if dirty && tokio::time::Instant::now() >= next_draw {
            update_textarea_style(&mut textarea, app);
            terminal.draw(|f| ui::render(f, app, &textarea))?;
            last_draw = Some(tokio::time::Instant::now());
            dirty = false;
        }

        tokio::select! {
            maybe_event = events.next() => {
                match maybe_event {
                    Some(Ok(event)) => handle_crossterm_event(event, app, &mut textarea, &tx),
                    Some(Err(e)) => return Err(e),
                    None => return Err(io::Error::other("terminal event stream closed")),
                }
                dirty = true;
            }
            maybe_msg = rx.recv() => {
                let Some(msg) = maybe_msg else { break };
                handle_app_event(msg, app, &tx);
                dirty = true;
            }
            _ = health_tick.tick() => {
                spawn_health_check(app, tx.clone());
            }
            _ = &mut shutdown => {
                app.should_quit = true;
            }
            _ = tokio::time::sleep_until(next_draw), if dirty => {}
        }

        if app.should_quit {
            break;
        }
    }

    Ok(())
}

/// Resolves on SIGTERM / SIGHUP / SIGINT (unix) so the app quits cleanly and the terminal is
/// restored. Never resolves elsewhere (Ctrl+C arrives as a key event in raw mode).
async fn shutdown_signal() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};
        let (Ok(mut term), Ok(mut hup), Ok(mut int)) = (
            signal(SignalKind::terminate()),
            signal(SignalKind::hangup()),
            signal(SignalKind::interrupt()),
        ) else {
            return std::future::pending().await;
        };
        tokio::select! {
            _ = term.recv() => {}
            _ = hup.recv() => {}
            _ = int.recv() => {}
        }
    }
    #[cfg(not(unix))]
    std::future::pending::<()>().await
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
                // Normalise CRLF / lone CR so stray `\r` never reaches the input.
                let text = text.replace("\r\n", "\n").replace('\r', "\n");
                let mut first = true;
                for line in text.split('\n') {
                    if !first {
                        textarea.insert_newline();
                    }
                    textarea.insert_str(line);
                    first = false;
                }
                app.update_slash_completions(&textarea.lines().join("\n"));
            }
            return;
        }
        Event::Key(_) => {}
        _ => return,
    }

    let Event::Key(key) = event else { return };
    if !key_accepted(key.kind, key.code, key.modifiers, &app.input_mode) {
        return;
    }
    // The user is doing something else: a model list arriving now must not pop a picker over it.
    app.model_picker_pending = false;

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
                app.confirm_scroll = 0;
            }
            KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Esc => {
                spawn_confirm_tool(app, tx.clone(), false);
                app.input_mode = InputMode::Editing;
                app.pending_confirmation = None;
                app.confirm_scroll = 0;
            }
            // Long descriptions (bulk ops) scroll; the renderer clamps the offset.
            KeyCode::Up => app.confirm_scroll = app.confirm_scroll.saturating_sub(1),
            KeyCode::Down => app.confirm_scroll = app.confirm_scroll.saturating_add(1),
            KeyCode::PageUp => app.confirm_scroll = app.confirm_scroll.saturating_sub(8),
            KeyCode::PageDown => app.confirm_scroll = app.confirm_scroll.saturating_add(8),
            KeyCode::Home => app.confirm_scroll = 0,
            KeyCode::End => app.confirm_scroll = usize::MAX,
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
                if app.cancel_request() {
                    // Tools may already have changed disk before the abort.
                    finish_request(app, tx);
                }
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
                // If the completions popup is open, Enter accepts the highlighted entry only
                // for a partial command word; typed args / full names fall through to submit.
                if !app.slash_completions.is_empty()
                    && app.focused_panel == Panel::Chat
                    && let Some(&cmd_idx) = app.slash_completions.get(app.slash_selected)
                {
                    let typed = textarea.lines().join("\n");
                    match commands::decide_enter(&typed, cmd_idx) {
                        commands::SlashEnter::Submit => {}
                        commands::SlashEnter::Run(i) => {
                            let name = commands::COMMANDS[i].name.to_string();
                            app.slash_completions.clear();
                            app.slash_selected = 0;
                            *textarea = new_textarea();
                            dispatch_slash_command(&name, app, tx);
                            return;
                        }
                        commands::SlashEnter::Complete(i) => {
                            let completion = format!("{} ", commands::COMMANDS[i].name);
                            *textarea = new_textarea();
                            textarea.insert_str(&completion);
                            app.update_slash_completions(&completion);
                            return;
                        }
                    }
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
                // Aborts any in-flight request first so its late reply can't restore the old chat.
                app.clear_conversation();
            }
            // Tab: accept highlighted completion if popup is open, else switch panel.
            KeyCode::Tab
                if !app.slash_completions.is_empty() && app.focused_panel == Panel::Chat =>
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
            KeyCode::Up if app.slash_picker.is_some() && app.focused_panel == Panel::Chat => {
                if let Some(p) = app.slash_picker.as_mut() {
                    p.select_prev();
                }
            }
            KeyCode::Down if app.slash_picker.is_some() && app.focused_panel == Panel::Chat => {
                if let Some(p) = app.slash_picker.as_mut() {
                    p.select_next();
                }
            }
            KeyCode::Up
                if !app.slash_completions.is_empty() && app.focused_panel == Panel::Chat =>
            {
                app.slash_select_prev();
            }
            KeyCode::Down
                if !app.slash_completions.is_empty() && app.focused_panel == Panel::Chat =>
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

/// Windows reports Press and Release (and kitty-protocol terminals Repeat): act on Press only,
/// plus Repeat for navigation and typing keys while editing. Never on Release.
fn key_accepted(
    kind: KeyEventKind,
    code: KeyCode,
    modifiers: KeyModifiers,
    mode: &InputMode,
) -> bool {
    match kind {
        KeyEventKind::Press => true,
        KeyEventKind::Release => false,
        KeyEventKind::Repeat if *mode == InputMode::Confirming => matches!(
            code,
            KeyCode::Up | KeyCode::Down | KeyCode::PageUp | KeyCode::PageDown
        ),
        KeyEventKind::Repeat => {
            *mode == InputMode::Editing
                && match code {
                    KeyCode::Up
                    | KeyCode::Down
                    | KeyCode::Left
                    | KeyCode::Right
                    | KeyCode::Backspace
                    | KeyCode::Delete => true,
                    KeyCode::Char(_) => !modifiers.contains(KeyModifiers::CONTROL),
                    _ => false,
                }
        }
    }
}

fn dispatch_slash_command(text: &str, app: &mut App, tx: &UnboundedSender<AppEvent>) {
    let (cmd, arg) = text
        .split_once(' ')
        .map_or((text, ""), |(c, a)| (c, a.trim()));

    // Running any command ends a pending async model-picker request.
    app.model_picker_pending = false;

    match cmd {
        "/clear" => app.clear_conversation(),
        "/exit" => app.should_quit = true,
        "/dir" => {
            if arg.is_empty() {
                push_error(app, "Usage: /dir <path>");
            } else {
                match app.set_working_dir(PathBuf::from(arg)) {
                    Ok(dir) => {
                        spawn_file_tree_load(dir, tx.clone());
                        spawn_health_check(app, tx.clone());
                    }
                    Err(e) => push_error(app, &e),
                }
            }
        }
        "/model" => {
            if arg.is_empty() {
                match app.config.provider {
                    Provider::OpenRouter => {
                        open_model_picker(app, openrouter_model_list());
                    }
                    Provider::Ollama => {
                        app.model_picker_pending = true;
                        spawn_fetch_ollama_models(app, tx.clone());
                    }
                    Provider::Local => {
                        push_info(app, &format!("Current model: {}", app.config.model));
                    }
                }
            } else {
                mutate_config(app, |c| c.model = arg.to_string());
                persist_setting(app, storage::model_setting_key(&app.config.provider), arg);
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
            persist_setting(
                app,
                "streaming_enabled",
                if next { "true" } else { "false" },
            );
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
            if let Some(sessions) = sessions_or_notice(app) {
                let mut lines = vec!["Recent sessions — use /resume <id> to continue:".to_string()];
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
                if let Some(sessions) = sessions_or_notice(app) {
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
                    Ok(id) => match app.resume_session(id) {
                        Ok(()) => push_info(app, &format!("Session #{id} resumed.")),
                        Err(e) => push_error(app, &e),
                    },
                    Err(_) => push_error(app, "Usage: /resume <session-id>"),
                }
            }
        }
        _ => push_error(app, &format!("Unknown command: {cmd}")),
    }
}

/// Recent sessions for the open project, or `None` after pushing why there is nothing to show
/// (no project, storage error, or no sessions yet).
fn sessions_or_notice(app: &mut App) -> Option<Vec<storage::SessionSummary>> {
    match app.list_sessions() {
        Ok(s) if !s.is_empty() => Some(s),
        Ok(_) if app.storage.project.is_none() => {
            push_error(app, "No project open. Use /dir <path> first.");
            None
        }
        Ok(_) => {
            push_info(app, "No sessions yet for this project.");
            None
        }
        Err(e) => {
            push_error(app, &format!("Could not read sessions: {e}"));
            None
        }
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
        .map(|utc| {
            utc.with_timezone(&Local)
                .format("%Y-%m-%d %H:%M")
                .to_string()
        })
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
                            c.openrouter_reasoning = Some(RequestReasoning {
                                effort: Some(effort),
                                summary,
                            });
                        });
                        persist_setting(app, "openrouter_reasoning", arg);
                        push_info(app, &format!("OpenRouter reasoning set to: {arg}"));
                    }
                    Err(_) => {
                        push_error(
                            app,
                            "Usage: /reasoning <off|minimal|none|low|medium|high|xhigh>",
                        );
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
        Some(OllamaThink::OnOff(true)) | Some(OllamaThink::Level(OllamaThinkLevel::High)) => "high",
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
    match client
        .get(&url)
        .timeout(std::time::Duration::from_secs(5))
        .send()
        .await
    {
        Ok(resp) => match resp.json::<TagsResponse>().await {
            Ok(data) => data.models.into_iter().map(|m| m.name).collect(),
            Err(_) => Vec::new(),
        },
        Err(_) => Vec::new(),
    }
}

/// Persist the session and surface a failure as a chat warning instead of losing data silently.
fn save_session_or_warn(app: &mut App) {
    if let Err(e) = app.save_current_session() {
        app.chat_messages.push(ChatEntry {
            role: ChatRole::Warning,
            content: format!("Session not saved: {e}"),
        });
    }
}

/// Reload the file tree, keeping expansion state. Tools may have changed disk during a request
/// even when it ended in an error or was cancelled.
fn refresh_file_tree(app: &mut App, tx: &UnboundedSender<AppEvent>) {
    if let Some(dir) = app.working_dir.clone() {
        app.prepare_refresh();
        spawn_file_tree_load(dir, tx.clone());
    }
}

/// Common tail of every request that ended (success, error, or cancel).
fn finish_request(app: &mut App, tx: &UnboundedSender<AppEvent>) {
    save_session_or_warn(app);
    refresh_file_tree(app, tx);
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
            // A task panic reports through here while a stream may be mid-flight.
            app.finalize_stream();
            let ended = !matches!(outcome, app::LlmOutcome::PendingConfirmation { .. });
            app.current_request = None;
            app.active_request_id = None;
            app.handle_outcome(outcome, conversation);
            if ended {
                finish_request(app, tx);
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
            let ended = !matches!(outcome, app::LlmOutcome::PendingConfirmation { .. });
            app.current_request = None;
            app.active_request_id = None;
            app.handle_outcome(outcome, conversation);
            if ended {
                finish_request(app, tx);
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
            // Ignore a late reply once the user has moved on (typed, ran another command).
            if !std::mem::take(&mut app.model_picker_pending) {
                return;
            }
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

/// Aborts the wrapped task when dropped, so aborting the outer request handle also stops the
/// inner task it supervises.
struct AbortOnDrop(tokio::task::JoinHandle<()>);

impl Drop for AbortOnDrop {
    fn drop(&mut self) {
        self.0.abort();
    }
}

fn panic_message(payload: Box<dyn std::any::Any + Send>) -> String {
    match payload.downcast::<String>() {
        Ok(s) => *s,
        Err(p) => p
            .downcast::<&'static str>()
            .map_or_else(|_| "unknown panic".to_string(), |s| (*s).to_string()),
    }
}

/// Run a request task under a supervisor. If it panics, report an error for `request_id`
/// (restoring `fallback` as the conversation) instead of leaving the UI stuck in "loading".
/// The returned handle is the one stored in `App::current_request`; aborting it aborts the task.
fn spawn_request(
    tx: UnboundedSender<AppEvent>,
    request_id: app::RequestId,
    fallback: Vec<crate::llm::types::Message>,
    fut: impl std::future::Future<Output = ()> + Send + 'static,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut inner = AbortOnDrop(tokio::spawn(fut));
        if let Err(e) = (&mut inner.0).await
            && e.is_panic()
        {
            let _ = tx.send(AppEvent::LlmResponse {
                request_id: Some(request_id),
                outcome: app::LlmOutcome::Error {
                    message: format!(
                        "Internal error: the request task panicked ({})",
                        panic_message(e.into_panic())
                    ),
                },
                conversation: fallback,
            });
        }
    })
}

fn spawn_send_message(app: &mut App, tx: UnboundedSender<AppEvent>, text: String) {
    app.abort_in_flight();
    app.is_loading = true;

    let runtime = app.llm_runtime.clone();
    let config = app.config.clone();
    let conversation = app.conversation.clone();
    let working_dir = app.working_dir.clone();

    if !check_context(&config, &conversation, &tx) {
        return;
    }

    let request_id = app.allocate_request_id();
    let fallback = conversation.clone();
    let tx2 = tx.clone();
    let h = if config.streaming_enabled {
        spawn_request(tx, request_id, fallback, async move {
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
        })
    } else {
        spawn_request(tx, request_id, fallback, async move {
            let (outcome, conv) =
                app::send_message(runtime, config, conversation, working_dir, text).await;
            let _ = tx2.send(AppEvent::LlmResponse {
                request_id: Some(request_id),
                outcome,
                conversation: conv,
            });
        })
    };
    app.current_request = Some(h);
}

fn spawn_confirm_tool(app: &mut App, tx: UnboundedSender<AppEvent>, approved: bool) {
    let Some(pending) = app.pending_confirmation.clone() else {
        return;
    };

    app.abort_in_flight();
    // Also while declining: a fast Enter must not abort the decline task and send the open
    // snapshot as a new message.
    app.is_loading = true;

    let runtime = app.llm_runtime.clone();
    let config = app.config.clone();
    let working_dir = app.working_dir.clone();
    let conversation = app.conversation.clone();

    // No context-size check here: the user already decided, and bailing out would leave the
    // pending snapshot unanswered and the approved tool unrun.

    let request_id = app.allocate_request_id();
    let fallback = conversation.clone();
    let tx2 = tx.clone();
    let h = if config.streaming_enabled {
        spawn_request(tx, request_id, fallback, async move {
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
        })
    } else {
        spawn_request(tx, request_id, fallback, async move {
            let (outcome, conv) = app::confirm_tool(
                runtime,
                config,
                working_dir,
                conversation,
                pending,
                approved,
            )
            .await;
            let _ = tx2.send(AppEvent::LlmResponse {
                request_id: Some(request_id),
                outcome,
                conversation: conv,
            });
        })
    };
    app.current_request = Some(h);
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn release_events_are_ignored() {
        let none = KeyModifiers::NONE;
        assert!(!key_accepted(
            KeyEventKind::Release,
            KeyCode::Char('a'),
            none,
            &InputMode::Editing
        ));
        assert!(key_accepted(
            KeyEventKind::Press,
            KeyCode::Enter,
            none,
            &InputMode::Editing
        ));
    }

    #[test]
    fn repeat_only_for_navigation_and_typing() {
        let none = KeyModifiers::NONE;
        let edit = InputMode::Editing;
        assert!(key_accepted(
            KeyEventKind::Repeat,
            KeyCode::Down,
            none,
            &edit
        ));
        assert!(key_accepted(
            KeyEventKind::Repeat,
            KeyCode::Char('a'),
            none,
            &edit
        ));
        assert!(!key_accepted(
            KeyEventKind::Repeat,
            KeyCode::Enter,
            none,
            &edit
        ));
        assert!(!key_accepted(
            KeyEventKind::Repeat,
            KeyCode::Tab,
            none,
            &edit
        ));
        assert!(!key_accepted(
            KeyEventKind::Repeat,
            KeyCode::Char('l'),
            KeyModifiers::CONTROL,
            &edit
        ));
        assert!(!key_accepted(
            KeyEventKind::Repeat,
            KeyCode::Char('y'),
            none,
            &InputMode::Confirming
        ));
    }
}
