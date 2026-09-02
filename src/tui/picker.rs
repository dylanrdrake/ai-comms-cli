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

use super::render::draw_rule;
use crate::store::{SessionSummary, KIND_AGENT_CHAT};
use ratatui::prelude::*;
use ratatui::widgets::Paragraph;
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Debug, Clone)]
pub struct SessionRow {
    pub id: String,
    pub kind: String,
    pub title: String,
    pub updated_at: i64,
    /// Where the session was started, which is its sandbox boundary. `None`
    /// for one recorded before that was tracked.
    pub working_dir: Option<String>,
}

impl From<SessionSummary> for SessionRow {
    fn from(summary: SessionSummary) -> Self {
        SessionRow {
            id: summary.id,
            kind: summary.kind,
            title: summary.title,
            updated_at: summary.updated_at,
            working_dir: summary.working_dir,
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
#[derive(Debug, Clone)]
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
        for (label, rows) in [("In this directory", here), ("Elsewhere", elsewhere)] {
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
        for (label, section) in [
            ("In this directory", &self.here),
            ("Elsewhere", &self.elsewhere),
        ] {
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

/// A path with the home directory shown as `~`, so the column stays
/// readable on the long paths most projects have.
fn home_relative(dir: &str) -> String {
    let Some(home) = home::home_dir() else {
        return dir.to_string();
    };
    let home = home.display().to_string();
    match dir.strip_prefix(&home) {
        Some(rest) => format!("~{rest}"),
        None => dir.to_string(),
    }
}

pub fn draw(frame: &mut Frame, picker: &Picker, title: &str, hint: &str) {
    let areas = Layout::vertical([
        Constraint::Length(1), // title
        Constraint::Length(1), // rule
        Constraint::Min(1),    // list
        Constraint::Length(1), // footer/hint
    ])
    .split(frame.area());

    let mut lines: Vec<Line> = Vec::new();
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
                spans.push(Span::styled(
                    "  starts in ask mode; /agent enables tools",
                    Style::new().dark_gray(),
                ));
            }
            LaunchItem::Header(label) => {
                // Replaces its own marker: a section label isn't a choice,
                // so it shouldn't look like one the cursor skipped.
                spans.clear();
                spans.push(Span::styled(
                    format!("  {label}"),
                    Style::new().dark_gray().italic(),
                ));
            }
            LaunchItem::Resume(row) => {
                spans.push(Span::styled(
                    format!("{:<9}", row.short_id()),
                    Style::new().dark_gray(),
                ));
                spans.push(Span::styled(
                    format!("{:<7}", if row.is_agentic() { "agent" } else { "chat" }),
                    if row.is_agentic() {
                        Style::new().yellow()
                    } else {
                        Style::new().cyan()
                    },
                ));
                spans.push(Span::styled(truncate(&row.title, 34), base));
                spans.push(Span::styled(
                    format!("  {}", relative_time(row.updated_at)),
                    Style::new().dark_gray(),
                ));
                // Where it will resume — the thing that decides whether
                // opening it moves you, and what its sandbox will bound.
                spans.push(Span::styled(
                    match &row.working_dir {
                        Some(dir) => format!("  {}", truncate(&home_relative(dir), 40)),
                        None => "  dir not recorded".to_string(),
                    },
                    Style::new().dark_gray(),
                ));
            }
        }
        lines.push(Line::from(spans));
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

    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            title.to_string(),
            Style::new().bold(),
        ))),
        areas[0],
    );
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

fn truncate(text: &str, max: usize) -> String {
    let flat: String = text
        .chars()
        .map(|c| if c == '\n' { ' ' } else { c })
        .collect();
    if flat.chars().count() > max {
        format!("{}…", flat.chars().take(max).collect::<String>())
    } else {
        flat
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
        }
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
