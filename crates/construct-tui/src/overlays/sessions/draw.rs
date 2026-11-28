//! Session rows keep their count, age and state at narrow widths. A wrapped,
//! scrollable details pane preserves the original first prompt without truncation.

use super::Browser;
use crate::theme;
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Position, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::Line;
use ratatui::widgets::{Block, BorderType, Borders, Clear, Paragraph, Row, Table, Wrap};
use unicode_width::UnicodeWidthStr;

fn age(at: u64) -> String {
    match construct_core::journal::now_ms().saturating_sub(at) / 1000 {
        n @ 0..60 => format!("{n}s ago"),
        n @ 60..3600 => format!("{}m ago", n / 60),
        n @ 3600..86400 => format!("{}h ago", n / 3600),
        n => format!("{}d ago", n / 86400),
    }
}

impl Browser {
    pub(crate) fn render(&mut self, f: &mut Frame, area: Rect) {
        self.poll();
        f.render_widget(Clear, area);
        let width = area.width.saturating_sub(2).min(116);
        let height = area.height.saturating_sub(2).min(28);
        let rect = Rect::new(
            area.x + area.width.saturating_sub(width) / 2,
            area.y + area.height.saturating_sub(height) / 2,
            width,
            height,
        );
        let block = Block::new()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(Style::new().fg(theme::ACCENT))
            .title(format!(" Resume session · this directory · {}/{} ", self.shown.len(), self.rows.len()))
            .title_bottom(Line::from(if rect.width < 72 {
                " ↑↓ · Enter resume · Esc close "
            } else {
                " ↑↓ · PgUp/PgDn · Home/End · Enter resume · Esc close "
            }));
        let inner = block.inner(rect);
        f.render_widget(block, rect);
        let [search, body] = Layout::vertical([Constraint::Length(2), Constraint::Min(0)]).areas(inner);
        let query = if self.query.is_empty() { "Search sessions…" } else { &self.query };
        f.render_widget(Paragraph::new(format!("⌕ {query}")), search);
        f.set_cursor_position(Position::new(
            search.x.saturating_add(2 + self.query.width() as u16).min(search.right().saturating_sub(1)),
            search.y,
        ));
        let split = body.width >= 88;
        let [list, details] = if split {
            Layout::horizontal([Constraint::Percentage(53), Constraint::Percentage(47)]).spacing(1).areas(body)
        } else {
            Layout::vertical([Constraint::Min(2), Constraint::Length(body.height.min(7))]).areas(body)
        };
        if self.loading {
            f.render_widget(Paragraph::new("Loading sessions…"), list);
        } else if let Some(e) = &self.error {
            f.render_widget(
                Paragraph::new(e.as_str()).style(Style::new().fg(theme::YELLOW)).wrap(Wrap { trim: false }),
                list,
            );
        } else if self.shown.is_empty() {
            let text =
                if self.rows.is_empty() { "No sessions in this directory yet." } else { "No matching sessions." };
            f.render_widget(Paragraph::new(text), list);
        } else {
            let rows = self.shown.iter().map(|&i| {
                let r = &self.rows[i];
                let label = r.title.split_whitespace().collect::<Vec<_>>().join(" ");
                let state = if r.id == self.current {
                    "current"
                } else if r.unreadable.is_some() {
                    "unreadable"
                } else if r.in_use {
                    "in use"
                } else if r.interrupted {
                    "interrupted"
                } else {
                    ""
                };
                let state = if list.width < 44 {
                    match state {
                        "current" => "*",
                        "interrupted" => "!",
                        "in use" => "busy",
                        "unreadable" => "error",
                        _ => "",
                    }
                } else {
                    state
                };
                Row::new(vec![
                    label,
                    age(r.at_ms),
                    format!("{} {}", r.turns, if r.turns == 1 { "turn" } else { "turns" }),
                    state.into(),
                ])
            });
            let table = Table::new(
                rows,
                [
                    Constraint::Min(5),
                    Constraint::Length(7),
                    Constraint::Length(8),
                    Constraint::Length(if list.width >= 44 { 11 } else { 5 }),
                ],
            )
            .row_highlight_style(Style::new().bg(Color::Rgb(44, 48, 72)).add_modifier(Modifier::BOLD))
            .highlight_symbol("▸ ");
            self.table.select(Some(self.selected));
            f.render_stateful_widget(table, list, &mut self.table);
        }
        if let Some(r) = self.row() {
            let provider = r.provider.as_deref().unwrap_or("default");
            let normalized = |s: &str| s.split_whitespace().collect::<Vec<_>>().join(" ");
            let cap = usize::from(details.width.saturating_sub(10)).max(5);
            let first =
                if split { r.first.clone() } else { construct_core::text::truncate(&normalized(&r.first), cap) };
            let latest = construct_core::text::truncate(&normalized(&r.latest), if split { 160 } else { cap });
            let model = if r.model.starts_with(&format!("{provider}/")) {
                r.model.clone()
            } else {
                format!("{provider}/{}", r.model)
            };
            let text = if split {
                format!(
                    "First: {first}\nLatest: {latest}\nDirectory: {}\nModel: {model}\nLast activity: {}\nID: {}\nAlt+PgUp/PgDn: scroll details",
                    r.cwd,
                    construct_core::journal::timestamp(r.at_ms),
                    r.id
                )
            } else {
                format!(
                    "First: {first}\nLatest: {latest}\nDirectory: {}\nModel: {}\nAt: {}\nID: {}",
                    construct_core::text::truncate(&r.cwd, cap),
                    construct_core::text::truncate(&model, cap),
                    construct_core::journal::timestamp(r.at_ms),
                    r.id
                )
            };
            f.render_widget(
                Paragraph::new(text)
                    .wrap(Wrap { trim: false })
                    .scroll((self.detail_scroll, 0))
                    .block(Block::new().borders(Borders::TOP)),
                details,
            );
        }
    }
}
