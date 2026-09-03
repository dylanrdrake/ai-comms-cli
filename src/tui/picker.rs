//! The launch screen: everything you can start or return to, in one list.
//!
//! There was a second screen listing all sessions, which meant the launch
//! screen could only offer a handful and hand off to it. One list grouped by
//! where each session lives says more in fewer keystrokes — the sessions for
//! the directory you're in are the ones you almost always want, and the rest
//! are still right there.
//!
//! Kept free of I/O: the caller loads sessions and acts on the
//! [`Activation`] returned when a row is chosen.

use super::render::{band, draw_rule, home_relative, identicon, pad_to};
use crate::store::{mode_label, Activity, LastMessage, SessionSummary, KIND_AGENT_CHAT};
use ratatui::prelude::*;
use ratatui::widgets::Paragraph;
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionRow {
    pub id: String,
    pub kind: String,
    pub title: String,
    pub updated_at: i64,
    /// Where the session was started, which is its sandbox boundary. `None`
    /// for one recorded before that was tracked.
    pub working_dir: Option<String>,
    /// Where the session left off, from its most recent stored message.
    /// `None` for one that has never been used.
    pub last: Option<LastMessage>,
    /// What its process says it's doing, when it said anything.
    pub activity: Option<Activity>,
    /// The line that goes with that — for an approval, what is being asked.
    pub activity_detail: Option<String>,
}

impl From<SessionSummary> for SessionRow {
    fn from(summary: SessionSummary) -> Self {
        SessionRow {
            id: summary.id,
            kind: summary.kind,
            title: summary.title,
            updated_at: summary.updated_at,
            working_dir: summary.working_dir,
            last: None,
            activity: summary.activity,
            activity_detail: summary.activity_detail,
        }
    }
}

impl SessionRow {
    pub fn short_id(&self) -> &str {
        &self.id[..8.min(self.id.len())]
    }

    pub fn is_agentic(&self) -> bool {
        self.kind == KIND_AGENT_CHAT
    }
}

/// A row on the launch screen.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LaunchItem {
    NewSession,
    /// A section label. Not selectable — the cursor steps over it.
    Header(&'static str),
    Resume(SessionRow),
}

/// What choosing a row means to the caller.
#[derive(Debug, Clone)]
pub enum Activation {
    NewSession,
    Resume(SessionRow),
    Delete(SessionRow),
    /// Resume a session whose recorded directory is gone, in the current
    /// one, repointing it there.
    Repoint(SessionRow),
}

/// A list with a moving selection. Empty lists are allowed (a fresh install
/// has no sessions), in which case there is nothing to activate.
#[derive(Debug, Default)]
pub struct Picker {
    pub items: Vec<LaunchItem>,
    pub selected: usize,
    /// Set while a delete is awaiting y/n confirmation, since dropping a
    /// saved conversation shouldn't be one keystroke away.
    pub confirming_delete: Option<SessionRow>,
    /// Set while a rename is in progress: the row being renamed, and the
    /// text typed so far (pre-filled with its current title).
    pub renaming: Option<(SessionRow, String)>,
    /// Set while a repoint is awaiting y/n: the row, and the directory it
    /// was recorded in and can no longer be opened from.
    pub confirming_repoint: Option<(SessionRow, String)>,
    /// Which session ids fall in each section, so a delete can regroup what
    /// remains without re-reading the database.
    here: Vec<String>,
    elsewhere: Vec<String>,
    /// Why the last attempt to open a session failed, shown in place of the
    /// key hints. Opening can fail for reasons the user needs to act on —
    /// a session whose directory is gone, or one deleted from under the
    /// list — and the picker is where they still have other choices.
    pub notice: Option<String>,
}

impl Picker {
    /// Every session, grouped by whether it belongs to `cwd`.
    ///
    /// The split is the useful one: a session's directory is its sandbox
    /// boundary, so the ones started here are the ones that will resume
    /// without moving you anywhere. A session with no recorded directory
    /// (written before that was tracked) sorts with the others, since it
    /// can't be claimed by this one.
    pub fn launch(all: Vec<SessionRow>, cwd: Option<&str>) -> Self {
        let (here, elsewhere): (Vec<_>, Vec<_>) = all
            .into_iter()
            .partition(|row| cwd.is_some_and(|cwd| row.working_dir.as_deref() == Some(cwd)));

        let here_ids: Vec<String> = here.iter().map(|row| row.id.clone()).collect();
        let elsewhere_ids: Vec<String> = elsewhere.iter().map(|row| row.id.clone()).collect();

        let mut items = vec![LaunchItem::NewSession];
        for (label, rows) in [(HERE_SECTION, here), ("Elsewhere", elsewhere)] {
            if rows.is_empty() {
                continue;
            }
            items.push(LaunchItem::Header(label));
            items.extend(rows.into_iter().map(LaunchItem::Resume));
        }

        Picker {
            items,
            selected: 0,
            confirming_delete: None,
            renaming: None,
            confirming_repoint: None,
            notice: None,
            here: here_ids,
            elsewhere: elsewhere_ids,
        }
    }

    /// Folds freshly-read state into the rows already on screen, reporting
    /// whether anything changed.
    ///
    /// The whole list is rebuilt, so sessions started or deleted elsewhere
    /// appear and disappear — a view you leave open to watch is not much use
    /// if it only knows the sessions that existed when you opened it.
    ///
    /// Rebuilding is why the selection is restored by session id rather than
    /// left on its row number: rows can be inserted, removed or regrouped
    /// underneath it, and an index would quietly come to rest on a different
    /// conversation.
    pub fn refresh(&mut self, latest: Vec<SessionRow>, cwd: Option<&str>) -> bool {
        let rebuilt = Picker::launch(latest, cwd);
        if rebuilt.items == self.items {
            return false;
        }

        // The selection follows the session, not the row number: rebuilding
        // can insert, remove or regroup rows, and a cursor that stayed on an
        // index would silently come to rest on a different conversation.
        let selected = self.selected_session().map(|row| row.id.clone());
        let previous = self.selected;
        self.items = rebuilt.items;
        self.here = rebuilt.here;
        self.elsewhere = rebuilt.elsewhere;

        self.selected = selected
            .and_then(|id| {
                self.items
                    .iter()
                    .position(|item| matches!(item, LaunchItem::Resume(row) if row.id == id))
            })
            // Whatever was selected is gone — deleted from another process,
            // say. Stay about where the eye was rather than jumping home.
            .unwrap_or_else(|| previous.min(self.items.len().saturating_sub(1)));
        if !self.selectable(self.selected) {
            self.move_up();
        }
        true
    }

