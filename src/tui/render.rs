//! Drawing the TUI. Pure presentation over [`App`] — no state changes here.

use super::app::{App, Focus, ToolStatus, TranscriptItem};
use crate::ui::{json_fields, summarize, tool_call_fields, ApprovalRequest};
use ratatui::prelude::*;
use ratatui::widgets::{Block, Borders, Paragraph, Wrap};

/// Spinner frames, reused from the CLI's so both front ends feel the same.
const FRAMES: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

/// Most rows the message box will grow to before it scrolls internally,
/// so a long paste can't squeeze the conversation off the screen.
const MAX_INPUT_ROWS: u16 = 10;

pub fn draw(frame: &mut Frame, app: &App, tick: usize) {
    // The message box grows with what's in it instead of staying at one
    // row — normally what's been typed, or, while a tool call is waiting on
    // a decision, the approval prompt that takes over the same box instead
    // of floating a separate modal over the conversation.
    let input_width = frame.area().width.saturating_sub(2);
    let content_rows = match &app.pending_approval {
        // The typed-answer line plus the blank line under it, ahead of the
        // tool/argument detail — see `draw_approval`.
        Some(request) => approval_lines(request).len() as u16 + 2,
        None => input_lines(&app.input, input_width).len() as u16,
    };
    let input_rows = content_rows
        .clamp(1, MAX_INPUT_ROWS)
        // Never take so much that the conversation has nowhere to go.
        .min(frame.area().height.saturating_sub(5).max(1));

    let areas = Layout::vertical([
        Constraint::Min(1),                 // transcript
        Constraint::Length(input_rows + 2), // input, plus its borders
        Constraint::Length(1),              // status
    ])
    .split(frame.area());

    draw_transcript(frame, areas[0], app);
    draw_input(frame, areas[1], app);
    draw_status(frame, areas[2], app, tick);
}

