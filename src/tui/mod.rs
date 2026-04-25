use std::io;
use std::path::PathBuf;
use std::time::Duration;

use crossterm::{
    event::{DisableMouseCapture, EnableMouseCapture, Event, EventStream, KeyCode, KeyModifiers, MouseEventKind},
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use futures::StreamExt;
use ratatui::{Terminal, backend::CrosstermBackend};
use tokio::sync::mpsc::{self, UnboundedSender};
use tui_textarea::TextArea;

use crate::app::{self, App, AppEvent, InputMode, Panel};
use crate::fs;
use crate::llm;

pub mod ui;
pub mod widgets;

pub async fn run(mut app: App) -> io::Result<()> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    // Restore terminal on panic so the shell isn't left in raw mode
    let original_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let _ = disable_raw_mode();
        let _ = crossterm::execute!(io::stdout(), LeaveAlternateScreen, DisableMouseCapture);
        original_hook(info);
    }));

    let result = run_loop(&mut terminal, &mut app).await;

    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen, DisableMouseCapture)?;
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
            }
            KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Esc => {
                spawn_confirm_tool(app, tx.clone(), false);
            }
            _ => {}
        },

        InputMode::Editing => match key.code {
            KeyCode::Enter => {
                // File tree panel: Enter toggles directory expand/collapse
                if app.focused_panel == Panel::FileTree {
                    if let Some(path) = app.toggle_expand() {
                        spawn_subdir_load(path, tx.clone());
                    }
                    return;
                }
                // Chat panel: Enter submits the message
                let text = textarea.lines().join("\n");
                let text = text.trim().to_string();
                if text.is_empty() || app.is_loading {
                    return;
                }
                if let Some(path) = text.strip_prefix("/dir ") {
                    let dir = PathBuf::from(path.trim());
                    app.set_working_dir(dir.clone());
                    spawn_file_tree_load(dir, tx.clone());
                    spawn_health_check(app, tx.clone());
                } else {
                    app.begin_send(&text);
                    spawn_send_message(app, tx.clone(), text);
                }
                *textarea = new_textarea();
            }
            KeyCode::Right if app.focused_panel == Panel::FileTree => {
                if let Some(path) = app.toggle_expand() {
                    spawn_subdir_load(path, tx.clone());
                }
            }
            KeyCode::Left if app.focused_panel == Panel::FileTree => {
                let idx = app.file_tree_scroll;
                let should_collapse =
                    app.file_tree.get(idx).is_some_and(|e| e.is_dir && e.expanded);
                if should_collapse {
                    app.collapse_dir(idx);
                } else {
                    app.jump_to_parent();
                }
            }
            KeyCode::Char('l') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                app.clear_conversation();
            }
            KeyCode::Tab => {
                app.focused_panel = match app.focused_panel {
                    Panel::Chat => Panel::FileTree,
                    Panel::FileTree => Panel::Chat,
                };
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
            }
        },
    }
}

fn handle_app_event(event: AppEvent, app: &mut App, tx: &UnboundedSender<AppEvent>) {
    match event {
        AppEvent::LlmResponse(outcome, conversation) => {
            // Refresh the file tree after any successful LLM response so newly
            // created/deleted/renamed files appear without a manual /dir reload.
            let should_refresh = matches!(outcome, app::LlmOutcome::Complete { .. });
            app.handle_outcome(outcome, conversation);
            if should_refresh && let Some(dir) = app.working_dir.clone() {
                spawn_file_tree_load(dir, tx.clone());
            }
        }
        AppEvent::StreamChunk(chunk) => {
            app.handle_stream_chunk(&chunk);
        }
        AppEvent::StreamComplete(outcome, conversation) => {
            app.finalize_stream();
            let should_refresh = matches!(outcome, app::LlmOutcome::Complete { .. });
            app.handle_outcome(outcome, conversation);
            if should_refresh && let Some(dir) = app.working_dir.clone() {
                spawn_file_tree_load(dir, tx.clone());
            }
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
        }
        AppEvent::SubdirLoaded { parent_path, result } => {
            app.handle_subdir_loaded(parent_path, result);
        }
    }
}

// ── Async task spawners ───────────────────────────────────────────────────────

fn spawn_health_check(app: &App, tx: UnboundedSender<AppEvent>) {
    let client = app.http_client.clone();
    let config = app.config.clone();
    tokio::spawn(async move {
        let ok = llm::health_check(&client, &config).await;
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
        let _ = tx.send(AppEvent::LlmResponse(
            app::LlmOutcome::Error {
                message: format!(
                    "Conversation is too long (~{estimated} tokens, limit {}). \
                     Press Ctrl+L to clear and start fresh.",
                    config.context_max_tokens
                ),
            },
            conversation.to_vec(),
        ));
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

fn spawn_send_message(app: &App, tx: UnboundedSender<AppEvent>, text: String) {
    let client = app.http_client.clone();
    let config = app.config.clone();
    let conversation = app.conversation.clone();
    let working_dir = app.working_dir.clone();

    if !check_context(&config, &conversation, &tx) {
        return;
    }

    if config.streaming_enabled {
        let tx2 = tx.clone();
        tokio::spawn(async move {
            let (outcome, conv) =
                app::send_message_streaming(client, config, conversation, working_dir, text, tx2.clone())
                    .await;
            let _ = tx2.send(AppEvent::StreamComplete(outcome, conv));
        });
    } else {
        tokio::spawn(async move {
            let (outcome, conv) =
                app::send_message(client, config, conversation, working_dir, text).await;
            let _ = tx.send(AppEvent::LlmResponse(outcome, conv));
        });
    }
}

fn spawn_confirm_tool(app: &App, tx: UnboundedSender<AppEvent>, approved: bool) {
    let Some(pending) = app.pending_confirmation.clone() else {
        return;
    };
    let client = app.http_client.clone();
    let config = app.config.clone();
    let working_dir = app.working_dir.clone();
    let conversation = app.conversation.clone();

    // Only check context when the user approves (cancellation never hits the LLM).
    if approved && !check_context(&config, &conversation, &tx) {
        return;
    }

    if config.streaming_enabled {
        let tx2 = tx.clone();
        tokio::spawn(async move {
            let (outcome, conv) =
                app::confirm_tool_streaming(client, config, working_dir, conversation, pending, approved, tx2.clone())
                    .await;
            let _ = tx2.send(AppEvent::StreamComplete(outcome, conv));
        });
    } else {
        tokio::spawn(async move {
            let (outcome, conv) =
                app::confirm_tool(client, config, working_dir, conversation, pending, approved)
                    .await;
            let _ = tx.send(AppEvent::LlmResponse(outcome, conv));
        });
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
            " Streaming... ",
            Style::default().fg(ratatui::style::Color::DarkGray),
        )
    } else if app.is_loading {
        (
            " Waiting for response... ",
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