    /// Whether any row is mid-request, which is the only reason this screen
    /// needs to redraw between refreshes.
    pub fn has_working_session(&self) -> bool {
        self.items.iter().any(|item| {
            matches!(item, LaunchItem::Resume(row) if row.last_state() == LastState::Working)
        })
    }

    /// Whether a row can hold the cursor. Headers can't.
    fn selectable(&self, index: usize) -> bool {
        !matches!(self.items.get(index), Some(LaunchItem::Header(_)) | None)
    }

    pub fn move_up(&mut self) {
        let mut index = self.selected;
        while index > 0 {
            index -= 1;
            if self.selectable(index) {
                self.selected = index;
                return;
            }
        }
    }

    pub fn move_down(&mut self) {
        let mut index = self.selected;
        while index + 1 < self.items.len() {
            index += 1;
            if self.selectable(index) {
                self.selected = index;
                return;
            }
        }
    }

    pub fn selected_session(&self) -> Option<&SessionRow> {
        match self.items.get(self.selected) {
            Some(LaunchItem::Resume(row)) => Some(row),
            _ => None,
        }
    }

    /// What the currently selected row does when chosen.
    pub fn activate(&self) -> Option<Activation> {
        match self.items.get(self.selected)? {
            LaunchItem::NewSession => Some(Activation::NewSession),
            LaunchItem::Resume(row) => Some(Activation::Resume(row.clone())),
            LaunchItem::Header(_) => None,
        }
    }

    /// Begins a delete, which then needs confirming. Only meaningful on a
    /// session row.
    pub fn begin_delete(&mut self) {
        if let Some(row) = self.selected_session() {
            self.confirming_delete = Some(row.clone());
        }
    }

    /// Offers to resume a session here when its own directory is gone.
    pub fn begin_repoint(&mut self, row: SessionRow, missing: String) {
        self.confirming_repoint = Some((row, missing));
    }

    /// Resolves a pending repoint; `true` means resume here and repoint.
    pub fn resolve_repoint(&mut self, confirmed: bool) -> Option<Activation> {
        let (row, _) = self.confirming_repoint.take()?;
        confirmed.then_some(Activation::Repoint(row))
    }

    /// Resolves a pending confirmation; `true` means go ahead.
    pub fn resolve_delete(&mut self, confirmed: bool) -> Option<Activation> {
        let row = self.confirming_delete.take()?;
        confirmed.then_some(Activation::Delete(row))
    }

    /// Drops a row after it's been deleted, keeping the selection in range.
    ///
    /// Takes the section label with it when that was the last row under it:
    /// a header with nothing beneath reads as a section that failed to load
    /// rather than one that's empty.
    pub fn remove_session(&mut self, id: &str) {
        self.items
            .retain(|item| !matches!(item, LaunchItem::Resume(row) if row.id == id));
        self.items
            .retain(|item| !matches!(item, LaunchItem::Header(_)));

        // Rebuilt rather than patched: the rows that are left already know
        // which section they belong to, so regrouping them can't drift from
        // what `launch` would have produced.
        let rows: Vec<SessionRow> = self
            .items
            .iter()
            .filter_map(|item| match item {
                LaunchItem::Resume(row) => Some(row.clone()),
                _ => None,
            })
            .collect();
        let mut items = vec![LaunchItem::NewSession];
        for (label, section) in [(HERE_SECTION, &self.here), ("Elsewhere", &self.elsewhere)] {
            let section: Vec<SessionRow> = rows
                .iter()
                .filter(|row| section.contains(&row.id))
                .cloned()
                .collect();
            if section.is_empty() {
                continue;
            }
            items.push(LaunchItem::Header(label));
            items.extend(section.into_iter().map(LaunchItem::Resume));
        }
        self.items = items;

        if self.selected >= self.items.len() {
            self.selected = self.items.len().saturating_sub(1);
        }
        if !self.selectable(self.selected) {
            self.move_up();
        }
    }

    /// Begins renaming the selected session, pre-filling the input with its
    /// current title so it can be edited rather than retyped from scratch.
    /// Only meaningful on a session row.
    pub fn begin_rename(&mut self) {
        if let Some(row) = self.selected_session() {
            self.renaming = Some((row.clone(), row.title.clone()));
        }
    }

    pub fn rename_insert_char(&mut self, c: char) {
        if let Some((_, input)) = &mut self.renaming {
            input.push(c);
        }
    }

    pub fn rename_backspace(&mut self) {
        if let Some((_, input)) = &mut self.renaming {
            input.pop();
        }
    }

    /// Cancels an in-progress rename without saving anything.
    pub fn cancel_rename(&mut self) {
        self.renaming = None;
    }

    /// Confirms the rename, returning the session id and its new title to
    /// persist. A blank (post-trim) title isn't meaningful, so it's
    /// rejected rather than saved — the rename stays open for another try
    /// instead of silently discarding it on a stray Enter.
    pub fn confirm_rename(&mut self) -> Option<(String, String)> {
        let (row, input) = self.renaming.as_ref()?;
        let title = input.trim().to_string();
        if title.is_empty() {
            return None;
        }
        let id = row.id.clone();
        self.renaming = None;
        Some((id, title))
    }

    /// Reflects a persisted rename in the row itself, so the list shows it
    /// without a reload.
    pub fn apply_rename(&mut self, id: &str, title: String) {
        for item in &mut self.items {
            if let LaunchItem::Resume(row) = item {
                if row.id == id {
                    row.title = title;
                    return;
                }
            }
        }
    }
}

/// What a session row reports, whichever source can answer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LastState {
    /// A request is in flight right now.
    Working,
    /// Blocked on an approval nobody has answered.
    AwaitingApproval,
    /// The last turn ended in an error.
    Failed,
    /// Created, never used.
    New,
    /// The model answered. A turn that ran to completion.
    Replied,
    /// A message was sent and nothing came back — the turn is either running
    /// somewhere right now or it ended badly. Nothing on disk tells the two
    /// apart, because a turn's messages are only written when it finishes.
    NoReply,
    /// It stopped part-way through working: after a tool result with no
    /// answer after it, or on a tool call that never ran.
    Interrupted,
}

