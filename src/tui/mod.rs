//! The full-screen terminal front end.
//!
//! Owns the screen rather than scrolling through it, which is what makes a
//! persistent input box, live streaming text, and inline tool approval
//! possible — all things the line-based CLI structurally can't do.
//!
//! It drives a [`Conversation`] worker and renders the events it emits; it
//! never calls the agent loop itself. A GUI would attach at the same place.
//!
//! Three screens: a launch list, a sessions browser, and the conversation
//! itself. Only the conversation owns a worker, and it's shut down cleanly
//! whenever you navigate away, so switching sessions can't leave a turn
//! running against a screen nobody is looking at.

mod app;
mod picker;
mod render;

use crate::agent::AGENT_CHAT_SYSTEM_PROMPT;
use crate::client::{ChatMessage, Client};
use crate::config::ApprovalSettings;
use crate::conversation::{Command, Conversation};
use crate::session::ChatSession;
use crate::store::{self, SessionSummary, StoredMessage, KIND_AGENT_CHAT, KIND_CHAT};
use crate::ui::{parse_yes_no, response_label};
use anyhow::Result;
use app::{App, Focus, TranscriptItem};
use crossterm::event::{
    DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture,
    Event as TermEvent, EventStream, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseEvent,
    MouseEventKind,
};
use crossterm::{execute, terminal};
use futures_util::StreamExt;
use picker::{Activation, Picker, SessionRow};
use ratatui::backend::CrosstermBackend;
use ratatui::Terminal;
use std::io::{self, Stdout};
use std::sync::Arc;
use std::time::Duration;

/// How often the screen redraws while idle, driving the spinner.
const TICK: Duration = Duration::from_millis(100);

/// Everything needed to start conversations on demand, since with a launch
/// screen the TUI no longer receives a ready-made session.
pub struct Context {
    pub client: Arc<Client>,
    /// Model for new sessions; a resumed one keeps its own unless the user
    /// passed `--model`.
    pub default_model: String,
    pub model_override: Option<String>,
    pub effort_level: Option<String>,
    pub max_iterations: usize,
    pub temperature: f32,
    pub approval: ApprovalSettings,
}

/// Where the TUI opens.
pub enum Start {
    /// The launch screen (bare `comms tui`).
    Launch,
    /// Straight into a new session (`--session`), starting in ask mode —
    /// same as choosing "New session" on the launch screen, minus the
    /// naming prompt.
    New,
    /// Straight into a saved session (`--resume`).
    Resume(Box<SessionSummary>),
}

enum Screen {
    Launch(Picker),
    Sessions(Picker),
    /// Naming a new session before it's created — reached by choosing "New
    /// session" on the launch screen.
    NameSession {
        input: String,
    },
    Chat(Box<Chat>),
}

struct Chat {
    app: App,
    conversation: Conversation,
}

/// Runs the TUI until the user quits.
pub async fn run(context: Context, start: Start) -> Result<()> {
    let mut screen = match start {
        Start::Launch => Screen::Launch(Picker::launch(load_sessions()?)),
        Start::New => Screen::Chat(Box::new(open_new(&context, false, None)?)),
        Start::Resume(summary) => Screen::Chat(Box::new(open_resumed(&context, &summary)?)),
    };

    let mut terminal = enter()?;
    // Restore the terminal even on the way out of an error, so a failure
    // never leaves the user with a broken shell.
    let result = event_loop(&mut terminal, &context, &mut screen).await;
    if let Screen::Chat(chat) = screen {
        chat.conversation.shutdown().await;
    }
    leave(&mut terminal)?;
    result
}

fn load_sessions() -> Result<Vec<SessionRow>> {
    let conn = store::open_db()?;
    Ok(store::list_sessions(&conn)?
        .into_iter()
        .map(SessionRow::from)
        .collect())
}

/// Each conversation gets its own database handle, so sessions can be
/// opened and closed over the life of the TUI without threading one
/// connection through every screen.
fn open_new(context: &Context, agentic: bool, title: Option<String>) -> Result<Chat> {
    let kind = if agentic { KIND_AGENT_CHAT } else { KIND_CHAT };
    let model = context
        .model_override
        .clone()
        .unwrap_or_else(|| context.default_model.clone());

    let mut session = ChatSession::create(
        store::open_db()?,
        model,
        kind,
        context.effort_level.clone(),
        context.approval.clone(),
    )?;
    if let Some(title) = title {
        session.set_title(title)?;
    }
    if agentic {
        session.push_and_persist(ChatMessage {
            role: "system".to_string(),
            content: Some(AGENT_CHAT_SYSTEM_PROMPT.to_string()),
            tool_calls: None,
            tool_call_id: None,
        })?;
    }

    Ok(start_chat(context, session, Vec::new(), agentic))
}

