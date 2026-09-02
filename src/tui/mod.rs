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
use crate::conversation::{command_for, Command, Conversation};
use crate::session::{self, ChatSession};
use crate::store::{self, SessionSummary, StoredMessage, KIND_AGENT_CHAT, KIND_CHAT};
use crate::ui::{parse_yes_no, response_label};
use anyhow::Result;
use app::{App, Focus, TranscriptItem};
use crossterm::event::{
    DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture,
    Event as TermEvent, EventStream, KeyCode, KeyEvent, KeyEventKind, KeyModifiers,
    KeyboardEnhancementFlags, MouseEvent, MouseEventKind, PopKeyboardEnhancementFlags,
    PushKeyboardEnhancementFlags,
};
use crossterm::{execute, terminal};
use futures_util::StreamExt;
use picker::{Activation, Picker, SessionRow};
use ratatui::backend::CrosstermBackend;
use ratatui::Terminal;
use std::io::{self, Stdout};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

/// How often the screen redraws while idle, driving the spinner.
const TICK: Duration = Duration::from_millis(100);
/// How often the picker screen re-reads where each session left off. Slower
/// than the animation tick on purpose: it's a database read, and a session's
/// last message doesn't change faster than a turn takes.
const SESSION_REFRESH: Duration = Duration::from_secs(2);

/// Everything needed to start conversations on demand, since with a launch
/// screen the TUI no longer receives a ready-made session.
pub struct Context {
    pub client: Arc<Client>,
    /// Model for new sessions; a resumed one keeps its own.
    pub default_model: String,
    pub effort_level: Option<String>,
    pub max_iterations: Option<usize>,
    pub temperature: Option<f32>,
    pub approval: ApprovalSettings,
    /// The configured default for confining the agent's file writes, taken
    /// as this session's starting value.
    pub sandbox: bool,
    /// The configured default for showing full tool-call detail, taken as
    /// this session's starting value.
    pub verbose: bool,
    /// The configured default for streaming replies, same deal.
    pub stream: bool,
}