impl SessionRow {
    /// What to show for this session.
    ///
    /// The process's own word wins when it gave one: it knows things the
    /// stored messages can't say, like that a request is in flight or that
    /// somebody is being asked a question. Otherwise the last message
    /// speaks, which is all a session nobody is running can offer.
    pub fn last_state(&self) -> LastState {
        match self.activity {
            Some(Activity::Working) => return LastState::Working,
            Some(Activity::AwaitingApproval) => return LastState::AwaitingApproval,
            Some(Activity::Failed) => return LastState::Failed,
            None => {}
        }
        let Some(last) = &self.last else {
            return LastState::New;
        };
        match last.role.as_str() {
            "assistant" if last.has_tool_calls => LastState::Interrupted,
            "assistant" => LastState::Replied,
            "tool" => LastState::Interrupted,
            // A user message with nothing after it.
            _ => LastState::NoReply,
        }
    }
}

impl SessionRow {
    /// The line to show after the state, when there is one worth showing.
    ///
    /// A pending approval displaces the conversation preview: what the
    /// session last said matters far less than what it is stuck asking, and
    /// that is the row someone reading this list needs to act on.
    pub fn preview(&self) -> Option<String> {
        if self.activity == Some(Activity::AwaitingApproval) {
            if let Some(detail) = &self.activity_detail {
                return Some(format!("needs approval — {detail}"));
            }
        }
        self.last
            .as_ref()
            .map(|last| last.preview.clone())
            .filter(|preview| !preview.is_empty())
    }
}

/// The glyph and colour for a session's state.
///
/// The glyph carries the meaning and the colour only reinforces it, so the
/// list still reads on a terminal without colour.
///
/// Every glyph here is East Asian Width *Neutral* or *Narrow*, which is what
/// keeps the column aligned. The obvious circles — `●`, `◐`, `○`, `•` — are
/// *Ambiguous*, and a terminal may draw those two cells wide while drawing
/// the rest one, which pushed some rows a column right of the others.
fn state_badge(state: LastState, tick: usize) -> (String, Style) {
    let (glyph, style) = match state {
        LastState::New => (" ", Style::new()),
        // The conversation's own spinner, frame for frame and in the same
        // yellow, so a busy session animates identically whether you're
        // watching it from the list or sitting inside it.
        LastState::Working => (
            super::render::FRAMES[tick % super::render::FRAMES.len()],
            Style::new().yellow(),
        ),
        LastState::AwaitingApproval => ("?", Style::new().yellow().bold()),
        LastState::Failed => ("✗", Style::new().red().bold()),
        LastState::Replied => ("✓", Style::new().green()),
        LastState::NoReply => ("⋯", Style::new().cyan()),
        LastState::Interrupted => ("⚑", Style::new().yellow()),
    };
    (format!("{glyph:<BADGE_WIDTH$}"), style)
}

/// The label whose rows share the directory the process is in, so they
/// needn't each repeat it.
const HERE_SECTION: &str = "In this directory";

// Fixed columns, so the preview can be given whatever the line has left.
const MARKER_WIDTH: usize = 2 + ICON_WIDTH + 1 + BADGE_WIDTH + 1; // marker, mark, badge, gaps
const KIND_WIDTH: usize = 7;
const TITLE_WIDTH: usize = 24;
const DIR_WIDTH: usize = 24;
const WHEN_WIDTH: usize = 8;

/// Below this a preview says too little to be worth the clutter.
const MIN_PREVIEW: usize = 12;
/// Every badge is a single cell — see `state_badge` — and padded to this so
/// the column stays straight.
const BADGE_WIDTH: usize = 1;

/// The mark is two braille cells: 4 dots across by 4 down. One cell is 2
/// dots wide by 4 tall in a character box about half as wide as it is tall,
/// so the dots already sit on a square lattice — it's the glyph that's a
/// 1:2 rectangle. Two of them side by side makes the block square as well.
const ICON_WIDTH: usize = 2;