fn open_resumed(context: &Context, summary: &SessionSummary) -> Result<Chat> {
    // Prefer this session's own persisted effort — a real `/effort` switch
    // — over the general configured default, the same way its model does.
    let effort_level = summary
        .effort_level
        .clone()
        .or_else(|| context.effort_level.clone());
    let (session, history) = ChatSession::resume(
        store::open_db()?,
        summary,
        summary.model.clone(),
        effort_level,
    )?;
    let agentic = summary.kind == KIND_AGENT_CHAT;
    let mut chat = start_chat(context, session, history, agentic);
    // A resumed session keeps its own saved settings; `-m` (like any other
    // override flag) only ever applies to a brand new one.
    if context.model_override.is_some() {
        chat.app.transcript.push(TranscriptItem::Notice(
            "Ignoring --model: resumed sessions keep their saved model".to_string(),
        ));
    }
    Ok(chat)
}

fn open_row(context: &Context, row: &SessionRow) -> Result<Chat> {
    let conn = store::open_db()?;
    let summary = store::find_session(&conn, &row.id)?
        .ok_or_else(|| anyhow::anyhow!("Session {} no longer exists", row.short_id()))?;
    open_resumed(context, &summary)
}

fn start_chat(
    context: &Context,
    session: ChatSession,
    history: Vec<StoredMessage>,
    agentic: bool,
) -> Chat {
    let mut app = App::new(
        session.model().to_string(),
        session.effort_level().map(str::to_string),
        session.short_id().to_string(),
    );
    app.agentic = agentic;
    app.verbose = session.verbose();
    app.max_iterations = session.max_iterations();
    app.temperature = session.temperature();
    app.approval = session.approval().clone();
    app.title = session.title().to_string();
    seed_transcript(&mut app, &history);

    let conversation = Conversation::spawn(
        Arc::clone(&context.client),
        session,
        context.max_iterations,
        context.temperature,
        agentic,
    );
    Chat { app, conversation }
}

/// Replays a resumed session into the transcript so the TUI opens showing
/// the conversation so far.
fn seed_transcript(app: &mut App, history: &[StoredMessage]) {
    for stored in history {
        let message = &stored.message;
        match message.role.as_str() {
            "user" => {
                if let Some(text) = &message.content {
                    app.transcript.push(TranscriptItem::User(text.clone()));
                    // So Up/Down can recall prompts from before this resume,
                    // not just what's typed in the current sitting.
                    app.input_history.push(text.clone());
                }
            }
            "assistant" => {
                if let Some(text) = &message.content {
                    if !text.trim().is_empty() {
                        // Each stored message knows the model that produced
                        // it, so a session whose model changed part-way
                        // replays with each reply correctly attributed.
                        let label = stored
                            .model
                            .as_ref()
                            .map(|model| response_label(model, &stored.effort_level));
                        app.transcript.push(TranscriptItem::Assistant {
                            text: text.clone(),
                            streaming: false,
                            label,
                        });
                    }
                }
            }
            // Tool results and the system prompt are bookkeeping; a resumed
            // view shows the conversation, not the plumbing.
            _ => {}
        }
    }
}

type Tui = Terminal<CrosstermBackend<Stdout>>;

fn enter() -> Result<Tui> {
    terminal::enable_raw_mode()?;
    let mut stdout = io::stdout();
    // Without this, a terminal delivers a paste as plain keystrokes, and
    // any embedded newline reads as a real Enter — submitting each pasted
    // line as its own message instead of landing in the input box as text.
    // Mouse capture is what lets the scroll wheel move the transcript
    // instead of the terminal's own (unrelated) native scrollback.
    execute!(
        stdout,
        terminal::EnterAlternateScreen,
        EnableBracketedPaste,
        EnableMouseCapture
    )?;

    // A panic while in raw mode would otherwise leave the terminal unusable
    // with no echo and no cursor, so restore first, then panic normally.
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let _ = terminal::disable_raw_mode();
        let _ = execute!(
            io::stdout(),
            DisableBracketedPaste,
            DisableMouseCapture,
            terminal::LeaveAlternateScreen
        );
        previous(info);
    }));

    Ok(Terminal::new(CrosstermBackend::new(stdout))?)
}

fn leave(terminal: &mut Tui) -> Result<()> {
    terminal::disable_raw_mode()?;
    execute!(
        terminal.backend_mut(),
        DisableBracketedPaste,
        DisableMouseCapture,
        terminal::LeaveAlternateScreen
    )?;
    terminal.show_cursor()?;
    Ok(())
}

