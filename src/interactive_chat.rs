use crate::client::{response_label, ChatMessage, ChatResponse, Client};
use crate::store;
use crate::wrap;
use anyhow::Result;
use colored::*;
use rusqlite::Connection;
use rustyline::error::ReadlineError;
use rustyline::{DefaultEditor, ExternalPrinter};
use std::collections::VecDeque;
use std::sync::Arc;
use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinHandle;

/// Typing this prefix before a message cancels whatever request is
/// currently in flight and sends this message in its place, instead of
/// waiting in the queue behind it.
const STEER_PREFIX: &str = "/steer";

enum ParsedInput {
    Empty,
    Exit,
    Steer(String),
    Message(String),
}

fn parse_input(line: &str) -> ParsedInput {
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return ParsedInput::Empty;
    }
    if trimmed.eq_ignore_ascii_case("exit") {
        return ParsedInput::Exit;
    }
    if let Some(rest) = trimmed.strip_prefix(STEER_PREFIX) {
        let rest = rest.trim();
        return if rest.is_empty() {
            ParsedInput::Empty
        } else {
            ParsedInput::Steer(rest.to_string())
        };
    }
    ParsedInput::Message(line.to_string())
}

/// Writes text to the terminal without corrupting whatever the user is
/// currently typing at the input prompt, if possible. Falls back to a
/// plain write when no external printer is available (e.g. output isn't a
/// TTY).
fn out(printer: &mut Option<Box<dyn ExternalPrinter + Send>>, text: String) {
    match printer {
        Some(p) => {
            let _ = p.print(text);
        }
        None => print!("{text}"),
    }
}

/// Fixed context for the session a chat loop is running in, bundled so
/// helper functions don't need a long, repeated parameter list.
struct SessionCtx<'a> {
    conn: &'a Connection,
    session_id: &'a str,
    model: &'a str,
    effort_level: Option<&'a str>,
}

/// Pushes a user message onto history, persists it, and sets the session
/// title from it if this is the first user message.
fn record_user_turn(
    ctx: &SessionCtx,
    messages: &mut Vec<ChatMessage>,
    title_set: &mut bool,
    printer: &mut Option<Box<dyn ExternalPrinter + Send>>,
    text: String,
) {
    let seq = messages.len();
    let message = ChatMessage {
        role: "user".to_string(),
        content: Some(text),
        tool_calls: None,
        tool_call_id: None,
    };
    messages.push(message.clone());
    if let Err(e) = store::append_message(
        ctx.conn,
        ctx.session_id,
        seq,
        &message,
        ctx.model,
        ctx.effort_level,
    ) {
        out(
            printer,
            format!("{} Failed to save message: {}\n", "✗".red(), e),
        );
    }
    if !*title_set {
        let title = store::derive_title(messages);
        if let Err(e) = store::set_session_title(ctx.conn, ctx.session_id, &title) {
            out(
                printer,
                format!("{} Failed to save session title: {}\n", "✗".red(), e),
            );
        }
        *title_set = true;
    }
}

fn spawn_request(
    client: &Arc<Client>,
    model: &str,
    effort_level: Option<String>,
    messages: Vec<ChatMessage>,
) -> JoinHandle<Result<ChatResponse>> {
    let client = Arc::clone(client);
    let model = model.to_string();
    tokio::spawn(async move { client.chat(model, messages, 0.7, None, effort_level).await })
}