fn draw_transcript(frame: &mut Frame, area: Rect, app: &App) {
    let mut lines: Vec<Line> = Vec::new();

    for item in &app.transcript {
        match item {
            TranscriptItem::User(text) => {
                push_block(
                    &mut lines,
                    Span::styled("You  ", Style::new().blue().bold()),
                    text,
                    None,
                );
                lines.push(Line::raw(""));
            }
            TranscriptItem::Assistant {
                text,
                streaming,
                label,
            } => {
                // A reply with no recorded model is shown as plainly
                // "assistant", dimmed, rather than inheriting a label it
                // can't be known to deserve.
                let prefix = match label {
                    Some(label) => Span::styled(format!("{label}  "), Style::new().cyan().bold()),
                    None => Span::styled("assistant  ", Style::new().dark_gray().bold()),
                };
                let cursor = streaming.then(|| Span::styled("▌", Style::new().cyan()));
                if *streaming {
                    // Mid-stream the text is usually mid-construct — an
                    // unclosed fence or a half-written list — so render it
                    // plainly and let the finished message reformat once.
                    push_block(&mut lines, prefix, text, cursor);
                } else {
                    push_rendered(&mut lines, prefix, markdown_lines(text), cursor);
                }
                lines.push(Line::raw(""));
            }
            TranscriptItem::ToolCall {
                name,
                arguments,
                status,
            } => {
                let (marker, style) = match status {
                    ToolStatus::AwaitingApproval => ("?", Style::new().yellow()),
                    ToolStatus::Running => ("▸", Style::new().yellow()),
                    ToolStatus::Denied => ("✗", Style::new().red()),
                    ToolStatus::Done { .. } => ("✓", Style::new().green()),
                };
                let mut header = vec![
                    Span::styled(format!("  {marker} "), style),
                    Span::styled(name.clone(), Style::new().bold()),
                ];
                // The file or command a call is acting on identifies it well
                // enough to show even without -v; the rest of its arguments
                // (and its result) are the detail that gates behind verbose.
                if let Some(detail) = crate::ui::primary_argument(arguments) {
                    header.push(Span::styled(
                        format!("  {}", summarize(&detail, 60)),
                        Style::new().dark_gray(),
                    ));
                }
                lines.push(Line::from(header));
                if app.verbose {
                    for (key, shown) in tool_call_fields(name, arguments) {
                        lines.push(Line::from(vec![
                            Span::styled(format!("     {key}  "), Style::new().dark_gray()),
                            Span::raw(shown),
                        ]));
                    }
                    if let ToolStatus::Done { result } = status {
                        for (key, shown) in json_fields(result) {
                            lines.push(Line::from(vec![
                                Span::styled(format!("     {key}  "), Style::new().dark_gray()),
                                Span::raw(shown),
                            ]));
                        }
                    }
                }
                lines.push(Line::raw(""));
            }
            TranscriptItem::Error(message) => {
                lines.push(Line::from(vec![
                    Span::styled("  ✗ ", Style::new().red().bold()),
                    Span::styled(message.clone(), Style::new().red()),
                ]));
                lines.push(Line::raw(""));
            }
            TranscriptItem::Notice(message) => {
                lines.push(Line::from(Span::styled(
                    format!("  — {message}"),
                    Style::new().dark_gray().italic(),
                )));
                lines.push(Line::raw(""));
            }
        }
    }

    // Every block appends a trailing blank; drop it so the newest message
    // sits flush against the bottom rather than floating above a gap.
    while matches!(lines.last(), Some(line) if line.spans.iter().all(|s| s.content.is_empty())) {
        lines.pop();
    }

    let inner_width = area.width.saturating_sub(2);
    let visible = area.height.saturating_sub(2);

    // Measured with ratatui's own wrapper rather than estimated: any
    // disagreement between the estimate and the real layout shows up as the
    // view scrolling to the wrong place, which on a long transcript means
    // the newest messages land outside the pane entirely. The clone is only
    // for measuring; the rendered paragraph is built below.
    let measure = |lines: Vec<Line<'static>>| -> (u16, Vec<Line<'static>>) {
        let height = Paragraph::new(Text::from(lines.clone()))
            .wrap(Wrap { trim: false })
            .line_count(inner_width) as u16;
        (height, lines)
    };
    let (mut total, mut lines) = measure(lines);

    // Grow the conversation up from the input box instead of down from the
    // title, the way a chat reads: until there's enough to fill the pane,
    // pad above so the newest message stays at the bottom.
    if total < visible {
        let mut padded = vec![Line::raw(""); (visible - total) as usize];
        padded.extend(lines);
        lines = padded;
        total = visible;
    }

    let text = Text::from(lines);
    // Wrapping is recomputed every frame, which is what lets streamed text
    // re-flow as it grows — the thing a scrolling terminal can't do.
    let paragraph = Paragraph::new(text).wrap(Wrap { trim: false });

    let max_offset = total.saturating_sub(visible);
    // scroll_back counts up from the bottom; 0 pins to the newest content.
    let offset = max_offset.saturating_sub(app.scroll_back.min(max_offset));

    frame.render_widget(
        paragraph
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .title(Span::styled(" comms ", Style::new().bold()))
                    .title_bottom(scroll_hint(app, max_offset)),
            )
            .scroll((offset, 0)),
        area,
    );
}

fn scroll_hint(app: &App, max_offset: u16) -> Line<'static> {
    if !app.is_pinned_to_bottom() && max_offset > 0 {
        Line::from(Span::styled(
            " scrolled — End to follow ",
            Style::new().yellow(),
        ))
        .right_aligned()
    } else {
        Line::raw("")
    }
}