pub fn draw(
    frame: &mut Frame,
    picker: &Picker,
    title: &str,
    dir: Option<&str>,
    hint: &str,
    tick: usize,
) {
    let areas = Layout::vertical([
        Constraint::Length(1), // title
        Constraint::Length(1), // rule
        Constraint::Min(1),    // list
        Constraint::Length(1), // footer/hint
    ])
    .split(frame.area());

    let mut lines: Vec<Line> = Vec::new();
    // Tracks which section the rows being drawn belong to. Under "In this
    // directory" every row has the same path, so repeating it on each is
    // noise — and it's the column that was squeezing the preview off the
    // end of the line.
    let mut in_current_dir = false;
    // Whether this section has already named its directory on an earlier row.
    let mut here_dir_shown = false;
    let width = areas[2].width as usize;
    for (index, item) in picker.items.iter().enumerate() {
        let selected = index == picker.selected;
        let marker = if selected { "❯ " } else { "  " };
        let base = if selected {
            Style::new().bold()
        } else {
            Style::new()
        };

        let mut spans = vec![Span::styled(marker, Style::new().cyan().bold())];
        match item {
            LaunchItem::NewSession => {
                spans.push(Span::styled("New session", base.green()));
            }
            LaunchItem::Header(label) => {
                in_current_dir = *label == HERE_SECTION;
                here_dir_shown = false;
                // A blank line before every section label, which puts one
                // under "New session" and one between the two groups —
                // enough to read the screen as three lists rather than one
                // long one.
                lines.push(Line::raw(""));
                // Replaces its own marker: a section label isn't a choice,
                // so it shouldn't look like one the cursor skipped.
                spans.clear();
                spans.push(Span::styled(
                    format!("  {label}"),
                    Style::new().dark_gray().italic(),
                ));
            }
            LaunchItem::Resume(row) => {
                // Identity first, then state: what the session is, then what
                // it's doing.
                let (mark, mark_style) = identicon(&row.id);
                spans.push(Span::styled(mark, mark_style));
                spans.push(Span::raw(" "));

                let (glyph, style) = state_badge(row.last_state(), tick);
                spans.push(Span::styled(format!("{glyph} "), style));
                spans.push(Span::styled(
                    column(mode_label(row.is_agentic()), KIND_WIDTH),
                    if row.is_agentic() {
                        Style::new().yellow()
                    } else {
                        Style::new().cyan()
                    },
                ));
                spans.push(Span::styled(column(&row.title, TITLE_WIDTH), base));

                // Under "In this directory" every row shares one path, so it
                // is spelled out on the first row and stood in for below.
                // The column keeps its slot either way, so what follows it
                // stays aligned down the whole section.
                let dir = match (&row.working_dir, in_current_dir && here_dir_shown) {
                    (_, true) => "…".to_string(),
                    (Some(dir), false) => home_relative(dir),
                    (None, false) => "dir not recorded".to_string(),
                };
                here_dir_shown |= in_current_dir;
                spans.push(Span::styled(
                    column(&dir, DIR_WIDTH),
                    Style::new().dark_gray(),
                ));

                spans.push(Span::styled(
                    column(&relative_time(row.updated_at), WHEN_WIDTH),
                    Style::new().dark_gray(),
                ));

                let used = MARKER_WIDTH + KIND_WIDTH + TITLE_WIDTH + DIR_WIDTH + WHEN_WIDTH;

                // Whatever is left of the line goes to what was last said,
                // so the row describes where the session got to rather than
                // only when it was touched. Dropped entirely when the
                // terminal is too narrow to say anything useful.
                if let Some(preview) = row.preview() {
                    let room = width.saturating_sub(used + 2);
                    if room >= MIN_PREVIEW {
                        spans.push(Span::styled(
                            format!("  {}", truncate(&preview, room)),
                            Style::new().dark_gray().italic(),
                        ));
                    }
                }
            }
        }
        let mut line = Line::from(spans);
        // The same band the transcript puts behind your own messages: the
        // cursor is easy to lose in a list where several rows are animating.
        if selected {
            pad_to(&mut line, width);
            line.style = line.style.patch(band());
        }
        lines.push(line);
    }

    if picker.items.is_empty() {
        lines.push(Line::from(Span::styled(
            "  no saved sessions yet",
            Style::new().dark_gray().italic(),
        )));
    }

    // A pending delete or rename takes over the hint line, so the question
    // (or the text being typed) is right where the answer goes.
    let footer = if let Some((_, input)) = &picker.renaming {
        Line::from(vec![
            Span::styled(" rename to: ", Style::new().yellow().bold()),
            Span::raw(input.clone()),
            Span::styled("▏", Style::new().yellow()),
        ])
    } else if let Some(row) = &picker.confirming_delete {
        Line::from(vec![
            Span::styled(
                format!(
                    " delete session {} ({})? ",
                    row.short_id(),
                    truncate(&row.title, 30)
                ),
                Style::new().red().bold(),
            ),
            Span::styled("y / n", Style::new().red()),
        ])
    } else if let Some((_, missing)) = &picker.confirming_repoint {
        Line::from(vec![
            Span::styled(
                format!(
                    " {} is gone — resume here instead? ",
                    home_relative(missing)
                ),
                Style::new().yellow().bold(),
            ),
            Span::styled("y / n", Style::new().yellow()),
        ])
    } else {
        Line::from(Span::styled(format!(" {hint}"), Style::new().dark_gray()))
    };

    let mut heading = vec![Span::styled(title.to_string(), Style::new().bold())];
    // The directory the list is grouped around: "In this directory" means
    // nothing without saying which.
    if let Some(dir) = dir {
        heading.push(Span::styled(
            format!("  {}", home_relative(dir)),
            Style::new().dark_gray(),
        ));
    }
    frame.render_widget(Paragraph::new(Line::from(heading)), areas[0]);
    draw_rule(frame, areas[1], None);
    frame.render_widget(Paragraph::new(Text::from(lines)), areas[2]);
    // A failed open replaces the key hints: it's the thing that just
    // happened, and the hints are still discoverable by pressing anything.
    match &picker.notice {
        Some(notice) => frame.render_widget(
            Paragraph::new(Line::from(Span::styled(
                format!("✗ {notice}"),
                Style::new().red(),
            ))),
            areas[3],
        ),
        None => frame.render_widget(Paragraph::new(footer), areas[3]),
    }
}

/// The prompt shown before a new session is created, so it starts with a
/// real name instead of "Untitled". Leaving it blank falls back to the
/// usual behavior: derived from the first message once there is one.
pub fn draw_naming(frame: &mut Frame, input: &str) {
    let areas = Layout::vertical([
        Constraint::Length(1), // title
        Constraint::Length(1), // rule
        Constraint::Min(1),    // content
        Constraint::Length(1), // hint
    ])
    .split(frame.area());

    frame.render_widget(
        Paragraph::new(Line::from(Span::styled("comms", Style::new().bold()))),
        areas[0],
    );
    draw_rule(frame, areas[1], None);

    let lines = vec![
        Line::raw(""),
        Line::from(vec![
            Span::styled("  Session title  ", Style::new().dark_gray()),
            Span::raw(input.to_string()),
            Span::styled("▏", Style::new().yellow()),
        ]),
    ];
    frame.render_widget(Paragraph::new(Text::from(lines)), areas[2]);

    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            " A title is required · Enter start · Esc cancel",
            Style::new().dark_gray(),
        ))),
        areas[3],
    );
}

/// One cell of the row grid: the text truncated to fit and padded out to
/// `width`, always leaving a two-space gutter so a full-width value can't
/// run into the column after it.
fn column(text: &str, width: usize) -> String {
    let text = truncate(text, width.saturating_sub(2));
    format!("{text:<width$}")
}

/// At most `max` characters, the ellipsis included — it replaces the last
/// character kept rather than being added past the limit, so a caller that
/// sized a column or the room left on a line gets something that fits it.
fn truncate(text: &str, max: usize) -> String {
    let flat: String = text
        .chars()
        .map(|c| if c == '\n' { ' ' } else { c })
        .collect();
    if flat.chars().count() <= max {
        return flat;
    }
    match max {
        0 => String::new(),
        _ => format!("{}…", flat.chars().take(max - 1).collect::<String>()),
    }
}