/// What woke the loop. Resolving the select into this first keeps the
/// screen borrow short, so handling can then mutate it freely.
enum Wake {
    Key(TermEvent),
    Conversation(Option<crate::conversation::Event>),
    Tick,
}

/// Waits on the conversation worker when one exists, and otherwise never
/// resolves, so the same `select!` serves every screen.
async fn next_conversation_event(screen: &mut Screen) -> Option<crate::conversation::Event> {
    match screen {
        Screen::Chat(chat) => chat.conversation.next_event().await,
        _ => std::future::pending().await,
    }
}

fn draw(terminal: &mut Tui, screen: &Screen, tick: usize) -> Result<()> {
    terminal.draw(|frame| match screen {
        Screen::Launch(p) => picker::draw(frame, p, "comms", "↑/↓ move · Enter open · q quit"),
        Screen::Sessions(p) => picker::draw(
            frame,
            p,
            "sessions",
            "↑/↓ move · Enter resume · r rename · d delete · Esc back · q quit",
        ),
        Screen::NameSession { input } => picker::draw_naming(frame, input),
        Screen::Chat(chat) => render::draw(frame, &chat.app, tick),
    })?;
    Ok(())
}

async fn event_loop(terminal: &mut Tui, context: &Context, screen: &mut Screen) -> Result<()> {
    let mut keys = EventStream::new();
    let mut ticker = tokio::time::interval(TICK);
    let mut tick = 0usize;
    let mut quit = false;

    draw(terminal, screen, tick)?;

    while !quit {
        let wake = tokio::select! {
            Some(Ok(event)) = keys.next() => Wake::Key(event),
            event = next_conversation_event(screen) => Wake::Conversation(event),
            _ = ticker.tick() => Wake::Tick,
        };

        let mut dirty = false;
        match wake {
            Wake::Key(TermEvent::Key(key)) if key.kind == KeyEventKind::Press => {
                quit = handle_key(context, screen, key).await?;
                dirty = true;
            }
            Wake::Key(TermEvent::Paste(text)) => {
                if let Screen::Chat(chat) = screen {
                    chat.app.paste(&text);
                }
                dirty = true;
            }
            Wake::Key(TermEvent::Resize(_, _)) => dirty = true,
            Wake::Key(TermEvent::Mouse(mouse)) => {
                if let Screen::Chat(chat) = screen {
                    handle_mouse_scroll(&mut chat.app, mouse);
                    dirty = true;
                }
            }
            Wake::Key(_) => {}
            Wake::Conversation(Some(event)) => {
                if let Screen::Chat(chat) = screen {
                    chat.app.apply(event);
                }
                dirty = true;
            }
            // The worker stopped on its own; nothing more will arrive.
            Wake::Conversation(None) => {}
            Wake::Tick => {
                if matches!(screen, Screen::Chat(chat) if chat.app.busy) {
                    tick = tick.wrapping_add(1);
                    dirty = true;
                }
            }
        }

        if dirty && !quit {
            draw(terminal, screen, tick)?;
        }
    }

    Ok(())
}

/// Handles one keypress. Returns whether the TUI should exit.
async fn handle_key(context: &Context, screen: &mut Screen, key: KeyEvent) -> Result<bool> {
    // Quit works from anywhere, including mid-turn.
    if key.modifiers.contains(KeyModifiers::CONTROL) && matches!(key.code, KeyCode::Char('c')) {
        return Ok(true);
    }

    match screen {
        Screen::Launch(_) | Screen::Sessions(_) => handle_picker_key(context, screen, key).await,
        Screen::NameSession { .. } => handle_naming_key(context, screen, key),
        Screen::Chat(chat) => {
            if handle_chat_key(&mut chat.app, &chat.conversation, key) {
                // Leaving the conversation: stop its worker before the
                // screen is replaced, so its final writes land.
                let Screen::Chat(chat) =
                    std::mem::replace(screen, Screen::Launch(Picker::launch(load_sessions()?)))
                else {
                    unreachable!("just matched Chat")
                };
                chat.conversation.shutdown().await;
                // The list was loaded before the shutdown flushed this
                // session, so refresh it to show the up-to-date title.
                *screen = Screen::Launch(Picker::launch(load_sessions()?));
            }
            Ok(false)
        }
    }
}