/// Runs an interactive chat session where the input prompt stays live the
/// whole time: you're never blocked waiting for a response. A message
/// typed while one is already in flight is queued and sent once it
/// finishes; prefixing a message with `/steer` instead cancels the
/// in-flight request immediately and sends that message in its place.
pub async fn run(
    client: Arc<Client>,
    conn: &Connection,
    session_id: String,
    model: String,
    effort_level: Option<String>,
    mut messages: Vec<ChatMessage>,
    mut title_set: bool,
) -> Result<()> {
    println!(
        "{}\n",
        "Starting chat session (type 'exit' to quit, prefix a message with '/steer' to interrupt and redirect the current response)"
            .blue()
    );

    // Shown alongside every prompt so it's always clear what's currently
    // active, since it can change across a resume (`--model`) or a config
    // edit (effort level) between sessions.
    let prompt = format!(
        "{} {} ",
        format!("[{}]", response_label(&model, &effort_level)).bright_black(),
        "You:".blue()
    );

    // The input prompt runs on its own OS thread since rustyline's
    // readline() blocks synchronously; this lets the async loop below keep
    // accepting typed messages while a request is in flight. `continue_rx`
    // gates each subsequent readline() call so the thread never re-enters
    // raw terminal mode after the main loop has decided to exit.
    let (line_tx, mut line_rx) =
        mpsc::unbounded_channel::<std::result::Result<String, ReadlineError>>();
    let (continue_tx, continue_rx) = std::sync::mpsc::channel::<()>();
    let (printer_tx, printer_ready) = oneshot::channel();

    std::thread::spawn(move || {
        let mut rl = match DefaultEditor::new() {
            Ok(rl) => rl,
            Err(_) => {
                let _ = printer_tx.send(None);
                return;
            }
        };
        let printer: Option<Box<dyn ExternalPrinter + Send>> = rl
            .create_external_printer()
            .ok()
            .map(|p| Box::new(p) as Box<dyn ExternalPrinter + Send>);
        let _ = printer_tx.send(printer);

        loop {
            let result = rl.readline(&prompt);
            let should_stop = matches!(
                result,
                Err(ReadlineError::Eof) | Err(ReadlineError::Interrupted)
            );
            if line_tx.send(result).is_err() || should_stop {
                break;
            }
            // Wait for the main loop's go-ahead before reading the next
            // line, so we never start a new readline() after it has
            // decided to exit.
            if continue_rx.recv().is_err() {
                break;
            }
        }
    });

    let mut printer = printer_ready.await.ok().flatten();

    let ctx = SessionCtx {
        conn,
        session_id: &session_id,
        model: &model,
        effort_level: effort_level.as_deref(),
    };
    let mut queue: VecDeque<String> = VecDeque::new();
    let mut inflight: Option<JoinHandle<Result<ChatResponse>>> = None;

    loop {
        tokio::select! {
            maybe_line = line_rx.recv() => {
                let Some(result) = maybe_line else { break };

                match result {
                    Err(ReadlineError::Interrupted) | Err(ReadlineError::Eof) => {
                        println!("{} Chat session ended", "✓".green());
                        break;
                    }
                    Err(e) => {
                        eprintln!("{} Error: {}", "✗".red(), e);
                        break;
                    }
                    Ok(line) => {
                        match parse_input(&line) {
                            ParsedInput::Empty => {}
                            ParsedInput::Exit => {
                                println!("{} Chat session ended", "✓".green());
                                break;
                            }
                            ParsedInput::Steer(text) => {
                                if let Some(handle) = inflight.take() {
                                    handle.abort();
                                    out(&mut printer, format!("{} steering — redirecting the current response\n", "⤷".yellow()));
                                }
                                record_user_turn(&ctx, &mut messages, &mut title_set, &mut printer, text);
                                inflight = Some(spawn_request(&client, &model, effort_level.clone(), messages.clone()));
                            }
                            ParsedInput::Message(text) => {
                                if inflight.is_some() {
                                    queue.push_back(text);
                                    out(&mut printer, format!("{} message queued ({} pending)\n", "…".bright_black(), queue.len()));
                                } else {
                                    record_user_turn(&ctx, &mut messages, &mut title_set, &mut printer, text);
                                    inflight = Some(spawn_request(&client, &model, effort_level.clone(), messages.clone()));
                                }
                            }
                        }
                    }
                }

                let _ = continue_tx.send(());
            }
            resp = async { inflight.as_mut().unwrap().await }, if inflight.is_some() => {
                inflight = None;

                match resp {
                    Ok(Ok(response)) => {
                        let choice = &response.choices[0];
                        if choice.message.has_visible_content() {
                            let content = choice.message.content.as_deref().unwrap();
                            out(&mut printer, format!(
                                "\n{} {}\n\n",
                                format!("{}:", response_label(&model, &effort_level)).cyan(),
                                wrap::wrap(content)
                            ));
                            let seq = messages.len();
                            let assistant_message = ChatMessage {
                                role: "assistant".to_string(),
                                content: Some(content.to_string()),
                                tool_calls: None,
                                tool_call_id: None,
                            };
                            messages.push(assistant_message.clone());
                            if let Err(e) = store::append_message(ctx.conn, ctx.session_id, seq, &assistant_message, ctx.model, ctx.effort_level) {
                                out(&mut printer, format!("{} Failed to save message: {}\n", "✗".red(), e));
                            }
                        }
                    }
                    Ok(Err(e)) => {
                        out(&mut printer, format!("{} {}\n\n", "✗".red(), e));
                    }
                    Err(e) => {
                        if !e.is_cancelled() {
                            out(&mut printer, format!("{} Request failed: {}\n\n", "✗".red(), e));
                        }
                    }
                }

                if let Some(next) = queue.pop_front() {
                    record_user_turn(&ctx, &mut messages, &mut title_set, &mut printer, next);
                    inflight = Some(spawn_request(&client, &model, effort_level.clone(), messages.clone()));
                }
            }
        }
    }

    if let Some(handle) = inflight.take() {
        handle.abort();
    }

    println!(
        "{} Session saved. Resume with: comms chat --resume {}",
        "✓".green(),
        &session_id[..8]
    );

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_input_detects_exit_case_insensitively() {
        assert!(matches!(parse_input("exit"), ParsedInput::Exit));
        assert!(matches!(parse_input("EXIT"), ParsedInput::Exit));
        assert!(matches!(parse_input("  exit  "), ParsedInput::Exit));
    }

    #[test]
    fn parse_input_treats_blank_lines_as_empty() {
        assert!(matches!(parse_input(""), ParsedInput::Empty));
        assert!(matches!(parse_input("   "), ParsedInput::Empty));
    }

    #[test]
    fn parse_input_extracts_steer_message() {
        match parse_input("/steer actually do this instead") {
            ParsedInput::Steer(text) => assert_eq!(text, "actually do this instead"),
            _ => panic!("expected Steer"),
        }
    }

    #[test]
    fn parse_input_bare_steer_is_empty() {
        assert!(matches!(parse_input("/steer"), ParsedInput::Empty));
        assert!(matches!(parse_input("/steer   "), ParsedInput::Empty));
    }

    #[test]
    fn parse_input_plain_text_is_message() {
        match parse_input("what's the weather like") {
            ParsedInput::Message(text) => assert_eq!(text, "what's the weather like"),
            _ => panic!("expected Message"),
        }
    }
}