/// Coarse "how long ago", enough to tell yesterday's work from this
/// morning's without a date library.
fn relative_time(timestamp: i64) -> String {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let seconds = now.saturating_sub(timestamp);
    match seconds {
        s if s < 60 => "just now".to_string(),
        s if s < 3600 => format!("{}m ago", s / 60),
        s if s < 86_400 => format!("{}h ago", s / 3600),
        s if s < 2_592_000 => format!("{}d ago", s / 86_400),
        s => format!("{}mo ago", s / 2_592_000),
    }
}

#[cfg(test)]
mod tests {

    #[test]
    fn the_selected_row_carries_a_band_across_the_whole_row() {
        use ratatui::{backend::TestBackend, Terminal};
        let picker = picker_of(vec![
            row_in("00000001", KIND_AGENT_CHAT, "first", Some(HERE)),
            row_in("00000002", KIND_AGENT_CHAT, "second", Some(HERE)),
        ]);
        let mut terminal = Terminal::new(TestBackend::new(70, 10)).unwrap();
        terminal
            .draw(|f| draw(f, &picker, "COMMS", None, "hint", 0))
            .unwrap();
        let buffer = terminal.backend().buffer().clone();

        let row_of = |needle: &str| {
            (0..buffer.area.height)
                .find(|y| {
                    (0..buffer.area.width)
                        .map(|x| buffer[(x, *y)].symbol().to_string())
                        .collect::<String>()
                        .contains(needle)
                })
                .unwrap_or_else(|| panic!("{needle} not drawn"))
        };

        // "New session" is selected by default.
        let selected = row_of("New session");
        let other = row_of("first");
        assert_eq!(buffer[(65, selected)].style().bg, band().bg);
        assert_ne!(buffer[(65, other)].style().bg, band().bg);
    }

    #[test]
    fn the_kind_column_says_ask_not_chat() {
        // The stored kind is "chat"; the word everywhere a user reads it is
        // "ask", matching /ask and /agent.
        let ask = row("00000001", crate::store::KIND_CHAT, "t");
        let agent = row("00000002", KIND_AGENT_CHAT, "t");
        assert_eq!(
            column(mode_label(ask.is_agentic()), KIND_WIDTH).trim_end(),
            "ask"
        );
        assert_eq!(
            column(mode_label(agent.is_agentic()), KIND_WIDTH).trim_end(),
            "agent"
        );
    }

    #[test]
    fn truncate_never_exceeds_its_limit() {
        // The ellipsis takes the place of a kept character. Returning max + 1
        // used to be enough to wrap a row whose preview was sized to the
        // space left on the line.
        assert_eq!(truncate("abcdefgh", 4).chars().count(), 4);
        assert_eq!(truncate("abcdefgh", 4), "abc…");
        assert_eq!(truncate("abcd", 4), "abcd");
        assert_eq!(truncate("abc", 4), "abc");
        assert_eq!(truncate("abc", 1), "…");
        assert_eq!(truncate("abc", 0), "");
    }

    #[test]
    fn truncate_counts_characters_not_bytes() {
        assert_eq!(truncate("ünïcödé test", 6).chars().count(), 6);
    }

    #[test]
    fn truncate_flattens_newlines() {
        assert_eq!(truncate("two\nlines", 20), "two lines");
    }

    #[test]
    fn column_pads_short_values_and_keeps_a_gutter() {
        assert_eq!(column("chat", 7), "chat   ");
        // A value wider than its column still can't touch the next one.
        let cell = column("~/code/some/very/long/path", 24);
        assert_eq!(cell.chars().count(), 24);
        assert!(cell.ends_with("  "), "no gutter left in {cell:?}");
    }
    #[test]
    fn a_notice_replaces_the_key_hints() {
        // A session that can't be opened has to say so where the user still
        // has other choices, rather than taking the whole TUI down.
        let mut picker = picker_of(vec![]);
        assert!(picker.notice.is_none());
        picker.notice = Some("abc123 was started in /gone, which no longer exists.".to_string());
        assert!(picker.notice.as_deref().unwrap().contains("/gone"));
    }

    use super::*;

    const HERE: &str = "/work/project";

    fn row(id: &str, kind: &str, title: &str) -> SessionRow {
        row_in(id, kind, title, None)
    }

    fn row_in(id: &str, kind: &str, title: &str, dir: Option<&str>) -> SessionRow {
        SessionRow {
            id: format!("{id}-0000-0000-0000-000000000000"),
            kind: kind.to_string(),
            title: title.to_string(),
            updated_at: 0,
            working_dir: dir.map(str::to_string),
            last: None,
            activity: None,
            activity_detail: None,
        }
    }

    fn with_last(mut row: SessionRow, role: &str, tool_calls: bool, preview: &str) -> SessionRow {
        row.last = Some(LastMessage {
            role: role.to_string(),
            has_tool_calls: tool_calls,
            preview: preview.to_string(),
        });
        row
    }

    /// A picker over `rows`, as seen from `HERE`.
    fn picker_of(rows: Vec<SessionRow>) -> Picker {
        Picker::launch(rows, Some(HERE))
    }

    #[test]
    fn sessions_are_grouped_by_whether_they_belong_here() {
        let picker = picker_of(vec![
            row_in("00000001", "chat", "here", Some(HERE)),
            row_in("00000002", "chat", "away", Some("/somewhere/else")),
            row_in("00000003", "chat", "unrecorded", None),
        ]);

        let shape: Vec<String> = picker
            .items
            .iter()
            .map(|item| match item {
                LaunchItem::NewSession => "new".to_string(),
                LaunchItem::Header(label) => format!("[{label}]"),
                LaunchItem::Resume(row) => row.title.clone(),
            })
            .collect();
        assert_eq!(
            shape,
            vec![
                "new",
                "[In this directory]",
                "here",
                "[Elsewhere]",
                "away",
                // No recorded directory can't be claimed by this one.
                "unrecorded",
            ]
        );
    }

