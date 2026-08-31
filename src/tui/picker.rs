//! The launch screen and the sessions browser.
//!
//! Both are a selectable list over the same state, so they share one
//! implementation and differ only in what they list and what activating a
//! row does. Kept free of I/O: the caller loads sessions and acts on the
//! [`Activation`] returned when a row is chosen.

use crate::store::{SessionSummary, KIND_AGENT_CHAT};
use ratatui::prelude::*;
use ratatui::widgets::{Block, Borders, Paragraph};
use std::time::{SystemTime, UNIX_EPOCH};

/// How many recent sessions the launch screen offers directly, before
/// sending you to the full browser.
pub const RECENT_LIMIT: usize = 6;

#[derive(Debug, Clone)]
pub struct SessionRow {
    pub id: String,
    pub kind: String,
    pub title: String,
    pub updated_at: i64,
}

impl From<SessionSummary> for SessionRow {
    fn from(summary: SessionSummary) -> Self {
        SessionRow {
            id: summary.id,
            kind: summary.kind,
            title: summary.title,
            updated_at: summary.updated_at,
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
    NewChat,
    NewAgentChat,
    Resume(SessionRow),
    BrowseAll,
}

/// What choosing a row means to the caller.
#[derive(Debug, Clone)]
pub enum Activation {
    NewChat,
    NewAgentChat,
    Resume(SessionRow),
    BrowseAll,
    Delete(SessionRow),
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
}

impl Picker {
    /// The launch screen: start something new, jump back into recent work,
    /// or go browse everything.
    pub fn launch(recent: Vec<SessionRow>) -> Self {
        let mut items = vec![LaunchItem::NewChat, LaunchItem::NewAgentChat];
        items.extend(
            recent
                .into_iter()
                .take(RECENT_LIMIT)
                .map(LaunchItem::Resume),
        );
        items.push(LaunchItem::BrowseAll);
        Picker {
            items,
            selected: 0,
            confirming_delete: None,
        }
    }

    /// The sessions browser: every saved session, both kinds.
    pub fn sessions(all: Vec<SessionRow>) -> Self {
        Picker {
            items: all.into_iter().map(LaunchItem::Resume).collect(),
            selected: 0,
            confirming_delete: None,
        }
    }

    pub fn move_up(&mut self) {
        if self.selected > 0 {
            self.selected -= 1;
        }
    }

    pub fn move_down(&mut self) {
        if self.selected + 1 < self.items.len() {
            self.selected += 1;
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
            LaunchItem::NewChat => Some(Activation::NewChat),
            LaunchItem::NewAgentChat => Some(Activation::NewAgentChat),
            LaunchItem::BrowseAll => Some(Activation::BrowseAll),
            LaunchItem::Resume(row) => Some(Activation::Resume(row.clone())),
        }
    }

    /// Begins a delete, which then needs confirming. Only meaningful on a
    /// session row.
    pub fn begin_delete(&mut self) {
        if let Some(row) = self.selected_session() {
            self.confirming_delete = Some(row.clone());
        }
    }

    /// Resolves a pending confirmation; `true` means go ahead.
    pub fn resolve_delete(&mut self, confirmed: bool) -> Option<Activation> {
        let row = self.confirming_delete.take()?;
        confirmed.then_some(Activation::Delete(row))
    }

    /// Drops a row after it's been deleted, keeping the selection in range.
    pub fn remove_session(&mut self, id: &str) {
        self.items
            .retain(|item| !matches!(item, LaunchItem::Resume(row) if row.id == id));
        if self.selected >= self.items.len() {
            self.selected = self.items.len().saturating_sub(1);
        }
    }
}

pub fn draw(frame: &mut Frame, picker: &Picker, title: &str, hint: &str) {
    let areas = Layout::vertical([Constraint::Min(1), Constraint::Length(1)]).split(frame.area());

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
            LaunchItem::NewChat => {
                spans.push(Span::styled("New chat", base.green()));
                spans.push(Span::styled(
                    "  plain conversation",
                    Style::new().dark_gray(),
                ));
            }
            LaunchItem::NewAgentChat => {
                spans.push(Span::styled("New agent chat", base.green()));
                spans.push(Span::styled(
                    "  with tools (read/write files, run commands)",
                    Style::new().dark_gray(),
                ));
            }
            LaunchItem::BrowseAll => {
                spans.push(Span::styled("Browse all sessions", base.blue()));
                spans.push(Span::styled("  →", Style::new().dark_gray()));
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
                spans.push(Span::styled(truncate(&row.title, 44), base));
                spans.push(Span::styled(
                    format!("  {}", relative_time(row.updated_at)),
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

    // A pending delete takes over the hint line, so the question is right
    // where the answer gets typed.
    let footer = match &picker.confirming_delete {
        Some(row) => Line::from(vec![
            Span::styled(
                format!(
                    " delete session {} ({})? ",
                    row.short_id(),
                    truncate(&row.title, 30)
                ),
                Style::new().red().bold(),
            ),
            Span::styled("y / n", Style::new().red()),
        ]),
        None => Line::from(Span::styled(format!(" {hint}"), Style::new().dark_gray())),
    };

    frame.render_widget(
        Paragraph::new(Text::from(lines)).block(
            Block::default()
                .borders(Borders::ALL)
                .title(Span::styled(format!(" {title} "), Style::new().bold())),
        ),
        areas[0],
    );
    frame.render_widget(Paragraph::new(footer), areas[1]);
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
    use super::*;

    fn row(id: &str, kind: &str, title: &str) -> SessionRow {
        SessionRow {
            id: format!("{id}-0000-0000-0000-000000000000"),
            kind: kind.to_string(),
            title: title.to_string(),
            updated_at: 0,
        }
    }

    #[test]
    fn launch_offers_both_new_modes_and_a_browse_entry() {
        let picker = Picker::launch(vec![]);
        assert!(matches!(picker.items[0], LaunchItem::NewChat));
        assert!(matches!(picker.items[1], LaunchItem::NewAgentChat));
        assert!(matches!(picker.items.last(), Some(LaunchItem::BrowseAll)));
    }

    #[test]
    fn launch_caps_the_recent_list() {
        let recent: Vec<SessionRow> = (0..20)
            .map(|i| row(&format!("{i:08}"), "chat", "t"))
            .collect();
        let picker = Picker::launch(recent);
        let shown = picker
            .items
            .iter()
            .filter(|i| matches!(i, LaunchItem::Resume(_)))
            .count();
        assert_eq!(shown, RECENT_LIMIT);
    }

    #[test]
    fn selection_stays_in_bounds() {
        let mut picker = Picker::launch(vec![row("abcd1234", "chat", "t")]);
        picker.move_up();
        assert_eq!(picker.selected, 0);
        for _ in 0..50 {
            picker.move_down();
        }
        assert_eq!(picker.selected, picker.items.len() - 1);
    }

    #[test]
    fn activating_each_row_type() {
        let mut picker = Picker::launch(vec![row("abcd1234", "agent_chat", "t")]);
        assert!(matches!(picker.activate(), Some(Activation::NewChat)));
        picker.move_down();
        assert!(matches!(picker.activate(), Some(Activation::NewAgentChat)));
        picker.move_down();
        match picker.activate() {
            Some(Activation::Resume(r)) => assert!(r.is_agentic()),
            other => panic!("expected Resume, got {other:?}"),
        }
        picker.move_down();
        assert!(matches!(picker.activate(), Some(Activation::BrowseAll)));
    }

    #[test]
    fn an_empty_picker_activates_to_nothing() {
        let picker = Picker::sessions(vec![]);
        assert!(picker.activate().is_none());
        assert!(picker.selected_session().is_none());
    }

    #[test]
    fn delete_requires_confirmation() {
        let mut picker = Picker::sessions(vec![row("abcd1234", "chat", "t")]);
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
        let mut picker = Picker::launch(vec![row("abcd1234", "chat", "t")]);
        picker.begin_delete();
        assert!(picker.confirming_delete.is_none());
    }

    #[test]
    fn removing_a_row_keeps_the_selection_valid() {
        let rows = vec![
            row("aaaaaaaa", "chat", "one"),
            row("bbbbbbbb", "chat", "two"),
        ];
        let mut picker = Picker::sessions(rows);
        picker.move_down();
        let id = picker.selected_session().unwrap().id.clone();
        picker.remove_session(&id);
        assert_eq!(picker.items.len(), 1);
        assert!(picker.selected < picker.items.len());
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