async fn handle_picker_key(context: &Context, screen: &mut Screen, key: KeyEvent) -> Result<bool> {
    let is_sessions = matches!(screen, Screen::Sessions(_));
    let (Screen::Launch(p) | Screen::Sessions(p)) = screen else {
        unreachable!("picker screens only")
    };

    // A pending rename swallows everything until it's answered.
    if p.renaming.is_some() {
        match key.code {
            KeyCode::Esc => p.cancel_rename(),
            KeyCode::Enter => {
                if let Some((id, title)) = p.confirm_rename() {
                    let conn = store::open_db()?;
                    store::set_session_title(&conn, &id, &title)?;
                    p.apply_rename(&id, title);
                }
            }
            KeyCode::Backspace => p.rename_backspace(),
            KeyCode::Char(c) => p.rename_insert_char(c),
            _ => {}
        }
        return Ok(false);
    }

    // A pending delete swallows everything until it's answered.
    if p.confirming_delete.is_some() {
        let action = match key.code {
            KeyCode::Char('y') | KeyCode::Char('Y') => p.resolve_delete(true),
            _ => p.resolve_delete(false),
        };
        if let Some(Activation::Delete(row)) = action {
            let conn = store::open_db()?;
            store::delete_session(&conn, &row.id)?;
            p.remove_session(&row.id);
        }
        return Ok(false);
    }

    match key.code {
        KeyCode::Char('q') => return Ok(true),
        KeyCode::Up | KeyCode::Char('k') => p.move_up(),
        KeyCode::Down | KeyCode::Char('j') => p.move_down(),
        KeyCode::Char('r') if is_sessions => p.begin_rename(),
        KeyCode::Char('d') if is_sessions => p.begin_delete(),
        KeyCode::Esc if is_sessions => {
            *screen = Screen::Launch(Picker::launch(load_sessions()?));
        }
        KeyCode::Enter => {
            let Some(activation) = p.activate() else {
                return Ok(false);
            };
            match activation {
                Activation::NewSession => {
                    *screen = Screen::NameSession {
                        input: String::new(),
                    }
                }
                Activation::Resume(row) => {
                    *screen = Screen::Chat(Box::new(open_row(context, &row)?))
                }
                Activation::BrowseAll => {
                    *screen = Screen::Sessions(Picker::sessions(load_sessions()?))
                }
                // Delete is resolved by the confirmation flow, not here.
                Activation::Delete(_) => {}
            }
        }
        _ => {}
    }
    Ok(false)
}

/// Handles a keypress on the new-session naming prompt. Returns whether the
/// TUI should exit (always `false` — quitting from here isn't supported,
/// same as any other picker screen).
fn handle_naming_key(context: &Context, screen: &mut Screen, key: KeyEvent) -> Result<bool> {
    let Screen::NameSession { input } = screen else {
        unreachable!("naming screen only")
    };

    match key.code {
        KeyCode::Esc => *screen = Screen::Launch(Picker::launch(load_sessions()?)),
        KeyCode::Enter => {
            let title = input.trim();
            let title = (!title.is_empty()).then(|| title.to_string());
            *screen = Screen::Chat(Box::new(open_new(context, false, title)?));
        }
        KeyCode::Backspace => {
            input.pop();
        }
        KeyCode::Char(c) => input.push(c),
        _ => {}
    }
    Ok(false)
}

/// How many transcript lines one wheel notch moves — a finer step than
/// PageUp/PageDown's 5, since a notch is closer to a nudge than a page.
const MOUSE_SCROLL_STEP: u16 = 3;

/// Scrolls the transcript with the wheel, the mouse counterpart to
/// PageUp/PageDown. Left `app.scroll_back` untouched (and the input box
/// alone) for any other mouse event — clicks aren't wired to anything yet.
fn handle_mouse_scroll(app: &mut App, mouse: MouseEvent) {
    match mouse.kind {
        MouseEventKind::ScrollUp => {
            app.scroll_back = app.scroll_back.saturating_add(MOUSE_SCROLL_STEP);
        }
        MouseEventKind::ScrollDown => {
            app.scroll_back = app.scroll_back.saturating_sub(MOUSE_SCROLL_STEP);
        }
        _ => {}
    }
}

