//! Drawing the TUI. Pure presentation over [`App`] — no state changes here.

use super::app::{App, Focus, ToolStatus, TranscriptItem};
use ratatui::prelude::*;
use ratatui::widgets::{Block, Borders, Clear, Paragraph, Wrap};

/// Spinner frames, reused from the CLI's so both front ends feel the same.
const FRAMES: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

/// Most rows the message box will grow to before it scrolls internally,
/// so a long paste can't squeeze the conversation off the screen.
const MAX_INPUT_ROWS: u16 = 10;

pub fn draw(frame: &mut Frame, app: &App, tick: usize) {
    // The message box grows with what's typed instead of staying at one row,
    // so multi-line input is visible while writing it.
    let input_width = frame.area().width.saturating_sub(2);
    let input_rows = (input_lines(&app.input, input_width).len() as u16)
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

    if app.pending_approval.is_some() {
        draw_approval(frame, frame.area(), app);
    }
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
                lines.push(Line::from(vec![
                    Span::styled(format!("  {marker} "), style),
                    Span::styled(name.clone(), Style::new().bold()),
                    Span::styled(
                        format!("  {}", summarize(arguments, 60)),
                        Style::new().dark_gray(),
                    ),
                ]));
                if let ToolStatus::Done { result } = status {
                    lines.push(Line::from(Span::styled(
                        format!("     {}", summarize(result, 80)),
                        Style::new().dark_gray(),
                    )));
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
    let (title, style) = if app.focus == Focus::Approval {
        (" allow this? y / n ", Style::new().yellow())
    } else if app.busy {
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

fn draw_approval(frame: &mut Frame, area: Rect, app: &App) {
    let Some(request) = &app.pending_approval else {
        return;
    };

    let category = match request.category {
        "read" => "Read from disk",
        "write" => "Write to disk",
        "terminal" => "Terminal command",
        _ => "Unknown action",
    };

    let mut lines = vec![
        Line::from(Span::styled(category, Style::new().yellow().bold())),
        Line::raw(""),
        Line::from(vec![
            Span::styled("tool  ", Style::new().dark_gray()),
            Span::styled(request.tool_name.clone(), Style::new().bold()),
        ]),
    ];

    // Show arguments field by field, matching how the CLI presents them.
    if let Ok(value) = serde_json::from_str::<serde_json::Value>(&request.arguments) {
        if let Some(object) = value.as_object() {
            for (key, value) in object {
                let shown = value
                    .as_str()
                    .map(|s| s.to_string())
                    .unwrap_or_else(|| value.to_string());
                lines.push(Line::from(vec![
                    Span::styled(format!("{key}  "), Style::new().dark_gray()),
                    Span::raw(summarize(&shown, 100)),
                ]));
            }
        }
    }

    lines.push(Line::raw(""));
    lines.push(Line::from(vec![
        Span::styled("y", Style::new().green().bold()),
        Span::raw(" allow    "),
        Span::styled("n", Style::new().red().bold()),
        Span::raw(" deny    "),
        Span::styled("Esc", Style::new().bold()),
        Span::raw(" cancel turn"),
    ]));

    let popup = centered(area, 70, lines.len() as u16 + 2);
    frame.render_widget(Clear, popup);
    frame.render_widget(
        Paragraph::new(Text::from(lines))
            .wrap(Wrap { trim: false })
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .border_style(Style::new().yellow())
                    .title(Span::styled(
                        " approval needed ",
                        Style::new().yellow().bold(),
                    )),
            ),
        popup,
    );
}

/// A box of at most `width`x`height`, centered, clamped to what fits.
fn centered(area: Rect, width: u16, height: u16) -> Rect {
    let width = width.min(area.width.saturating_sub(2));
    let height = height.min(area.height.saturating_sub(2));
    Rect {
        x: area.x + (area.width.saturating_sub(width)) / 2,
        y: area.y + (area.height.saturating_sub(height)) / 2,
        width,
        height,
    }
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

/// One-line preview of a possibly long/multi-line value.
fn summarize(text: &str, max: usize) -> String {
    let flat: String = text
        .chars()
        .map(|c| if c == '\n' || c == '\t' { ' ' } else { c })
        .collect();
    let flat = flat.trim();
    if flat.chars().count() > max {
        let kept: String = flat.chars().take(max).collect();
        format!("{kept}…")
    } else {
        flat.to_string()
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
    fn approval_modal_shows_tool_and_arguments() {
        let mut app = sample_app();
        app.pending_approval = Some(ApprovalRequest {
            tool_name: "write_file".into(),
            category: "write",
            arguments: r#"{"filepath":"/tmp/a.txt"}"#.into(),
        });
        let out = render_to_string(&app, 70, 20);
        assert!(out.contains("approval needed"), "{out}");
        assert!(out.contains("Write to disk"), "{out}");
        assert!(out.contains("write_file"), "{out}");
        assert!(out.contains("/tmp/a.txt"), "{out}");
        assert!(out.contains("allow"), "{out}");
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

    #[test]
    fn centered_box_fits_inside_small_areas() {
        let area = Rect::new(0, 0, 20, 6);
        let popup = centered(area, 70, 30);
        assert!(popup.width <= area.width);
        assert!(popup.height <= area.height);
        assert!(popup.x + popup.width <= area.x + area.width);
        assert!(popup.y + popup.height <= area.y + area.height);
    }
}