fn draw_input(frame: &mut Frame, area: Rect, app: &App) {
    if let Some(request) = &app.pending_approval {
        draw_approval(frame, area, app, request);
        return;
    }

    let (title, style) = if app.busy {
        (" message (queues while busy) ", Style::new().dark_gray())
    } else {
        (" message ", Style::new().dark_gray())
    };

    let width = area.width.saturating_sub(2).max(1);
    let rows = input_lines(&app.input, width);
    let (cursor_row, cursor_col) = input_cursor(&app.input, app.cursor, width);

    // Once the text is taller than the box, follow the cursor rather than
    // pinning to the top, so what you're typing stays on screen.
    let visible = area.height.saturating_sub(2).max(1);
    let scroll = (cursor_row + 1).saturating_sub(visible);

    // Wrapped by hand rather than by `Wrap`, so the cursor position below is
    // computed against exactly the rows being drawn.
    let paragraph = Paragraph::new(Text::from(
        rows.into_iter().map(Line::from).collect::<Vec<_>>(),
    ))
    .block(
        Block::default()
            .borders(Borders::ALL)
            .border_style(style)
            .title(Span::styled(title, style)),
    )
    .scroll((scroll, 0));
    frame.render_widget(paragraph, area);

    if app.focus == Focus::Input {
        frame.set_cursor_position((
            area.x + 1 + cursor_col,
            area.y + 1 + cursor_row.saturating_sub(scroll),
        ));
    }
}

/// Splits the input into the rows it occupies: on explicit newlines, and
/// hard-wrapped at `width`. Hard rather than word wrapping so that a cursor
/// position can be computed exactly against what's drawn.
fn input_lines(input: &str, width: u16) -> Vec<String> {
    let width = width.max(1) as usize;
    let mut rows = Vec::new();
    for segment in input.split('\n') {
        let chars: Vec<char> = segment.chars().collect();
        if chars.is_empty() {
            rows.push(String::new());
            continue;
        }
        for chunk in chars.chunks(width) {
            rows.push(chunk.iter().collect());
        }
        // A segment filling the last row exactly puts the caret on the next.
        if chars.len() % width == 0 {
            rows.push(String::new());
        }
    }
    rows
}

/// Where the caret sits, in the same rows [`input_lines`] produces.
fn input_cursor(input: &str, cursor: usize, width: u16) -> (u16, u16) {
    let width = width.max(1) as usize;
    let mut row = 0usize;
    let mut col = 0usize;
    for ch in input[..cursor.min(input.len())].chars() {
        if ch == '\n' {
            row += 1;
            col = 0;
        } else {
            col += 1;
            if col == width {
                row += 1;
                col = 0;
            }
        }
    }
    (row as u16, col as u16)
}

fn draw_status(frame: &mut Frame, area: Rect, app: &App, tick: usize) {
    let mut spans = Vec::new();

    if app.busy {
        spans.push(Span::styled(
            format!(" {} working ", FRAMES[tick % FRAMES.len()]),
            Style::new().yellow(),
        ));
    } else {
        spans.push(Span::styled(" ready ", Style::new().green()));
    }

    if app.queued > 0 {
        spans.push(Span::styled(
            format!("· {} queued ", app.queued),
            Style::new().dark_gray(),
        ));
    }

    spans.push(Span::styled(
        format!("· {} ", if app.agentic { "agent" } else { "chat" }),
        if app.agentic {
            Style::new().yellow()
        } else {
            Style::new().cyan()
        },
    ));
    spans.push(Span::styled(
        format!("· {} ", app.label()),
        Style::new().dark_gray(),
    ));
    spans.push(Span::styled(
        format!("· {} ", app.session_id),
        Style::new().dark_gray(),
    ));
    spans.push(Span::styled(
        "· Enter send · Esc cancel · PgUp/PgDn scroll · Ctrl-B back · Ctrl-C quit",
        Style::new().dark_gray(),
    ));

    frame.render_widget(Paragraph::new(Line::from(spans)), area);
}