enum Screen {
    /// Boxed for the same reason `Chat` is: it dwarfs the other variants,
    /// and every `Screen` would otherwise carry its footprint.
    Launch(Box<Picker>),
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

/// Runs the TUI until the user quits. Always opens on the launch screen —
/// there's no flag to skip straight into a new or resumed session, so this
/// is the one and only way in.
pub async fn run(context: Context) -> Result<()> {
    let mut screen = Screen::Launch(Box::new(launch_picker()?));

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
    // Every session's last message in one query, then matched up here —
    // a query per row would make opening the picker scale with how many
    // sessions have accumulated.
    let mut last = store::last_messages(&conn)?;
    Ok(store::list_sessions(&conn)?
        .into_iter()
        .map(|summary| {
            let mut row = SessionRow::from(summary);
            row.last = last.remove(&row.id);
            row
        })
        .collect())
}

/// Each conversation gets its own database handle, so sessions can be
/// opened and closed over the life of the TUI without threading one
/// connection through every screen.
/// The launch screen, grouped by whether each session belongs to the
/// directory the process is in right now.
fn launch_picker() -> Result<Picker> {
    Ok(Picker::launch(load_sessions()?, current_dir().as_deref()))
}

fn current_dir() -> Option<String> {
    std::env::current_dir()
        .ok()
        .map(|dir| dir.display().to_string())
}

fn open_new(context: &Context, agentic: bool, title: String) -> Result<Chat> {
    let kind = if agentic { KIND_AGENT_CHAT } else { KIND_CHAT };

    let mut session = ChatSession::create(
        store::open_db()?,
        context.default_model.clone(),
        kind,
        context.effort_level.clone(),
        context.max_iterations,
        context.temperature,
        context.approval.clone(),
        context.sandbox,
        context.verbose,
        context.stream,
        std::env::current_dir()
            .ok()
            .map(|dir| dir.display().to_string()),
    )?;
    session.set_title(title)?;
    if agentic {
        session.push_and_persist(ChatMessage {
            role: "system".to_string(),
            content: Some(AGENT_CHAT_SYSTEM_PROMPT.to_string()),
            tool_calls: None,
            tool_call_id: None,
            ..Default::default()
        })?;
    }

    Ok(start_chat(context, session, Vec::new(), agentic))
}

fn open_resumed(context: &Context, summary: &SessionSummary) -> Result<Chat> {
    let (session, history) =
        ChatSession::resume(store::open_db()?, summary, summary.model.clone())?;
    let agentic = summary.kind == KIND_AGENT_CHAT;

    // The session's directory is its sandbox boundary, so opening one moves
    // the process into it. Unlike the CLI there's nowhere to refuse to: the
    // TUI can open any session from its picker, so a directory that's gone
    // is reported in the transcript and the session opens where it is —
    // loudly, because its bound is now the wrong one.
    let entered = session::enter_working_dir(&session)?;
    let mut chat = start_chat(context, session, history, agentic);
    match entered {
        session::EnteredDir::Moved(dir) => chat
            .app
            .transcript
            .push(TranscriptItem::Notice(format!("Working directory: {dir}"))),
        session::EnteredDir::Unchanged => {}
        session::EnteredDir::Missing(dir) => {
            chat.app.transcript.push(TranscriptItem::Error(format!(
                "This session was started in {dir}, which no longer exists — \
                 it is running in the current directory instead, so its sandbox \
                 and relative paths point somewhere else than when it was saved."
            )))
        }
    }
    Ok(chat)
}

/// Why a row couldn't be opened, when the caller can do something about it.
enum OpenFailure {
    /// The session's directory is gone. Offerable: resuming here and
    /// repointing is a real answer, so the picker asks rather than refusing.
    MissingDir(String),
    Other(anyhow::Error),
}

fn open_row(context: &Context, row: &SessionRow) -> std::result::Result<Chat, OpenFailure> {
    let summary = (|| {
        let conn = store::open_db()?;
        store::find_session(&conn, &row.id)?
            .ok_or_else(|| anyhow::anyhow!("Session {} no longer exists", row.short_id()))
    })()
    .map_err(OpenFailure::Other)?;

    if let Some(dir) = summary.working_dir.as_deref() {
        if !std::path::Path::new(dir).is_dir() {
            return Err(OpenFailure::MissingDir(dir.to_string()));
        }
    }
    open_resumed(context, &summary).map_err(OpenFailure::Other)
}

/// Resumes `row` in the current directory, recording it as the session's own.
fn open_row_here(context: &Context, row: &SessionRow) -> Result<Chat> {
    let conn = store::open_db()?;
    let summary = store::find_session(&conn, &row.id)?
        .ok_or_else(|| anyhow::anyhow!("Session {} no longer exists", row.short_id()))?;
    let (mut session, history) = ChatSession::resume(conn, &summary, summary.model.clone())?;
    if let Some(cwd) = std::env::current_dir()
        .ok()
        .map(|d| d.display().to_string())
    {
        session.set_working_dir(cwd)?;
    }
    let agentic = summary.kind == KIND_AGENT_CHAT;
    Ok(start_chat(context, session, history, agentic))
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
    app.sandbox = session.sandbox();
    app.stream = session.stream();
    app.working_dir = session.working_dir().map(str::to_string);
    app.title = session.title().to_string();
    seed_transcript(&mut app, &history);

    let conversation = Conversation::spawn(
        Arc::clone(&context.client),
        session,
        context.max_iterations,
        context.temperature,
        context.effort_level.clone(),
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
                // Ahead of the reply it led to, matching the live ordering.
                // Pushed even when the reply itself had no visible text —
                // a turn that only called a tool still thought first.
                if let Some(thought) = message.thinking_text() {
                    app.transcript.push(TranscriptItem::Thinking(thought));
                }
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

/// Whether the terminal accepted the keyboard enhancement flags, so teardown
/// knows whether it has a push to undo. A static because both [`leave`] and
/// the panic hook have to see it and neither is handed any state.
static ENHANCED_KEYS: AtomicBool = AtomicBool::new(false);

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

    // Shift-Enter can't be seen at all under the legacy input protocol: a
    // bare Enter arrives as a carriage return, which has nowhere to carry a
    // modifier, so Shift-Enter is byte-identical to Enter. (Alt-Enter works
    // because Alt has always been encoded as an escape prefix, which *is*
    // distinguishable.) The kitty keyboard protocol reports the modifier
    // properly, so ask for its disambiguation flag wherever the terminal
    // advertises support and leave everything alone where it doesn't —
    // Alt-Enter still covers those.
    let enhanced = terminal::supports_keyboard_enhancement().unwrap_or(false);
    if enhanced {
        execute!(
            stdout,
            PushKeyboardEnhancementFlags(KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES)
        )?;
    }
    ENHANCED_KEYS.store(enhanced, Ordering::Relaxed);

    // A panic while in raw mode would otherwise leave the terminal unusable
    // with no echo and no cursor, so restore first, then panic normally.
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let _ = terminal::disable_raw_mode();
        if ENHANCED_KEYS.load(Ordering::Relaxed) {
            let _ = execute!(io::stdout(), PopKeyboardEnhancementFlags);
        }
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
    // Popped before the rest so the terminal is back on its own protocol
    // even if a later command fails.
    if ENHANCED_KEYS.swap(false, Ordering::Relaxed) {
        execute!(terminal.backend_mut(), PopKeyboardEnhancementFlags)?;
    }
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
        Screen::Launch(p) => picker::draw(
            frame,
            p,
            "comms",
            "↑/↓ move · Enter open · r rename · d delete · q quit",
            tick,
        ),
        Screen::NameSession { input } => picker::draw_naming(frame, input),
        Screen::Chat(chat) => render::draw(frame, &chat.app, tick),
    })?;
    Ok(())
}