    #[test]
    fn with_no_current_directory_nothing_is_claimed_as_here() {
        // `current_dir` can fail — a deleted cwd, a permissions problem — and
        // the list still has to render. Claiming sessions as "here" on a
        // directory we couldn't read would be worse than grouping them all
        // as elsewhere.
        let picker = Picker::launch(
            vec![row_in(
                "00000001",
                "chat",
                "somewhere",
                Some("/work/project"),
            )],
            None,
        );
        let headers: Vec<&str> = picker
            .items
            .iter()
            .filter_map(|item| match item {
                LaunchItem::Header(label) => Some(*label),
                _ => None,
            })
            .collect();
        assert_eq!(headers, vec!["Elsewhere"]);
    }

    #[test]
    fn a_section_with_nothing_in_it_gets_no_header() {
        let picker = picker_of(vec![row_in("00000001", "chat", "away", Some("/elsewhere"))]);
        let headers: Vec<&str> = picker
            .items
            .iter()
            .filter_map(|item| match item {
                LaunchItem::Header(label) => Some(*label),
                _ => None,
            })
            .collect();
        assert_eq!(headers, vec!["Elsewhere"]);
    }

    #[test]
    fn the_cursor_steps_over_headers() {
        let mut picker = picker_of(vec![
            row_in("00000001", "chat", "here", Some(HERE)),
            row_in("00000002", "chat", "away", Some("/elsewhere")),
        ]);
        // new → (skip header) → here → (skip header) → away
        assert!(matches!(picker.activate(), Some(Activation::NewSession)));
        picker.move_down();
        assert_eq!(picker.selected_session().unwrap().title, "here");
        picker.move_down();
        assert_eq!(picker.selected_session().unwrap().title, "away");
        // ...and back up the same way, never landing on a label.
        picker.move_up();
        assert_eq!(picker.selected_session().unwrap().title, "here");
        picker.move_up();
        assert!(matches!(picker.activate(), Some(Activation::NewSession)));
    }

    #[test]
    fn the_last_message_says_where_a_session_stopped() {
        let base = || row_in("00000001", "chat", "t", Some(HERE));

        // Never used.
        assert_eq!(base().last_state(), LastState::New);
        // Ran to completion.
        assert_eq!(
            with_last(base(), "assistant", false, "here you go").last_state(),
            LastState::Replied
        );
        // Sent, nothing came back — running elsewhere, or it failed. The
        // messages table can't tell those apart, and this doesn't pretend to.
        assert_eq!(
            with_last(base(), "user", false, "do the thing").last_state(),
            LastState::NoReply
        );
        // Stopped part-way: a tool result with no answer after it...
        assert_eq!(
            with_last(base(), "tool", false, "{}").last_state(),
            LastState::Interrupted
        );
        // ...or a tool call that never ran.
        assert_eq!(
            with_last(base(), "assistant", true, "read_file").last_state(),
            LastState::Interrupted
        );
    }

    #[test]
    fn a_running_process_speaks_over_the_stored_messages() {
        // The whole reason for the column: from the messages alone, a
        // request in flight and a turn that failed are the same row.
        let sent = with_last(
            row_in("00000001", "chat", "t", Some(HERE)),
            "user",
            false,
            "do the thing",
        );
        assert_eq!(sent.last_state(), LastState::NoReply);

        let mut running = sent.clone();
        running.activity = Some(Activity::Working);
        assert_eq!(running.last_state(), LastState::Working);

        let mut asking = sent.clone();
        asking.activity = Some(Activity::AwaitingApproval);
        assert_eq!(asking.last_state(), LastState::AwaitingApproval);

        let mut broken = sent.clone();
        broken.activity = Some(Activity::Failed);
        assert_eq!(broken.last_state(), LastState::Failed);
    }

    #[test]
    fn with_nothing_said_the_messages_still_speak() {
        // No process running it, so the column is empty and the row falls
        // back to what was stored — which is the common case.
        let answered = with_last(
            row_in("00000001", "chat", "t", Some(HERE)),
            "assistant",
            false,
            "here you go",
        );
        assert_eq!(answered.activity, None);
        assert_eq!(answered.last_state(), LastState::Replied);
    }

    #[test]
    fn the_working_badge_animates_and_stays_one_cell() {
        // It borrows the conversation's own spinner frames, so a busy
        // session looks the same from the list as from inside it — and every
        // frame still has to fit the column the other badges share.
        let frames: Vec<String> = (0..super::super::render::FRAMES.len())
            .map(|tick| state_badge(LastState::Working, tick).0)
            .collect();

        let mut distinct = frames.clone();
        distinct.sort();
        distinct.dedup();
        assert!(distinct.len() > 1, "it has to actually move: {frames:?}");
        for frame in &frames {
            assert_eq!(
                frame.chars().count(),
                BADGE_WIDTH,
                "{frame:?} is not one cell"
            );
        }
    }

    #[test]
    fn a_still_list_has_nothing_to_animate() {
        // The picker redraws itself while this is true, so it must only be
        // true when something is actually running.
        let idle = picker_of(vec![row_in("00000001", "chat", "one", Some(HERE))]);
        assert!(!idle.has_working_session());

        let mut busy_row = row_in("00000002", "chat", "two", Some(HERE));
        busy_row.activity = Some(Activity::Working);
        assert!(picker_of(vec![busy_row]).has_working_session());
    }

    #[test]
    fn a_pending_approval_displaces_the_conversation_preview() {
        // What the session last said matters far less than what it is stuck
        // asking — that's the row someone watching this list has to act on.
        let mut row = with_last(
            row_in("00000001", "agent_chat", "t", Some(HERE)),
            "user",
            false,
            "please tidy the build",
        );
        assert_eq!(row.preview().as_deref(), Some("please tidy the build"));

        row.activity = Some(Activity::AwaitingApproval);
        row.activity_detail = Some("run_terminal_command: rm -rf build".to_string());
        assert_eq!(
            row.preview().as_deref(),
            Some("needs approval — run_terminal_command: rm -rf build")
        );
        assert_eq!(row.last_state(), LastState::AwaitingApproval);
    }

    #[test]
    fn an_approval_with_no_detail_still_falls_back_to_the_messages() {
        // Written by an older version, or a tool whose arguments say nothing
        // worth naming.
        let mut row = with_last(
            row_in("00000001", "agent_chat", "t", Some(HERE)),
            "user",
            false,
            "please tidy the build",
        );
        row.activity = Some(Activity::AwaitingApproval);
        assert_eq!(row.preview().as_deref(), Some("please tidy the build"));
    }

