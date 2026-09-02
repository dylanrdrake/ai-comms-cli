//! Drawing the TUI. Pure presentation over [`App`] — no state changes here.

use super::app::{App, Focus, ToolStatus, TranscriptItem};
use crate::ui::{json_fields, summarize, tool_call_fields, ApprovalRequest};
use ratatui::prelude::*;
use ratatui::widgets::{Block, Borders, Paragraph, Wrap};

/// Spinner frames, reused from the CLI's so both front ends feel the same.
/// Shared with the picker's working badge, so a busy session animates the
/// same way wherever it's shown.
pub(super) const FRAMES: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

/// Most rows the message box will grow to before it scrolls internally,
/// so a long paste can't squeeze the conversation off the screen.
const MAX_INPUT_ROWS: u16 = 10;

pub fn draw(frame: &mut Frame, app: &App, tick: usize) {
    // The message box grows with what's in it instead of staying at one
    // row — normally what's been typed, or, while a tool call is waiting on
    // a decision, the approval prompt that takes over the same box instead
    // of floating a separate modal over the conversation.
    let content_rows = match &app.pending_approval {
        // The typed-answer line plus the blank line under it, ahead of the
        // tool/argument detail — see `draw_approval`.
        Some(request) => approval_lines(request).len() as u16 + 2,
        None => input_lines(&app.input, frame.area().width.saturating_sub(2)).len() as u16,
    };
    let input_rows = content_rows
        .clamp(1, MAX_INPUT_ROWS)
        // Never take so much that the conversation has nowhere to go. The
        // reserve covers the title row, the rule under it, the input box's
        // own two border rows, and the settings/key-binding rows below it.
        .min(frame.area().height.saturating_sub(7).max(1));

    let areas = Layout::vertical([
        Constraint::Length(1),              // session title
        Constraint::Length(1),              // rule
        Constraint::Min(1),                 // chat history
        Constraint::Length(input_rows + 2), // message prompt, bordered, plus its borders
        Constraint::Length(1),              // settings: ask/agent, model, effort, temp, verbose
        Constraint::Length(1),              // key bindings
    ])
    .split(frame.area());

    draw_title(frame, areas[0], app);
    draw_rule(frame, areas[1], None);
    let scrolled = draw_transcript(frame, areas[2], app);
    draw_input(frame, areas[3], app, scrolled);
    draw_settings(frame, areas[4], app, tick);
    draw_keybindings(frame, areas[5]);
}

/// The session's title, plain — no border, no "comms -" prefix.
fn draw_title(frame: &mut Frame, area: Rect, app: &App) {
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            app.title.clone(),
            Style::new().bold(),
        ))),
        area,
    );
}

/// A subtle full-width divider, standing in for the borders the screen used
/// to have. `hint`, when given, is overlaid right-aligned on top of it —
/// used for the "scrolled" notice, the way a bordered box would have shown
/// it in its own title.
pub(super) fn draw_rule(frame: &mut Frame, area: Rect, hint: Option<&str>) {
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            "─".repeat(area.width as usize),
            Style::new().dark_gray(),
        ))),
        area,
    );
    if let Some(hint) = hint {
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(hint, Style::new().yellow())).right_aligned()),
            area,
        );
    }
}