async fn event_loop(terminal: &mut Tui, context: &Context, screen: &mut Screen) -> Result<()> {
    let mut keys = EventStream::new();
    let mut ticker = tokio::time::interval(TICK);
    let mut last_refresh = std::time::Instant::now();
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
                // The picker animates too, but only while it has something
                // to animate: a list of idle sessions shouldn't repaint ten
                // times a second for nothing.
                if matches!(screen, Screen::Launch(p) if p.has_working_session()) {
                    tick = tick.wrapping_add(1);
                    dirty = true;
                }
                // Sessions move on while the picker is open — including ones
                // running in another terminal — so it re-reads rather than
                // showing whatever was true when it was opened.
                if let Screen::Launch(p) = screen {
                    if last_refresh.elapsed() >= SESSION_REFRESH {
                        last_refresh = std::time::Instant::now();
                        if p.refresh(load_sessions()?, current_dir().as_deref()) {
                            dirty = true;
                        }
                    }
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
        Screen::Launch(_) => handle_picker_key(context, screen, key).await,
        Screen::NameSession { .. } => handle_naming_key(context, screen, key),
        Screen::Chat(chat) => {
            if handle_chat_key(&mut chat.app, &chat.conversation, key) {
                // Leaving the conversation: stop its worker before the
                // screen is replaced, so its final writes land.
                let Screen::Chat(chat) =
                    std::mem::replace(screen, Screen::Launch(Box::new(launch_picker()?)))
                else {
                    unreachable!("just matched Chat")
                };
                chat.conversation.shutdown().await;
                // The list was loaded before the shutdown flushed this
                // session, so refresh it to show the up-to-date title.
                *screen = Screen::Launch(Box::new(launch_picker()?));
            }
            Ok(false)
        }
    }
}