    #[test]
    fn no_glyph_is_ambiguous_width() {
        // What kept the column ragged: a terminal may draw an
        // East-Asian-Ambiguous character two cells wide while drawing the
        // rest one. Every badge has to be a character that is one cell
        // everywhere — which rules out the obvious circles.
        const AMBIGUOUS: [char; 7] = ['●', '◐', '○', '•', '…', '→', '⊙'];
        for state in [
            LastState::New,
            LastState::Replied,
            LastState::NoReply,
            LastState::Interrupted,
            LastState::Working,
            LastState::AwaitingApproval,
            LastState::Failed,
        ] {
            // The badge is padded to a fixed width; the character itself is
            // what has to be one cell.
            let glyph = state_badge(state, 0).0;
            let ch = glyph.trim().chars().next().unwrap_or(' ');
            assert_eq!(
                glyph.trim().chars().count().max(1),
                1,
                "{state:?} is not a single char"
            );
            assert!(
                !AMBIGUOUS.contains(&ch),
                "{state:?} uses {ch:?}, which some terminals draw double-width"
            );
        }
    }

    #[test]
    fn every_state_has_its_own_glyph() {
        // On a terminal without colour the glyph is all there is, so no two
        // states may share one.
        let glyphs: Vec<String> = [
            LastState::New,
            LastState::Replied,
            LastState::NoReply,
            LastState::Interrupted,
            LastState::Working,
            LastState::AwaitingApproval,
            LastState::Failed,
        ]
        .into_iter()
        .map(|state| state_badge(state, 0).0)
        .collect();
        let mut unique = glyphs.clone();
        unique.sort_unstable();
        unique.dedup();
        assert_eq!(unique.len(), glyphs.len(), "{glyphs:?}");
    }

    #[test]
    fn refreshing_updates_state_without_disturbing_the_list() {
        let mut picker = picker_of(vec![
            row_in("00000001", "chat", "one", Some(HERE)),
            row_in("00000002", "chat", "two", Some(HERE)),
        ]);
        picker.move_down();
        let selected_before = picker.selected;
        let order_before: Vec<String> = picker
            .items
            .iter()
            .filter_map(|item| match item {
                LaunchItem::Resume(row) => Some(row.id.clone()),
                _ => None,
            })
            .collect();

        // Both rows come back — a refresh rebuilds the list, so anything
        // left out of `latest` is a session that was deleted.
        let moved_on = with_last(
            row_in("00000002", "chat", "two", Some(HERE)),
            "assistant",
            false,
            "all done",
        );
        assert!(picker.refresh(
            vec![row_in("00000001", "chat", "one", Some(HERE)), moved_on],
            Some(HERE)
        ));

        let states: Vec<LastState> = picker
            .items
            .iter()
            .filter_map(|item| match item {
                LaunchItem::Resume(row) => Some(row.last_state()),
                _ => None,
            })
            .collect();
        assert_eq!(states, vec![LastState::New, LastState::Replied]);

        // Nothing moved under the cursor.
        assert_eq!(picker.selected, selected_before);
        let order_after: Vec<String> = picker
            .items
            .iter()
            .filter_map(|item| match item {
                LaunchItem::Resume(row) => Some(row.id.clone()),
                _ => None,
            })
            .collect();
        assert_eq!(order_before, order_after);
    }

    #[test]
    fn refreshing_an_unchanged_list_reports_nothing() {
        // The caller redraws on `true`, so a quiet list must stay quiet
        // rather than repainting every couple of seconds.
        let mut picker = picker_of(vec![row_in("00000001", "chat", "one", Some(HERE))]);
        let same = row_in("00000001", "chat", "one", Some(HERE));
        assert!(!picker.refresh(vec![same], Some(HERE)));
    }

    #[test]
    fn refreshing_picks_up_sessions_added_and_removed_elsewhere() {
        // The list is worth leaving open only if it keeps up with what other
        // processes are doing to it.
        let mut picker = picker_of(vec![
            row_in("00000001", "chat", "one", Some(HERE)),
            row_in("00000002", "chat", "two", Some(HERE)),
        ]);
        picker.move_down();
        picker.move_down();
        let watched = picker.selected_session().unwrap().id.clone();

        // "one" was deleted elsewhere, "three" was started elsewhere.
        assert!(picker.refresh(
            vec![
                row_in("00000002", "chat", "two", Some(HERE)),
                row_in("00000003", "chat", "three", Some(HERE)),
            ],
            Some(HERE)
        ));

        let titles: Vec<String> = picker
            .items
            .iter()
            .filter_map(|item| match item {
                LaunchItem::Resume(row) => Some(row.title.clone()),
                _ => None,
            })
            .collect();
        assert_eq!(titles, vec!["two", "three"]);

        // The cursor followed the session it was on, not the row number it
        // happened to sit at.
        assert_eq!(picker.selected_session().unwrap().id, watched);
    }

    #[test]
    fn losing_the_selected_session_leaves_the_cursor_somewhere_sensible() {
        let mut picker = picker_of(vec![
            row_in("00000001", "chat", "one", Some(HERE)),
            row_in("00000002", "chat", "two", Some(HERE)),
        ]);
        picker.move_down();
        picker.move_down();

        // The one being watched is deleted from under it.
        picker.refresh(
            vec![row_in("00000001", "chat", "one", Some(HERE))],
            Some(HERE),
        );

        assert!(picker.selected < picker.items.len());
        assert!(
            !matches!(picker.items[picker.selected], LaunchItem::Header(_)),
            "never left resting on a label"
        );
    }

    #[test]
    fn a_missing_directory_offers_to_repoint() {
        // The row can't open where it says it lives, and resuming here is a
        // real answer — so it's a question, not a refusal.
        let mut picker = picker_of(vec![row_in("00000001", "chat", "t", Some("/gone"))]);
        picker.move_down();
        let row = picker.selected_session().unwrap().clone();

        picker.begin_repoint(row, "/gone".to_string());
        assert!(picker.confirming_repoint.is_some());
        // Declining leaves the session pointed where it was.
        assert!(picker.resolve_repoint(false).is_none());
        assert!(picker.confirming_repoint.is_none());

        let row = picker.selected_session().unwrap().clone();
        picker.begin_repoint(row, "/gone".to_string());
        assert!(matches!(
            picker.resolve_repoint(true),
            Some(Activation::Repoint(_))
        ));
    }