/// Draws the transcript and reports whether the view is scrolled away from
/// the newest content, so the top status row can flag it.
fn draw_transcript(frame: &mut Frame, area: Rect, app: &App) -> bool {
    let mut lines: Vec<Line> = Vec::new();
    // User/Assistant text is wrapped by hand (see `wrap_styled`) rather
    // than left to ratatui's `Wrap`, specifically so a row broken by
    // wrapping — not just one broken by a literal newline — still lines up
    // under the gutter instead of resuming at column 0.
    let content_width = area.width.saturating_sub(2).max(1) as usize;

    for item in &app.transcript {
        match item {
            TranscriptItem::User(text) => {
                push_block(
                    &mut lines,
                    Span::styled("❯ ", Style::new().green().bold()),
                    text,
                    None,
                    content_width,
                );
                lines.push(Line::raw(""));
            }
            TranscriptItem::Assistant {
                text, streaming, ..
            } => {
                // No model label — just a plain dot marking it as a reply,
                // and the text.
                let prefix = Span::styled("● ", Style::new().cyan());
                let cursor = streaming.then(|| Span::styled("▌", Style::new().cyan()));
                if *streaming {
                    // Mid-stream the text is usually mid-construct — an
                    // unclosed fence or a half-written list — so render it
                    // plainly and let the finished message reformat once.
                    push_block(&mut lines, prefix, text, cursor, content_width);
                } else {
                    push_rendered(
                        &mut lines,
                        prefix,
                        markdown_lines(text),
                        cursor,
                        content_width,
                        GUTTER_CONTINUATION,
                    );
                }
                lines.push(Line::raw(""));
            }
            TranscriptItem::ToolCall {
                name,
                arguments,
                status,
            } => {
                // No trailing marker while running — the spinner-driven
                // "working" state in the settings row already says so; a
                // static triangle here didn't add anything.
                let marker: Option<(&str, Style)> = match status {
                    ToolStatus::AwaitingApproval => Some(("?", Style::new().yellow())),
                    ToolStatus::Running => None,
                    ToolStatus::Denied => Some(("✗", Style::new().red())),
                    ToolStatus::Done { .. } => Some(("✓", Style::new().green())),
                };
                let mut header = vec![Span::styled(name.clone(), Style::new().bold())];
                // The file or command a call is acting on identifies it well
                // enough to show even without -v; the rest of its arguments
                // (and its result) are the detail that gates behind verbose.
                if let Some(detail) = crate::ui::primary_argument(arguments) {
                    header.push(Span::styled(
                        format!("  {}", summarize(&detail, 60)),
                        Style::new().dark_gray(),
                    ));
                }
                push_rendered(
                    &mut lines,
                    // The gutter marks the row as a tool call; the status
                    // (still color-coded) rides at the end instead, as a
                    // `trailing` marker — same mechanism the streaming
                    // cursor uses, so it always lands on the last wrapped
                    // row rather than getting buried mid-wrap.
                    Span::styled("🔨 ", Style::new().magenta()),
                    vec![Line::from(header)],
                    marker.map(|(m, style)| Span::styled(format!(" {m}"), style)),
                    // `content_width` assumes a 2-column prefix, one less
                    // than "🔨 "'s actual 3 (🔨 is double-width) — wrap one
                    // column narrower so the prefixed row still fits, rather
                    // than overflowing the terminal width and getting
                    // wrapped a second time, out from under the gutter.
                    content_width.saturating_sub(1),
                    "   ",
                );
                if app.verbose {
                    for (key, shown) in tool_call_fields(name, arguments) {
                        push_labeled(&mut lines, format!("     {key}  "), shown, content_width);
                    }
                    if let ToolStatus::Done { result } = status {
                        for (key, shown) in json_fields(result) {
                            push_labeled(&mut lines, format!("     {key}  "), shown, content_width);
                        }
                    }
                }
                lines.push(Line::raw(""));
            }
            TranscriptItem::Thinking(text) => {
                if app.verbose {
                    let thought: Vec<Line<'static>> = text
                        .lines()
                        .map(|line| {
                            Line::from(Span::styled(
                                line.to_string(),
                                Style::new().dark_gray().italic(),
                            ))
                        })
                        .collect();
                    push_rendered(
                        &mut lines,
                        Span::styled("💭 ", Style::new().dark_gray()),
                        thought,
                        None,
                        // 💭 is double-width, so wrap a column narrower —
                        // same adjustment the tool-call gutter makes.
                        content_width.saturating_sub(1),
                        "   ",
                    );
                    lines.push(Line::raw(""));
                }
            }
            TranscriptItem::Error(message) => {
                lines.push(Line::from(vec![
                    Span::styled("✗ ", Style::new().red().bold()),
                    Span::styled(message.clone(), Style::new().red()),
                ]));
                lines.push(Line::raw(""));
            }
            TranscriptItem::Notice(message) => {
                lines.push(Line::from(vec![
                    Span::styled("— ", Style::new().dark_gray().italic()),
                    Span::styled(message.clone(), Style::new().dark_gray().italic()),
                ]));
                lines.push(Line::raw(""));
            }
            TranscriptItem::SessionStatus(rows) => {
                lines.push(Line::from(vec![
                    Span::styled("— ", Style::new().dark_gray().italic()),
                    Span::styled("Session:", Style::new().dark_gray().italic()),
                ]));
                // Values line up under each other, so the column of labels
                // reads as a list rather than as ragged prose.
                let width = rows.iter().map(|(label, _)| label.len()).max().unwrap_or(0);
                for (label, value) in rows {
                    lines.push(Line::from(vec![
                        Span::styled(format!("      {label:<width$}  "), Style::new().dark_gray()),
                        Span::raw(value.clone()),
                    ]));
                }
                lines.push(Line::raw(""));
            }
            TranscriptItem::ApprovalStatus { approval, changed } => {
                lines.push(Line::from(vec![
                    Span::styled("— ", Style::new().dark_gray().italic()),
                    Span::styled(
                        format!("Approval {}:", if *changed { "set to" } else { "is" }),
                        Style::new().dark_gray().italic(),
                    ),
                ]));
                for (label, enabled) in [
                    ("Read from disk:    ", approval.read_disk),
                    ("Write to disk:     ", approval.write_disk),
                    ("Terminal commands: ", approval.terminal),
                ] {
                    let (mark, word, style) = if enabled {
                        ("✓", "Ask", Style::new().green())
                    } else {
                        ("✗", "Auto", Style::new().yellow())
                    };
                    lines.push(Line::from(vec![
                        Span::styled(format!("      {label}"), Style::new().dark_gray()),
                        Span::styled(format!("{mark} {word}"), style),
                    ]));
                }
                lines.push(Line::raw(""));
            }
        }
    }

    // Every block appends a trailing blank; drop it so the newest message
    // sits flush against the bottom rather than floating above a gap.
    while matches!(lines.last(), Some(line) if line.spans.iter().all(|s| s.content.is_empty())) {
        lines.pop();
    }

    // No border, and the title now rides in its own row above `area`
    // (see `draw_title`), so the whole of `area` is free for content.
    let inner_width = area.width;
    let visible = area.height;

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

    frame.render_widget(paragraph.scroll((offset, 0)), area);

    // Reported so the rule below can carry the "scrolled" notice instead of
    // a border's bottom title, since there's no border to carry it anymore.
    !app.is_pinned_to_bottom() && max_offset > 0
}