/// Handles a keypress in the conversation. Returns whether to leave it and
/// go back to the launch screen.
fn handle_chat_key(app: &mut App, conversation: &Conversation, key: KeyEvent) -> bool {
    if app.focus == Focus::Approval {
        match key.code {
            KeyCode::Enter => {
                let allowed = parse_yes_no(&app.take_approval_answer());
                conversation.send(Command::Approve(allowed));
                app.approval_answered(allowed);
            }
            KeyCode::Esc => conversation.send(Command::Cancel),
            KeyCode::Backspace => app.backspace(),
            KeyCode::Left => app.move_left(),
            KeyCode::Right => app.move_right(),
            KeyCode::Char(c) => app.insert_char(c),
            _ => {}
        }
        return false;
    }

    // Ctrl-B backs out to the launch screen; plain Esc stays reserved for
    // cancelling a turn, which is needed far more often.
    if key.modifiers.contains(KeyModifiers::CONTROL) && matches!(key.code, KeyCode::Char('b')) {
        return true;
    }

    match key.code {
        // Alt-Enter always inserts a newline; Shift-Enter does too on the
        // terminals that report the modifier at all, which many don't.
        KeyCode::Enter
            if key
                .modifiers
                .intersects(KeyModifiers::ALT | KeyModifiers::SHIFT) =>
        {
            app.insert_char('\n')
        }
        KeyCode::Enter => {
            if let Some(text) = app.take_input() {
                match app::classify(&text) {
                    app::Submission::Message(text) => conversation.send(Command::Send(text)),
                    app::Submission::SetModel(model) => conversation.send(Command::SetModel(model)),
                    // Answered by the worker so the reply reflects what the
                    // session actually holds, not what the UI assumes.
                    app::Submission::ShowModel => {
                        conversation.send(Command::SetModel(app.model.clone()))
                    }
                    app::Submission::SetAgentic(agentic) => {
                        conversation.send(Command::SetAgentic(agentic))
                    }
                    app::Submission::SetEffort(effort_level) => {
                        conversation.send(Command::SetEffort(effort_level))
                    }
                    app::Submission::ToggleVerbose => conversation.send(Command::ToggleVerbose),
                    app::Submission::SetMaxIterations(max_iterations) => {
                        conversation.send(Command::SetMaxIterations(max_iterations))
                    }
                    app::Submission::SetTemperature(temperature) => {
                        conversation.send(Command::SetTemperature(temperature))
                    }
                    app::Submission::SetApproval { category, enabled } => {
                        conversation.send(Command::SetApproval { category, enabled })
                    }
                    // Purely a read of state the UI already holds — no need
                    // to round-trip through the worker.
                    app::Submission::ShowApproval => {
                        app.transcript.push(TranscriptItem::ApprovalStatus {
                            approval: app.approval.clone(),
                            changed: false,
                        });
                    }
                }
            }
        }
        KeyCode::Esc => {
            if app.busy {
                conversation.send(Command::Cancel);
            }
        }
        KeyCode::Backspace => app.backspace(),
        KeyCode::Left => app.move_left(),
        KeyCode::Right => app.move_right(),
        KeyCode::Up => app.history_up(),
        KeyCode::Down => app.history_down(),
        KeyCode::PageUp => app.scroll_back = app.scroll_back.saturating_add(5),
        KeyCode::PageDown => app.scroll_back = app.scroll_back.saturating_sub(5),
        KeyCode::End => app.scroll_back = 0,
        KeyCode::Char(c) => app.insert_char(c),
        _ => {}
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mouse(kind: MouseEventKind) -> MouseEvent {
        MouseEvent {
            kind,
            column: 0,
            row: 0,
            modifiers: KeyModifiers::empty(),
        }
    }

    #[test]
    fn wheel_scrolls_the_transcript_by_a_wheel_step() {
        let mut app = App::new("m".to_string(), None, "id".to_string());
        assert_eq!(app.scroll_back, 0);

        handle_mouse_scroll(&mut app, mouse(MouseEventKind::ScrollUp));
        assert_eq!(app.scroll_back, MOUSE_SCROLL_STEP);

        handle_mouse_scroll(&mut app, mouse(MouseEventKind::ScrollUp));
        assert_eq!(app.scroll_back, MOUSE_SCROLL_STEP * 2);

        handle_mouse_scroll(&mut app, mouse(MouseEventKind::ScrollDown));
        assert_eq!(app.scroll_back, MOUSE_SCROLL_STEP);
    }

    #[test]
    fn wheel_does_not_scroll_past_the_newest_message() {
        let mut app = App::new("m".to_string(), None, "id".to_string());
        handle_mouse_scroll(&mut app, mouse(MouseEventKind::ScrollDown));
        assert_eq!(app.scroll_back, 0, "can't scroll below the newest content");
    }

    #[test]
    fn other_mouse_events_are_ignored() {
        let mut app = App::new("m".to_string(), None, "id".to_string());
        handle_mouse_scroll(
            &mut app,
            mouse(MouseEventKind::Down(crossterm::event::MouseButton::Left)),
        );
        assert_eq!(app.scroll_back, 0);
    }
}