    #[test]
    fn selection_stays_in_bounds() {
        let mut picker = picker_of(vec![row("abcd1234", "chat", "t")]);
        picker.move_up();
        assert_eq!(picker.selected, 0);
        for _ in 0..50 {
            picker.move_down();
        }
        assert_eq!(picker.selected, picker.items.len() - 1);
    }

    #[test]
    fn activating_each_row_type() {
        let mut picker = picker_of(vec![row("abcd1234", "agent_chat", "t")]);
        assert!(matches!(picker.activate(), Some(Activation::NewSession)));
        picker.move_down();
        match picker.activate() {
            Some(Activation::Resume(r)) => assert!(r.is_agentic()),
            other => panic!("expected Resume, got {other:?}"),
        }
    }

    #[test]
    fn with_no_sessions_there_is_still_something_to_start() {
        // A fresh install: one row, and it's the useful one.
        let picker = picker_of(vec![]);
        assert!(matches!(picker.activate(), Some(Activation::NewSession)));
        assert!(picker.selected_session().is_none());
    }

    #[test]
    fn delete_requires_confirmation() {
        let mut picker = picker_of(vec![row("abcd1234", "chat", "t")]);
        picker.move_down();
        picker.begin_delete();
        assert!(picker.confirming_delete.is_some());
        // Declining leaves the session alone.
        assert!(picker.resolve_delete(false).is_none());
        assert!(picker.confirming_delete.is_none());

        picker.begin_delete();
        assert!(matches!(
            picker.resolve_delete(true),
            Some(Activation::Delete(_))
        ));
    }

    #[test]
    fn delete_is_a_no_op_on_a_non_session_row() {
        // "New chat" is selected; there's nothing to delete.
        let mut picker = picker_of(vec![row("abcd1234", "chat", "t")]);
        picker.begin_delete();
        assert!(picker.confirming_delete.is_none());
    }

    #[test]
    fn rename_is_prefilled_and_editable() {
        let mut picker = picker_of(vec![row("abcd1234", "chat", "old title")]);
        picker.move_down();
        picker.begin_rename();
        let (row, input) = picker.renaming.as_ref().unwrap();
        assert_eq!(row.id, picker.selected_session().unwrap().id);
        assert_eq!(input, "old title");

        picker.rename_backspace();
        picker.rename_backspace();
        for c in "le".chars() {
            picker.rename_insert_char(c);
        }
        assert_eq!(picker.renaming.as_ref().unwrap().1, "old title");
    }

    #[test]
    fn confirming_a_rename_updates_the_row_and_clears_the_state() {
        let mut picker = picker_of(vec![row("abcd1234", "chat", "old title")]);
        picker.move_down();
        let id = picker.selected_session().unwrap().id.clone();
        picker.begin_rename();
        for c in " v2".chars() {
            picker.rename_insert_char(c);
        }

        let confirmed = picker.confirm_rename();
        assert_eq!(confirmed, Some((id.clone(), "old title v2".to_string())));
        assert!(picker.renaming.is_none());

        picker.apply_rename(&id, "old title v2".to_string());
        assert_eq!(picker.selected_session().unwrap().title, "old title v2");
    }

    #[test]
    fn a_blank_rename_is_rejected_and_stays_open() {
        let mut picker = picker_of(vec![row("abcd1234", "chat", "t")]);
        picker.move_down();
        picker.begin_rename();
        picker.rename_backspace();
        assert_eq!(picker.confirm_rename(), None);
        // Still editable, not silently dropped.
        assert!(picker.renaming.is_some());
    }

    #[test]
    fn cancelling_a_rename_leaves_the_title_alone() {
        let mut picker = picker_of(vec![row("abcd1234", "chat", "old title")]);
        picker.move_down();
        picker.begin_rename();
        picker.rename_insert_char('!');
        picker.cancel_rename();
        assert!(picker.renaming.is_none());
        assert_eq!(picker.selected_session().unwrap().title, "old title");
    }

    #[test]
    fn rename_is_a_no_op_on_a_non_session_row() {
        let mut picker = picker_of(vec![row("abcd1234", "chat", "t")]);
        picker.begin_rename();
        assert!(picker.renaming.is_none());
    }

    #[test]
    fn removing_a_row_keeps_the_selection_valid() {
        let rows = vec![
            row("aaaaaaaa", "chat", "one"),
            row("bbbbbbbb", "chat", "two"),
        ];
        let mut picker = picker_of(rows);
        picker.move_down();
        let id = picker.selected_session().unwrap().id.clone();
        picker.remove_session(&id);

        let remaining: Vec<&SessionRow> = picker
            .items
            .iter()
            .filter_map(|item| match item {
                LaunchItem::Resume(row) => Some(row),
                _ => None,
            })
            .collect();
        assert_eq!(remaining.len(), 1);
        assert!(picker.selected < picker.items.len());
        // Never left sitting on a label.
        assert!(!matches!(
            picker.items[picker.selected],
            LaunchItem::Header(_)
        ));
    }

    #[test]
    fn deleting_the_last_row_of_a_section_takes_its_header_too() {
        // A header with nothing under it reads as a section that failed to
        // load, not one that's empty.
        let mut picker = picker_of(vec![
            row_in("00000001", "chat", "here", Some(HERE)),
            row_in("00000002", "chat", "away", Some("/elsewhere")),
        ]);
        let away = picker
            .items
            .iter()
            .find_map(|item| match item {
                LaunchItem::Resume(row) if row.title == "away" => Some(row.id.clone()),
                _ => None,
            })
            .unwrap();

        picker.remove_session(&away);

        let headers: Vec<&str> = picker
            .items
            .iter()
            .filter_map(|item| match item {
                LaunchItem::Header(label) => Some(*label),
                _ => None,
            })
            .collect();
        assert_eq!(headers, vec!["In this directory"]);
    }

    #[test]
    fn relative_time_buckets() {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64;
        assert_eq!(relative_time(now), "just now");
        assert_eq!(relative_time(now - 120), "2m ago");
        assert_eq!(relative_time(now - 7200), "2h ago");
        assert_eq!(relative_time(now - 172_800), "2d ago");
    }
}