/// `scrolled` carries the "scrolled — End to follow" notice onto the box's
/// top border, right-aligned — the same edge the transcript's own border
/// used to show it on, back when it had one.
fn draw_input(frame: &mut Frame, area: Rect, app: &App, scrolled: bool) {
    if let Some(request) = &app.pending_approval {
        draw_approval(frame, area, app, request, scrolled);
        return;
    }

    let width = area.width.saturating_sub(2).max(1);
    let rows = input_lines(&app.input, width);
    let (cursor_row, cursor_col) = input_cursor(&app.input, app.cursor, width);

    // Once the text is taller than the box, follow the cursor rather than
    // pinning to the top, so what you're typing stays on screen.
    let visible = area.height.saturating_sub(2).max(1);
    let scroll = (cursor_row + 1).saturating_sub(visible);

    let mut block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::new().dark_gray());
    if scrolled {
        block = block.title(
            Line::from(Span::styled(
                " scrolled — End to follow ",
                Style::new().yellow(),
            ))
            .right_aligned(),
        );
    }

    // Wrapped by hand rather than by `Wrap`, so the cursor position below is
    // computed against exactly the rows being drawn.
    let paragraph = Paragraph::new(Text::from(
        rows.into_iter().map(Line::from).collect::<Vec<_>>(),
    ))
    .block(block)
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

/// A muted-to-intense gradient for `low`/`medium`/`high`; unset (following
/// the configured default) stays the same dark_gray every other "no
/// override" field uses.
fn effort_style(effort_level: Option<&str>) -> Style {
    match effort_level {
        Some("low") => Style::new().cyan(),
        Some("medium") => Style::new().yellow(),
        Some("high") => Style::new().red(),
        _ => Style::new().dark_gray(),
    }
}

/// A cool-to-hot gradient matching the word itself; unset stays the same
/// dark_gray every other "no override" field uses. Pink rather than red at
/// the top — red and orange read too similarly next to each other.
fn temperature_style(temperature: Option<f32>) -> Style {
    const ORANGE: Color = Color::Rgb(255, 140, 0);
    const PINK: Color = Color::Rgb(255, 105, 180);
    match temperature {
        None => Style::new().dark_gray(),
        Some(t) if t < 0.5 => Style::new().cyan(),
        Some(t) if t < 1.0 => Style::new().yellow(),
        Some(t) if t < 1.5 => Style::new().fg(ORANGE),
        Some(_) => Style::new().fg(PINK),
    }
}