async fn handle_picker_key(context: &Context, screen: &mut Screen, key: KeyEvent) -> Result<bool> {
    let Screen::Launch(p) = screen else {
        unreachable!("picker screen only")
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
            KeyCode::Char(c) if is_typed_char(&key) => p.rename_insert_char(c),
            _ => {}
        }
        return Ok(false);
    }

    // A pending repoint swallows everything until it's answered.
    if p.confirming_repoint.is_some() {
        let action = match key.code {
            KeyCode::Char('y') | KeyCode::Char('Y') => p.resolve_repoint(true),
            _ => p.resolve_repoint(false),
        };
        if let Some(Activation::Repoint(row)) = action {
            match open_row_here(context, &row) {
                Ok(chat) => *screen = Screen::Chat(Box::new(chat)),
                Err(e) => {
                    if let Screen::Launch(p) = screen {
                        p.notice = Some(e.to_string());
                    }
                }
            }
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
        // Rename and delete used to live on the separate browser; with one
        // list they belong here.
        KeyCode::Char('r') => p.begin_rename(),
        KeyCode::Char('d') => p.begin_delete(),
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
                Activation::Resume(row) => match open_row(context, &row) {
                    Ok(chat) => *screen = Screen::Chat(Box::new(chat)),
                    // Neither failure is fatal: a session that won't open is
                    // one row in a list of them, and taking the whole TUI
                    // down would lose access to every other session too.
                    Err(OpenFailure::MissingDir(dir)) => {
                        if let Screen::Launch(p) = screen {
                            p.begin_repoint(row, dir);
                        }
                    }
                    Err(OpenFailure::Other(e)) => {
                        if let Screen::Launch(p) = screen {
                            p.notice = Some(e.to_string());
                        }
                    }
                },
                // Delete and repoint are resolved by their confirmation
                // flows, not here.
                Activation::Delete(_) | Activation::Repoint(_) => {}
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
        KeyCode::Esc => *screen = Screen::Launch(Box::new(launch_picker()?)),
        KeyCode::Enter => {
            // A blank title does nothing rather than starting an untitled
            // session: naming it is the deliberate act that creating one
            // should take, and it's what makes the session worth keeping
            // whether or not anything is ever said in it.
            let title = input.trim();
            if !title.is_empty() {
                let title = title.to_string();
                *screen = Screen::Chat(Box::new(open_new(context, false, title)?));
            }
        }
        KeyCode::Backspace => {
            input.pop();
        }
        KeyCode::Char(c) if is_typed_char(&key) => input.push(c),
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

/// Whether a `Char` keypress is someone typing, rather than a chord that
/// happens to carry a letter. Without this every unhandled Ctrl-combination
/// types its bare letter — Ctrl-V, the paste chord users reach for first,
/// put a stray `v` in the input box instead of doing nothing.
///
/// Only CONTROL disqualifies a keypress. SHIFT is how capitals arrive, and
/// ALT composes real characters on some layouts and terminals, so neither
/// can be treated as "not typing".
fn is_typed_char(key: &KeyEvent) -> bool {
    !key.modifiers.contains(KeyModifiers::CONTROL)
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
            KeyCode::Char(c) if is_typed_char(&key) => app.insert_char(c),
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
        // Alt-Enter always inserts a newline. Shift-Enter does too wherever
        // the terminal can report the modifier — see `enter`, which asks
        // for that reporting when the terminal supports it.
        KeyCode::Enter
            if key
                .modifiers
                .intersects(KeyModifiers::ALT | KeyModifiers::SHIFT) =>
        {
            app.insert_char('\n')
        }
        KeyCode::Enter => {
            if let Some(text) = app.take_input() {
                let submission = app::classify(&text);
                match command_for(&submission) {
                    // Everything that changes session state is the worker's
                    // to apply; it replies with the event that updates the
                    // view, so the two can't disagree.
                    Some(command) => conversation.send(command),
                    // The rest are read-only, answered from state this side
                    // already holds. `command_for` is the exhaustive match,
                    // so a new submission variant is classified there first.
                    None => {
                        match submission {
                            // Round-tripped rather than read locally, so the
                            // answer reflects what the session actually holds.
                            app::Submission::ShowModel => {
                                conversation.send(Command::SetModel(app.model.clone()))
                            }
                            app::Submission::ShowStatus => {
                                let approval = app.approval.clone();
                                let rows =
                                    crate::ui::session_settings_rows(&crate::ui::SessionSettings {
                                        id: &app.session_id,
                                        title: &app.title,
                                        model: &app.model,
                                        agentic: app.agentic,
                                        effort_level: app.effort_level.as_deref(),
                                        temperature: app.temperature,
                                        max_iterations: app.max_iterations,
                                        verbose: app.verbose,
                                        sandbox: app.sandbox,
                                        stream: app.stream,
                                        working_dir: app.working_dir.as_deref(),
                                        approval: &approval,
                                    });
                                app.transcript.push(TranscriptItem::SessionStatus(rows));
                            }
                            app::Submission::ShowVerbose => {
                                app.transcript.push(TranscriptItem::Notice(
                                    crate::ui::verbose_notice(app.verbose, false),
                                ));
                            }
                            app::Submission::ShowTemperature => {
                                app.transcript.push(TranscriptItem::Notice(
                                    crate::ui::temperature_notice(app.temperature, false),
                                ));
                            }
                            app::Submission::ShowTitle => {
                                app.transcript.push(TranscriptItem::Notice(
                                    crate::ui::title_notice(&app.title, false),
                                ));
                            }
                            app::Submission::ShowStream => {
                                app.transcript.push(TranscriptItem::Notice(
                                    crate::ui::stream_notice(app.stream, false),
                                ));
                            }
                            app::Submission::ShowSandbox => {
                                app.transcript.push(TranscriptItem::Notice(
                                    crate::ui::sandbox_notice(app.sandbox, false),
                                ));
                            }
                            app::Submission::ShowApproval => {
                                app.transcript.push(TranscriptItem::ApprovalStatus {
                                    approval: app.approval.clone(),
                                    changed: false,
                                });
                            }
                            app::Submission::UnknownCommand(message) => {
                                app.transcript.push(TranscriptItem::Error(message));
                            }
                            // Listed rather than caught by `_`, so adding
                            // a submission has to be considered on this side
                            // too — a catch-all here would silently ignore a
                            // new read-only one.
                            app::Submission::Message(_)
                            | app::Submission::SetModel(_)
                            | app::Submission::SetAgentic(_)
                            | app::Submission::SetEffort(_)
                            | app::Submission::ResetEffort
                            | app::Submission::SetVerbose(_)
                            | app::Submission::SetStream(_)
                            | app::Submission::SetTitle(_)
                            | app::Submission::SetSandbox(_)
                            | app::Submission::SetMaxIterations(_)
                            | app::Submission::ResetMaxIterations
                            | app::Submission::SetTemperature(_)
                            | app::Submission::ResetTemperature
                            | app::Submission::SetApproval { .. } => {
                                unreachable!("command_for routes these to the worker")
                            }
                        }
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
        KeyCode::Char(c) if is_typed_char(&key) => app.insert_char(c),
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

    fn key(code: KeyCode, modifiers: KeyModifiers) -> KeyEvent {
        KeyEvent::new(code, modifiers)
    }

    fn test_context() -> Context {
        Context {
            client: Arc::new(Client::for_test(crate::config::Config::default())),
            default_model: "m".to_string(),
            effort_level: None,
            max_iterations: Some(20),
            temperature: None,
            approval: ApprovalSettings::default(),
            sandbox: true,
            verbose: false,
            stream: true,
        }
    }

    #[test]
    fn a_blank_title_does_not_start_a_session() {
        // Naming it is the deliberate act of creating one, so Enter on an
        // empty name has nothing to do — it must not fall through to
        // starting an untitled session.
        //
        // Only the blank path is tested here: every other key on this screen
        // reaches the database (Esc rebuilds the picker, a real title opens a
        // session), and a unit test has no business reading the user's own.
        let context = test_context();
        let mut screen = Screen::NameSession {
            input: "   ".to_string(),
        };

        handle_naming_key(&context, &mut screen, KeyEvent::from(KeyCode::Enter)).unwrap();

        assert!(
            matches!(screen, Screen::NameSession { .. }),
            "a blank title should leave you on the naming screen"
        );
    }

    #[test]
    fn a_control_chord_does_not_type_its_letter() {
        // Ctrl-V is the paste chord people reach for first. Most terminals
        // don't treat it as paste, so it arrives here as an ordinary
        // keypress — and used to leave a stray `v` in the input box.
        assert!(!is_typed_char(&key(
            KeyCode::Char('v'),
            KeyModifiers::CONTROL
        )));
    }

    #[test]
    fn ordinary_typing_still_types() {
        assert!(is_typed_char(&key(
            KeyCode::Char('v'),
            KeyModifiers::empty()
        )));
        // Capitals arrive carrying SHIFT, and ALT composes real characters
        // on some layouts — neither means "not typing".
        assert!(is_typed_char(&key(KeyCode::Char('V'), KeyModifiers::SHIFT)));
        assert!(is_typed_char(&key(KeyCode::Char('e'), KeyModifiers::ALT)));
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