/// Takes over the input box — rather than floating a modal over the
/// conversation — since typing is already redirected to y/n/esc during
/// approval and the box is otherwise sitting idle.
/// Answered by typing rather than a raw keypress, so the box works like the
/// ordinary input it's replacing: type, edit, Enter to submit. The tool's
/// detail comes first and the prompt sits last, right above where you'd
/// normally be typing a message — its row is computed from the detail's
/// line count, which only stays exact if a field can't reflow onto a
/// second row and silently push the prompt below the box's bottom edge.
/// That's why this deliberately doesn't `.wrap(..)`: a field long enough to
/// overflow the box just gets clipped at the edge instead, which loses
/// characters but never the interactive prompt beneath it.
fn draw_approval(frame: &mut Frame, area: Rect, app: &App, request: &ApprovalRequest) {
    let category = match request.category {
        "read" => "Read from disk",
        "write" => "Write to disk",
        "terminal" => "Terminal command",
        _ => "Unknown action",
    };
    let title = format!(" {category} — type y or yes to allow, Enter to deny, Esc to cancel ");

    let detail = approval_lines(request);
    let prompt_row = detail.len() as u16 + 1;

    let mut lines = detail;
    lines.push(Line::raw(""));
    let prompt = "Allow?  ";
    lines.push(Line::from(vec![
        Span::styled(prompt, Style::new().yellow().bold()),
        Span::raw(app.input.clone()),
    ]));

    let paragraph = Paragraph::new(Text::from(lines)).block(
        Block::default()
            .borders(Borders::ALL)
            .border_style(Style::new().yellow())
            .title(Span::styled(title, Style::new().yellow().bold())),
    );
    frame.render_widget(paragraph, area);

    let col = prompt.chars().count() as u16 + app.input[..app.cursor].chars().count() as u16;
    frame.set_cursor_position((area.x + 1 + col, area.y + 1 + prompt_row));
}

/// The tool and its arguments, field by field — matching how the CLI
/// presents an approval prompt and how a verbose tool-call notice presents
/// its arguments.
fn approval_lines(request: &ApprovalRequest) -> Vec<Line<'static>> {
    let mut lines = vec![Line::from(vec![
        Span::styled("tool  ", Style::new().dark_gray()),
        Span::styled(request.tool_name.clone(), Style::new().bold()),
    ])];
    for (key, shown) in json_fields(&request.arguments) {
        lines.push(Line::from(vec![
            Span::styled(format!("{key}  "), Style::new().dark_gray()),
            Span::raw(shown),
        ]));
    }
    lines
}

/// Renders markdown to styled rows.
///
/// Applied only to assistant replies: a user's own `*asterisks*` should
/// appear as typed, and tool lines are already formatted. The result is
/// re-homed into owned spans because the transcript outlives the borrow of
/// the message it came from.
fn markdown_lines(text: &str) -> Vec<Line<'static>> {
    let mut inside_code = false;

    tui_markdown::from_str(text)
        .lines
        .into_iter()
        .filter_map(|line| {
            let plain: String = line.spans.iter().map(|s| s.content.as_ref()).collect();

            // The fences come through as literal rows of backticks. The code
            // between them is already syntax-coloured, so they'd just be
            // noise: label the opening one with its language and drop the
            // close.
            if plain.trim_end().starts_with("```") {
                inside_code = !inside_code;
                if !inside_code {
                    return None;
                }
                let language = plain.trim().trim_start_matches('`').trim();
                let tag = if language.is_empty() {
                    "code".to_string()
                } else {
                    language.to_string()
                };
                return Some(Line::from(Span::styled(
                    tag,
                    Style::new().dark_gray().italic(),
                )));
            }

            let spans: Vec<Span<'static>> = line
                .spans
                .into_iter()
                .map(|span| Span::styled(span.content.into_owned(), span.style))
                .collect();
            Some(Line::from(spans))
        })
        .collect()
}

/// Pushes already-rendered rows under a speaker label, keeping the label on
/// the first row and any trailing marker on the last.
fn push_rendered(
    lines: &mut Vec<Line<'static>>,
    prefix: Span<'static>,
    mut rendered: Vec<Line<'static>>,
    trailing: Option<Span<'static>>,
) {
    if rendered.is_empty() {
        rendered.push(Line::raw(""));
    }
    let last = rendered.len() - 1;
    for (index, mut line) in rendered.into_iter().enumerate() {
        if index == 0 {
            line.spans.insert(0, prefix.clone());
        }
        if index == last {
            if let Some(trailing) = trailing.clone() {
                line.spans.push(trailing);
            }
        }
        lines.push(line);
    }
}