/// Every controllable setting in one row below the message prompt: ready/
/// busy, ask/agent mode, model, effort, temperature, verbose, and how many
/// messages are queued — everything `/model`, `/agent`, `/effort`,
/// `/temperature`, `/verbose`, etc. can change.
fn draw_settings(frame: &mut Frame, area: Rect, app: &App, tick: usize) {
    let mut spans = Vec::new();

    if app.busy {
        spans.push(Span::styled(
            format!(" {} working ", FRAMES[tick % FRAMES.len()]),
            Style::new().yellow(),
        ));
    } else {
        spans.push(Span::styled(" ready ", Style::new().green()));
    }

    spans.push(Span::styled(
        format!("· {} ", app.model),
        Style::new().dark_gray(),
    ));
    spans.push(Span::styled(
        format!("· {} ", if app.agentic { "agent" } else { "ask" }),
        if app.agentic {
            Style::new().yellow()
        } else {
            Style::new().cyan()
        },
    ));
    let effort_label = app.effort_level.as_deref().unwrap_or("default");
    spans.push(Span::styled(
        format!("· 🧠 {effort_label} "),
        effort_style(app.effort_level.as_deref()),
    ));
    let temp_label = app
        .temperature
        .map(|n| n.to_string())
        .unwrap_or_else(|| "default".to_string());
    spans.push(Span::styled(
        format!("· 🌡 {temp_label} "),
        temperature_style(app.temperature),
    ));
    spans.push(Span::styled(
        format!("· {} ", if app.verbose { "verbose" } else { "quiet" }),
        if app.verbose {
            Style::new().yellow()
        } else {
            Style::new().dark_gray()
        },
    ));

    if app.queued > 0 {
        spans.push(Span::styled(
            format!("· {} queued ", app.queued),
            Style::new().dark_gray(),
        ));
    }

    frame.render_widget(Paragraph::new(Line::from(spans)), area);
}

/// The keybinding hints, on their own row at the very bottom.
/// Dimmed a step further than the rest of the muted (dark_gray) text
/// elsewhere, so it recedes into the background rather than competing with
/// the settings row right above it.
fn draw_keybindings(frame: &mut Frame, area: Rect) {
    // A shade darker than the plain `dark_gray()` used elsewhere — `.dim()`
    // alone isn't reliable across terminals (some ignore the SGR faint
    // attribute entirely), so the color itself carries the extra dimness.
    const KEYBIND_GRAY: Color = Color::Rgb(90, 90, 90);
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            " Enter send · Esc cancel · PgUp/PgDn scroll · Ctrl-B back · Ctrl-C quit",
            Style::new().fg(KEYBIND_GRAY).dim(),
        ))),
        area,
    );
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
fn draw_approval(
    frame: &mut Frame,
    area: Rect,
    app: &App,
    request: &ApprovalRequest,
    scrolled: bool,
) {
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

    let mut block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::new().yellow())
        .title(Span::styled(title, Style::new().yellow().bold()));
    if scrolled {
        block = block.title(
            Line::from(Span::styled(
                " scrolled — End to follow ",
                Style::new().yellow(),
            ))
            .right_aligned(),
        );
    }

    let paragraph = Paragraph::new(Text::from(lines)).block(block);
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
/// Every message-start line leads with a 2-column glyph — `❯ `, `✓ `, `— `,
/// or two blank spaces where there's no icon — so replies read as one
/// aligned gutter down the left edge. Continuation lines — a literal
/// embedded newline, or a row `wrap_styled` broke a long one into — get
/// this same blank width instead of the glyph, so their text lines up
/// under the first line's content rather than under the icon.
const GUTTER_CONTINUATION: &str = "  ";

/// Pushes a "label  value" row, one level of indent deeper than the
/// message gutter — a verbose tool-call field or result. The value wraps
/// and, unlike the message gutter, its continuation lines up under the
/// value itself rather than back at the label, since there's no icon here
/// competing for that column.
fn push_labeled(lines: &mut Vec<Line<'static>>, label: String, value: String, width: usize) {
    let label_width = label.chars().count();
    let value_width = width.saturating_sub(label_width).max(1);
    let blank = " ".repeat(label_width);
    for (index, mut row) in wrap_styled(Line::from(Span::raw(value)), value_width)
        .into_iter()
        .enumerate()
    {
        if index == 0 {
            row.spans
                .insert(0, Span::styled(label.clone(), Style::new().dark_gray()));
        } else {
            row.spans.insert(0, Span::raw(blank.clone()));
        }
        lines.push(row);
    }
}

/// `gutter` is the continuation indent for a wrapped row — normally
/// [`GUTTER_CONTINUATION`], sized to match a single-width marker
/// (`❯`/`●`/`—`) plus its trailing space, but callers whose `prefix` is
/// wider (🔨 is double-width, so `"🔨 "` alone fills 3 columns) pass a
/// wider one instead, so a wrapped continuation row still lines up under
/// the first row's actual text rather than the usual 2-column gutter.
fn push_rendered(
    lines: &mut Vec<Line<'static>>,
    prefix: Span<'static>,
    mut rendered: Vec<Line<'static>>,
    trailing: Option<Span<'static>>,
    width: usize,
    gutter: &str,
) {
    if rendered.is_empty() {
        rendered.push(Line::raw(""));
    }
    let last_line = rendered.len() - 1;
    for (line_index, line) in rendered.into_iter().enumerate() {
        let wrapped = wrap_styled(line, width);
        let last_row = wrapped.len() - 1;
        for (row_index, mut row) in wrapped.into_iter().enumerate() {
            if line_index == 0 && row_index == 0 {
                row.spans.insert(0, prefix.clone());
            } else {
                row.spans.insert(0, Span::raw(gutter.to_string()));
            }
            if line_index == last_line && row_index == last_row {
                if let Some(trailing) = trailing.clone() {
                    row.spans.push(trailing);
                }
            }
            lines.push(row);
        }
    }
}

/// Pushes one speaker's text, split into a `Line` per newline (and, within
/// each, further wrapped to `width` — see `wrap_styled`).
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
    width: usize,
) {
    let segments: Vec<&str> = text.split('\n').collect();
    let last_segment = segments.len() - 1;
    for (seg_index, segment) in segments.into_iter().enumerate() {
        let wrapped = wrap_styled(Line::from(Span::raw(segment.to_string())), width);
        let last_row = wrapped.len() - 1;
        for (row_index, mut row) in wrapped.into_iter().enumerate() {
            if seg_index == 0 && row_index == 0 {
                row.spans.insert(0, prefix.clone());
            } else {
                row.spans.insert(0, Span::raw(GUTTER_CONTINUATION));
            }
            if seg_index == last_segment && row_index == last_row {
                if let Some(trailing) = trailing.clone() {
                    row.spans.push(trailing);
                }
            }
            lines.push(row);
        }
    }
}