/// Pushes one speaker's text, split into a `Line` per newline.
///
/// A ratatui `Line` is a single row: it doesn't break on an embedded `\n`,
/// so putting a whole multi-paragraph reply in one Line both renders it as
/// a run-together blob and makes it impossible to measure — the height
/// estimate would count the paragraphs that the render then doesn't make.
/// `prefix` labels the first row; `trailing` (the streaming cursor) goes on
/// the last.
fn push_block(
    lines: &mut Vec<Line<'static>>,
    prefix: Span<'static>,
    text: &str,
    trailing: Option<Span<'static>>,
) {
    let segments: Vec<&str> = text.split('\n').collect();
    let last = segments.len() - 1;
    for (index, segment) in segments.into_iter().enumerate() {
        let mut spans = Vec::new();
        if index == 0 {
            spans.push(prefix.clone());
        }
        spans.push(Span::raw(segment.to_string()));
        if index == last {
            if let Some(trailing) = trailing.clone() {
                spans.push(trailing);
            }
        }
        lines.push(Line::from(spans));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::app::App;
    use crate::ui::ApprovalRequest;
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    /// Renders to an off-screen buffer and returns it as text, so layout can
    /// be asserted (and panics caught) without a real terminal.
    fn render_to_string(app: &App, width: u16, height: u16) -> String {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal.draw(|frame| draw(frame, app, 0)).unwrap();
        let buffer = terminal.backend().buffer().clone();
        (0..buffer.area.height)
            .map(|y| {
                (0..buffer.area.width)
                    .map(|x| buffer[(x, y)].symbol().to_string())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn sample_app() -> App {
        let mut app = App::new("test-model".to_string(), None, "abcd1234".to_string());
        app.transcript.push(TranscriptItem::User("hello".into()));
        app.transcript.push(TranscriptItem::Assistant {
            text: "hi there".into(),
            streaming: false,
            label: Some("test-model".into()),
        });
        app
    }

    #[test]
    fn renders_conversation_and_status() {
        let out = render_to_string(&sample_app(), 60, 20);
        assert!(out.contains("You"), "{out}");
        assert!(out.contains("hello"), "{out}");
        assert!(out.contains("hi there"), "{out}");
        assert!(out.contains("test-model"), "{out}");
        assert!(out.contains("ready"), "{out}");
    }

    #[test]
    fn a_short_conversation_sits_at_the_bottom_of_the_pane() {
        let out = render_to_string(&sample_app(), 60, 20);
        let rows: Vec<&str> = out.lines().collect();
        // The transcript pane is everything above the 3-row input box and
        // 1-row status bar; its last content row is just inside the border.
        let last_content = rows.len() - 1 - 3 - 1 - 1;
        assert!(
            rows[last_content].contains("hi there"),
            "newest message should be flush with the bottom of the pane, got:\n{out}"
        );
        // ...and the space is above it, not below.
        assert!(
            rows[2].trim_matches(|c| c == '│' || c == ' ').is_empty(),
            "expected blank space above the conversation, got:\n{out}"
        );
    }

    #[test]
    fn newest_stays_visible_even_with_unbreakable_text() {
        // Long unbroken tokens (paths, URLs, base64) wrap differently than
        // ordinary prose. If the height estimate disagrees with how ratatui
        // actually lays them out, the view scrolls to the wrong place and the
        // newest message falls below the fold.
        let mut app = App::new("m".to_string(), None, "id".to_string());
        let blob = format!("/a/very/long/unbroken/path/{}", "x".repeat(200));
        for i in 0..12 {
            app.transcript.push(TranscriptItem::Assistant {
                text: format!("msg {i} {blob}"),
                streaming: false,
                label: Some("m".into()),
            });
        }
        app.transcript
            .push(TranscriptItem::User("LASTMESSAGE".to_string()));

        let out = render_to_string(&app, 60, 14);
        assert!(
            out.contains("LASTMESSAGE"),
            "newest message scrolled out of view:\n{out}"
        );
    }

    #[test]
    fn a_long_conversation_still_shows_the_newest_at_the_bottom() {
        let mut app = App::new("m".to_string(), None, "id".to_string());
        for i in 0..40 {
            app.transcript
                .push(TranscriptItem::User(format!("message {i}")));
        }
        let out = render_to_string(&app, 60, 12);
        assert!(out.contains("message 39"), "{out}");
        // The oldest has scrolled off the top.
        assert!(!out.contains("message 0 "), "{out}");
    }

    #[test]
    fn streaming_block_shows_a_cursor() {
        let mut app = App::new("m".to_string(), None, "id".to_string());
        app.transcript.push(TranscriptItem::Assistant {
            text: "partial".into(),
            streaming: true,
            label: Some("m".into()),
        });
        let out = render_to_string(&app, 40, 12);
        assert!(out.contains('▌'), "{out}");
    }

    #[test]
    fn busy_state_shows_spinner_and_queue_depth() {
        let mut app = sample_app();
        app.busy = true;
        app.queued = 2;
        let out = render_to_string(&app, 80, 15);
        assert!(out.contains("working"), "{out}");
        assert!(out.contains("2 queued"), "{out}");
    }

    #[test]
    fn approval_takes_over_the_input_box_instead_of_a_modal() {
        let mut app = sample_app();
        app.pending_approval = Some(ApprovalRequest {
            tool_name: "write_file".into(),
            category: "write",
            arguments: r#"{"filepath":"/tmp/a.txt"}"#.into(),
        });
        let out = render_to_string(&app, 70, 20);
        assert!(out.contains("Write to disk"), "{out}");
        assert!(out.contains("write_file"), "{out}");
        assert!(out.contains("/tmp/a.txt"), "{out}");
        assert!(out.contains("allow"), "{out}");
        // No separate floating box — the same message box that would
        // otherwise show "message" now shows the approval prompt.
        assert!(!out.contains("message"), "{out}");
    }

    #[test]
    fn approval_prompt_shows_what_was_typed_into_it() {
        let mut app = sample_app();
        app.pending_approval = Some(ApprovalRequest {
            tool_name: "write_file".into(),
            category: "write",
            arguments: "{}".into(),
        });
        app.focus = Focus::Approval;
        for c in "yes".chars() {
            app.insert_char(c);
        }
        let out = render_to_string(&app, 70, 20);
        assert!(out.contains("yes"), "{out}");
        assert!(out.contains("Enter to deny"), "{out}");
    }

    #[test]
    fn approval_box_puts_the_tool_detail_above_the_capitalized_prompt() {
        let mut app = sample_app();
        app.pending_approval = Some(ApprovalRequest {
            tool_name: "write_file".into(),
            category: "write",
            arguments: r#"{"filepath":"/tmp/a.txt"}"#.into(),
        });
        let out = render_to_string(&app, 70, 20);
        assert!(out.contains("Allow?"), "{out}");
        let tool_at = out.find("write_file").expect("tool name shown");
        let prompt_at = out.find("Allow?").expect("prompt shown");
        assert!(tool_at < prompt_at, "{out}");
    }

    #[test]
    fn approval_prompt_stays_visible_when_a_field_is_too_long_to_fit_one_row() {
        // A `content` value long enough that, with wrapping, it would spill
        // onto a second row the box's height wasn't sized for — pushing the
        // "Allow?" prompt past the bottom edge where it silently disappears.
        let mut app = sample_app();
        app.pending_approval = Some(ApprovalRequest {
            tool_name: "write_file".into(),
            category: "write",
            arguments: format!(
                r#"{{"filepath":"/tmp/a.txt","content":"{}"}}"#,
                "x".repeat(90)
            ),
        });
        let out = render_to_string(&app, 80, 20);
        assert!(out.contains("Allow?"), "{out}");
    }

    #[test]
    fn tool_calls_render_with_status() {
        let mut app = App::new("m".to_string(), None, "id".to_string());
        app.transcript.push(TranscriptItem::ToolCall {
            name: "read_file".into(),
            arguments: r#"{"filepath":"a.rs"}"#.into(),
            status: ToolStatus::Done {
                result: r#"{"success":true}"#.into(),
            },
        });
        let out = render_to_string(&app, 70, 12);
        assert!(out.contains("read_file"), "{out}");
        assert!(out.contains('✓'), "{out}");
    }

    #[test]
    fn tool_call_arguments_and_result_only_show_when_verbose() {
        let mut app = App::new("m".to_string(), None, "id".to_string());
        app.transcript.push(TranscriptItem::ToolCall {
            name: "read_file".into(),
            arguments: r#"{"filepath":"a.rs"}"#.into(),
            status: ToolStatus::Done {
                result: r#"{"success":true}"#.into(),
            },
        });

        let out = render_to_string(&app, 70, 12);
        assert!(out.contains("read_file"), "{out}");
        assert!(!out.contains("filepath"), "{out}");
        assert!(!out.contains("success"), "{out}");

        app.verbose = true;
        let out = render_to_string(&app, 70, 12);
        assert!(out.contains("filepath"), "{out}");
        assert!(out.contains("success"), "{out}");
    }

    #[test]
    fn tool_call_shows_its_file_or_command_even_when_not_verbose() {
        let mut app = App::new("m".to_string(), None, "id".to_string());
        app.transcript.push(TranscriptItem::ToolCall {
            name: "write_file".into(),
            arguments: r#"{"filepath":"src/main.rs","content":"fn main() {}"}"#.into(),
            status: ToolStatus::Running,
        });
        app.transcript.push(TranscriptItem::ToolCall {
            name: "run_terminal_command".into(),
            arguments: r#"{"command":"cargo test"}"#.into(),
            status: ToolStatus::Running,
        });

        let out = render_to_string(&app, 70, 16);
        assert!(out.contains("src/main.rs"), "{out}");
        assert!(!out.contains("fn main"), "{out}");
        assert!(out.contains("cargo test"), "{out}");
    }

    #[test]
    fn renders_without_panicking_at_awkward_sizes() {
        // The layout reserves 3 rows for input and 1 for status; a terminal
        // smaller than that must clamp rather than underflow.
        let mut app = sample_app();
        app.pending_approval = Some(ApprovalRequest {
            tool_name: "write_file".into(),
            category: "write",
            arguments: "{}".into(),
        });
        for (w, h) in [(1, 1), (3, 2), (10, 4), (20, 5), (200, 60)] {
            let _ = render_to_string(&app, w, h);
        }
    }

    fn plain(lines: &[Line]) -> Vec<String> {
        lines
            .iter()
            .map(|l| l.spans.iter().map(|s| s.content.as_ref()).collect())
            .collect()
    }

    #[test]
    fn markdown_strips_markers_and_styles_the_text() {
        let rendered = markdown_lines("**bold** and `code`");
        let text = plain(&rendered).join("");
        assert!(text.contains("bold"), "{text:?}");
        assert!(!text.contains("**"), "markers should be gone: {text:?}");
        assert!(!text.contains('`'), "markers should be gone: {text:?}");

        let styled = rendered
            .iter()
            .flat_map(|l| l.spans.iter())
            .filter(|s| s.style.add_modifier.contains(Modifier::BOLD))
            .count();
        assert!(styled > 0, "bold should carry a style");
    }

    #[test]
    fn code_fences_become_a_language_tag_and_keep_their_code() {
        let rendered = markdown_lines("before\n\n```rust\nfn main() {}\n```\n\nafter");
        let text = plain(&rendered);
        assert!(
            text.iter().all(|l| !l.contains("```")),
            "backticks should not survive: {text:?}"
        );
        assert!(text.iter().any(|l| l.trim() == "rust"), "{text:?}");
        assert!(text.iter().any(|l| l.contains("fn main()")), "{text:?}");
        assert!(text.iter().any(|l| l.contains("before")), "{text:?}");
        assert!(text.iter().any(|l| l.contains("after")), "{text:?}");
    }

    #[test]
    fn an_unlabelled_fence_still_marks_the_block() {
        let text = plain(&markdown_lines("```\nplain code\n```"));
        assert!(text.iter().any(|l| l.trim() == "code"), "{text:?}");
        assert!(text.iter().any(|l| l.contains("plain code")), "{text:?}");
        assert!(text.iter().all(|l| !l.contains("```")), "{text:?}");
    }

    #[test]
    fn lists_survive_rendering() {
        let text = plain(&markdown_lines("- one\n- two\n\n1. first\n2. second")).join("\n");
        for expected in ["one", "two", "first", "second"] {
            assert!(text.contains(expected), "{text:?}");
        }
    }

    #[test]
    fn only_assistant_text_is_treated_as_markdown() {
        let mut app = App::new("m".to_string(), None, "id".to_string());
        app.transcript
            .push(TranscriptItem::User("literal **stars** here".to_string()));
        app.transcript.push(TranscriptItem::Assistant {
            text: "rendered **stars** here".to_string(),
            streaming: false,
            label: Some("m".into()),
        });
        let out = render_to_string(&app, 60, 14);
        // The user's own asterisks are shown as typed; the reply's are not.
        assert!(out.contains("literal **stars**"), "{out}");
        assert!(!out.contains("rendered **stars**"), "{out}");
        assert!(out.contains("rendered stars"), "{out}");
    }

    #[test]
    fn a_streaming_reply_is_left_unformatted_until_it_finishes() {
        let mut app = App::new("m".to_string(), None, "id".to_string());
        // Mid-stream an unclosed fence would otherwise render as a stray tag.
        app.transcript.push(TranscriptItem::Assistant {
            text: "partial **bold".to_string(),
            streaming: true,
            label: Some("m".into()),
        });
        let out = render_to_string(&app, 60, 14);
        assert!(out.contains("partial **bold"), "{out}");
    }

    #[test]
    fn input_wraps_and_tracks_the_caret_together() {
        // The caret must land in the same rows the box actually draws.
        let rows = input_lines("abcdefghij", 4);
        assert_eq!(rows, vec!["abcd", "efgh", "ij"]);
        assert_eq!(input_cursor("abcdefghij", 10, 4), (2, 2));

        // Explicit newlines start a row, including empty ones.
        assert_eq!(input_lines("a\n\nb", 10), vec!["a", "", "b"]);
        assert_eq!(input_cursor("a\n\nb", 4, 10), (2, 1));

        // A segment that exactly fills a row puts the caret on the next.
        assert_eq!(input_lines("abcd", 4), vec!["abcd", ""]);
        assert_eq!(input_cursor("abcd", 4, 4), (1, 0));
    }

    #[test]
    fn the_message_box_grows_with_multiline_input() {
        let mut app = sample_app();
        let single = render_to_string(&app, 40, 16);
        app.input = "one\ntwo\nthree".to_string();
        app.cursor = app.input.len();
        let multi = render_to_string(&app, 40, 16);

        // All three lines are visible, and the box grew to show them.
        assert!(multi.contains("one") && multi.contains("two") && multi.contains("three"));
        let box_rows = |out: &str| out.lines().filter(|l| l.contains('│')).count();
        assert!(
            box_rows(&multi) > 0 && multi != single,
            "expected the input box to grow:\n{multi}"
        );
    }

    #[test]
    fn a_huge_input_cannot_squeeze_out_the_conversation() {
        let mut app = sample_app();
        app.input = (0..200)
            .map(|i| format!("line {i}"))
            .collect::<Vec<_>>()
            .join("\n");
        app.cursor = app.input.len();
        // Must render, and the transcript must still get rows.
        let out = render_to_string(&app, 40, 14);
        assert!(out.lines().count() == 14, "{out}");
    }

    #[test]
    fn summarize_flattens_and_truncates() {
        assert_eq!(summarize("a\nb\tc", 10), "a b c");
        assert_eq!(summarize(&"x".repeat(20), 5), "xxxxx…");
        assert_eq!(summarize("  padded  ", 20), "padded");
    }

    #[test]
    fn summarize_counts_characters_not_bytes() {
        // Truncating by byte index here would panic or corrupt the text.
        let text = "é".repeat(20);
        assert_eq!(summarize(&text, 3).chars().count(), 4); // 3 + ellipsis
    }
}