/// Word-wraps one styled line to `width` columns, keeping each span's style
/// attached to the text it colors. Breaks preferentially at spaces; a
/// single word longer than `width` is hard-broken so no row ever exceeds
/// it. Doing this ourselves — rather than leaving it to ratatui's `Wrap`,
/// which has no notion of a hanging indent — is what lets a wrapped
/// continuation row share the gutter indent with the row it continues,
/// the same as a row split by a literal newline already does.
fn wrap_styled(line: Line<'static>, width: usize) -> Vec<Line<'static>> {
    let width = width.max(1);

    // Each span's text broken into (word-or-space, is_space) tokens, style
    // still attached, so a word split across a style boundary (rare, but
    // possible around markdown emphasis markers) still wraps sanely.
    let mut tokens: Vec<(String, Style, bool)> = Vec::new();
    for span in line.spans {
        let style = span.style;
        let mut word = String::new();
        for ch in span.content.chars() {
            if ch == ' ' {
                if !word.is_empty() {
                    tokens.push((std::mem::take(&mut word), style, false));
                }
                tokens.push((" ".to_string(), style, true));
            } else {
                word.push(ch);
            }
        }
        if !word.is_empty() {
            tokens.push((word, style, false));
        }
    }

    let mut rows: Vec<Vec<Span<'static>>> = vec![Vec::new()];
    let mut col = 0usize;
    for (index, (text, style, is_space)) in tokens.into_iter().enumerate() {
        let token_width = text.chars().count();

        if is_space {
            // A space landing exactly at the start of a row *a wrap broke
            // onto* is just where the break happened to fall — starting
            // that row indented by it would look like a stray extra space.
            // But the line's own leading whitespace (`index == 0`, e.g. a
            // code block's indentation) is real content and must survive.
            if col == 0 && index != 0 {
                continue;
            }
            if col + token_width > width {
                rows.push(Vec::new());
                col = 0;
            } else {
                rows.last_mut().unwrap().push(Span::styled(text, style));
                col += token_width;
            }
            continue;
        }

        if token_width > width {
            // Doesn't fit on a row by itself either way: hard-break it.
            let chars: Vec<char> = text.chars().collect();
            for chunk in chars.chunks(width) {
                if col > 0 {
                    rows.push(Vec::new());
                }
                col = chunk.len();
                rows.last_mut()
                    .unwrap()
                    .push(Span::styled(chunk.iter().collect::<String>(), style));
            }
            continue;
        }

        if col > 0 && col + token_width > width {
            rows.push(Vec::new());
            col = 0;
        }
        rows.last_mut().unwrap().push(Span::styled(text, style));
        col += token_width;
    }

    rows.into_iter().map(Line::from).collect()
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
        assert!(out.contains("❯ hello"), "{out}");
        assert!(out.contains("hi there"), "{out}");
        assert!(out.contains("test-model"), "{out}");
        assert!(out.contains("ready"), "{out}");
    }

    #[test]
    fn status_bar_labels_the_mode_ask_or_agent() {
        let mut app = sample_app();
        assert!(!app.agentic);
        let out = render_to_string(&app, 60, 20);
        assert!(out.contains("ask"), "{out}");
        assert!(!out.contains("chat"), "{out}");

        app.agentic = true;
        let out = render_to_string(&app, 60, 20);
        assert!(out.contains("agent"), "{out}");
    }

    #[test]
    fn title_row_shows_the_session_title_alone() {
        let mut app = sample_app();
        app.title = "Write me a snake game".to_string();
        let out = render_to_string(&app, 60, 20);
        let title_row = out.lines().next().unwrap();
        assert_eq!(title_row.trim_end(), "Write me a snake game");
    }

    #[test]
    fn top_status_shows_model_and_effort() {
        let mut app = sample_app();
        let out = render_to_string(&app, 80, 20);
        assert!(out.contains("test-model"), "{out}");
        assert!(out.contains("default"), "{out}");

        app.effort_level = Some("high".to_string());
        let out = render_to_string(&app, 80, 20);
        assert!(out.contains("high"), "{out}");
    }

    #[test]
    fn top_status_shows_temperature_at_the_end() {
        let mut app = sample_app();
        let out = render_to_string(&app, 80, 20);
        assert!(out.contains("🌡 default"), "{out}");

        app.temperature = Some(1.2);
        let out = render_to_string(&app, 80, 20);
        assert!(out.contains("🌡 1.2"), "{out}");
    }

    #[test]
    fn temperature_style_follows_a_cool_to_hot_gradient() {
        assert_eq!(temperature_style(None), Style::new().dark_gray());
        assert_eq!(temperature_style(Some(0.0)), Style::new().cyan());
        assert_eq!(temperature_style(Some(0.7)), Style::new().yellow());
        assert_eq!(
            temperature_style(Some(1.2)),
            Style::new().fg(Color::Rgb(255, 140, 0))
        );
        assert_eq!(
            temperature_style(Some(2.0)),
            Style::new().fg(Color::Rgb(255, 105, 180))
        );
    }

    #[test]
    fn effort_style_follows_a_calm_to_intense_gradient() {
        assert_eq!(effort_style(None), Style::new().dark_gray());
        assert_eq!(effort_style(Some("low")), Style::new().cyan());
        assert_eq!(effort_style(Some("medium")), Style::new().yellow());
        assert_eq!(effort_style(Some("high")), Style::new().red());
    }

    #[test]
    fn bottom_status_shows_the_verbose_indicator() {
        let mut app = sample_app();
        assert!(!app.verbose);
        let out = render_to_string(&app, 80, 20);
        assert!(out.contains("quiet"), "{out}");

        app.verbose = true;
        let out = render_to_string(&app, 80, 20);
        assert!(out.contains("verbose"), "{out}");
    }

    #[test]
    fn a_short_conversation_sits_at_the_bottom_of_the_pane() {
        let out = render_to_string(&sample_app(), 60, 20);
        let rows: Vec<&str> = out.lines().collect();
        // Below the chat history: the input box's 3 rows (top border, one
        // content row for the empty input here, bottom border), the
        // settings row, and the key-bindings row — the last content row
        // sits right above all five of those.
        let last_content = rows.len() - 6;
        assert!(
            rows[last_content].contains("hi there"),
            "newest message should be flush with the bottom of the pane, got:\n{out}"
        );
        // ...and the space is above it, not below. Row 0 is the session
        // title and row 1 the rule under it, so content starts at row 2.
        assert!(
            rows[2].trim().is_empty(),
            "expected blank space above the conversation, got:\n{out}"
        );
    }

    #[test]
    fn scrolling_away_from_the_bottom_flags_the_input_box() {
        let mut app = sample_app();
        for i in 0..30 {
            app.transcript
                .push(TranscriptItem::User(format!("message {i}")));
        }
        let pinned = render_to_string(&app, 60, 20);
        assert!(!pinned.contains("scrolled"), "{pinned}");

        app.scroll_back = 3;
        let scrolled = render_to_string(&app, 60, 20);
        assert!(scrolled.contains("scrolled — End to follow"), "{scrolled}");
        // On the input box's own top border, not floating elsewhere.
        let hint_row = scrolled
            .lines()
            .find(|l| l.contains("scrolled"))
            .expect("hint shown");
        assert!(
            hint_row.starts_with('┌') && hint_row.ends_with('┐'),
            "{hint_row}"
        );
    }

    #[test]
    fn wrapped_continuation_rows_align_under_the_gutter() {
        let mut app = App::new("m".to_string(), None, "id".to_string());
        app.transcript.push(TranscriptItem::User(
            "one two three four five six seven eight nine ten".into(),
        ));
        let out = render_to_string(&app, 30, 14);
        let row = out
            .lines()
            .position(|l| l.trim_start().starts_with("❯ one"))
            .expect("first row shown");
        // The row after it is a wrap-induced continuation (no literal
        // newline in the input), and should start 2 columns in — lined up
        // under "one", not back at column 0 under the glyph.
        let continuation = out.lines().nth(row + 1).expect("continuation row");
        assert!(
            continuation.starts_with("  ") && !continuation.trim().is_empty(),
            "{continuation:?}"
        );
    }

    #[test]
    fn code_block_indentation_survives_wrapping() {
        // The wrap-styled "drop a leading space" rule is meant for the
        // stray space a wrap break happens to land on mid-line — not for a
        // line's own leading whitespace, which is real content (nested
        // indentation inside a code block, say) and must be kept.
        let mut app = App::new("m".to_string(), None, "id".to_string());
        app.transcript.push(TranscriptItem::Assistant {
            text: "```python\nif True:\n    return 1\n```".into(),
            streaming: false,
            label: Some("m".into()),
        });
        let out = render_to_string(&app, 50, 20);
        let indented = out
            .lines()
            .find(|l| l.contains("return 1"))
            .expect("indented line shown");
        // Gutter (2 cols) + the code's own 4-space indent.
        assert!(indented.starts_with("      return 1"), "{indented:?}");
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
    fn tool_call_gutter_is_generic_and_status_trails_the_line() {
        let mut app = App::new("m".to_string(), None, "id".to_string());
        app.transcript.push(TranscriptItem::ToolCall {
            name: "read_file".into(),
            arguments: r#"{"filepath":"a.rs"}"#.into(),
            status: ToolStatus::Done {
                result: r#"{"success":true}"#.into(),
            },
        });
        let out = render_to_string(&app, 70, 12);
        let row = out
            .lines()
            .find(|l| l.contains("read_file"))
            .expect("header row shown");
        assert!(row.trim_start().starts_with("🔨  read_file"), "{row:?}");
        assert!(row.trim_end().ends_with('✓'), "{row:?}");
    }

    #[test]
    fn a_running_tool_call_has_no_trailing_marker() {
        let mut app = App::new("m".to_string(), None, "id".to_string());
        app.transcript.push(TranscriptItem::ToolCall {
            name: "run_terminal_command".into(),
            arguments: r#"{"command":"cargo build"}"#.into(),
            status: ToolStatus::Running,
        });
        let out = render_to_string(&app, 70, 12);
        let row = out
            .lines()
            .find(|l| l.contains("run_terminal_command"))
            .expect("header row shown");
        assert!(
            row.trim_start().starts_with("🔨  run_terminal_command"),
            "{row:?}"
        );
        assert!(!row.contains('▸'), "{row:?}");
    }

    #[test]
    fn tool_call_header_wraps_under_the_gutter() {
        let mut app = App::new("m".to_string(), None, "id".to_string());
        app.transcript.push(TranscriptItem::ToolCall {
            name: "a_pretty_long_tool_name_that_should_wrap_around".into(),
            arguments: "{}".into(),
            status: ToolStatus::Running,
        });
        let out = render_to_string(&app, 30, 14);
        let row = out
            .lines()
            .position(|l| l.trim_start().starts_with("🔨  a_pretty"))
            .expect("header row shown");
        let continuation = out.lines().nth(row + 1).expect("continuation row");
        // 3 columns, not the usual 2 — "🔨 " is double-width, one column
        // wider than the other markers' gutter.
        assert!(
            continuation.starts_with("   ") && !continuation.trim().is_empty(),
            "{continuation:?}"
        );
    }

    #[test]
    fn verbose_field_values_wrap_under_themselves_not_the_label() {
        let mut app = App::new("m".to_string(), None, "id".to_string());
        app.verbose = true;
        app.transcript.push(TranscriptItem::ToolCall {
            name: "write_file".into(),
            arguments:
                r#"{"content":"a value long enough that it should wrap onto a second row here"}"#
                    .into(),
            status: ToolStatus::Done {
                result: "{}".into(),
            },
        });
        let out = render_to_string(&app, 40, 16);
        let label_row = out
            .lines()
            .position(|l| l.contains("content"))
            .expect("field label shown");
        let continuation = out.lines().nth(label_row + 1).expect("continuation row");
        // Indented past the label's own width, not just the 2-column
        // message gutter, and not empty.
        assert!(
            continuation.starts_with("     ") && !continuation.trim().is_empty(),
            "{continuation:?}"
        );
    }

    #[test]
    fn approval_status_pretty_prints_each_gate_like_the_cli_does() {
        let mut app = App::new("m".to_string(), None, "id".to_string());
        app.transcript.push(TranscriptItem::ApprovalStatus {
            approval: crate::config::ApprovalSettings {
                read_disk: false,
                write_disk: true,
                terminal: true,
            },
            changed: false,
        });
        let out = render_to_string(&app, 70, 12);
        assert!(out.contains("Approval is:"), "{out}");
        assert!(out.contains("Read from disk:"), "{out}");
        assert!(out.contains("Write to disk:"), "{out}");
        assert!(out.contains("Terminal commands:"), "{out}");
        assert!(out.contains("✗ Auto"), "{out}");
        assert!(out.contains("✓ Ask"), "{out}");
    }

    #[test]
    fn session_status_lists_every_setting() {
        let mut app = App::new("m".to_string(), None, "id".to_string());
        app.transcript.push(TranscriptItem::SessionStatus(vec![
            ("Model".to_string(), "openrouter/auto".to_string()),
            ("Temperature".to_string(), "none sent".to_string()),
        ]));

        let out = render_to_string(&app, 70, 14);
        assert!(out.contains("Session:"), "{out}");
        assert!(out.contains("Model"), "{out}");
        assert!(out.contains("openrouter/auto"), "{out}");
        assert!(out.contains("none sent"), "{out}");
        // Unlike thinking, this is a direct answer to a question the user
        // just asked, so it shows regardless of verbose.
        assert!(!app.verbose);
    }

    #[test]
    fn thinking_only_shows_when_verbose() {
        let mut app = App::new("m".to_string(), None, "id".to_string());
        app.transcript
            .push(TranscriptItem::Thinking("weighing the options".to_string()));

        let out = render_to_string(&app, 70, 12);
        assert!(!out.contains("weighing the options"), "{out}");

        app.verbose = true;
        let out = render_to_string(&app, 70, 12);
        assert!(out.contains("weighing the options"), "{out}");
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

        // All three lines are visible simultaneously — which only happens if
        // the box actually grew to 3 rows; at 1 row, cursor-follow scrolling
        // would show just "three" and hide the rest.
        assert!(
            multi.contains("one") && multi.contains("two") && multi.contains("three"),
            "expected the input box to grow:\n{multi}"
        );
        assert_ne!(multi, single);
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
