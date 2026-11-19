//! The launch and in-app session browser share one read-only archive, matching
//! and keyboard handling. Loading happens off the draw/input path.

mod draw;

use super::picker::Outcome;
use crate::config::Paths;
use crate::journal::{self, Session};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::widgets::TableState;
use std::sync::mpsc;

pub(super) struct Browser {
    pub rows: Vec<Session>,
    pub shown: Vec<usize>,
    pub selected: usize,
    pub table: TableState,
    pub query: String,
    pub current: String,
    pub startup: bool,
    pub loading: bool,
    pub error: Option<String>,
    pub detail_scroll: u16,
    rx: mpsc::Receiver<Result<Vec<Session>, String>>,
}

impl Browser {
    pub fn open(paths: Paths, cwd: std::path::PathBuf, current: String, startup: bool) -> Self {
        let (tx, rx) = mpsc::channel();
        tokio::task::spawn_blocking(move || {
            let _ = tx.send(journal::browse(&paths, &cwd).map_err(|e| format!("{e:#}")));
        });
        Self {
            rows: vec![],
            shown: vec![],
            selected: 0,
            table: TableState::default(),
            query: String::new(),
            current,
            startup,
            loading: true,
            error: None,
            detail_scroll: 0,
            rx,
        }
    }

    pub fn poll(&mut self) {
        if let Ok(result) = self.rx.try_recv() {
            self.loading = false;
            match result {
                Ok(rows) => {
                    self.rows = rows;
                    self.filter();
                }
                Err(e) => self.error = Some(format!("Unreadable sessions: {e}")),
            }
        }
    }

    fn filter(&mut self) {
        let query = self.query.split_whitespace().collect::<Vec<_>>().join(" ").to_lowercase();
        self.shown = self
            .rows
            .iter()
            .enumerate()
            .filter_map(|(i, r)| {
                [&r.title, &r.first, &r.latest, &r.id]
                    .iter()
                    .any(|s| s.split_whitespace().collect::<Vec<_>>().join(" ").to_lowercase().contains(&query))
                    .then_some(i)
            })
            .collect();
        self.selected = 0;
        self.error = None;
        self.detail_scroll = 0;
        self.table = TableState::default();
    }

    pub fn row(&self) -> Option<&Session> {
        self.shown.get(self.selected).and_then(|&n| self.rows.get(n))
    }

    pub fn paste(&mut self, s: &str) {
        self.query.push_str(&s.replace(['\r', '\n'], " "));
        self.filter();
    }

    pub fn key(&mut self, k: KeyEvent) -> Outcome {
        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
        let alt = k.modifiers.contains(KeyModifiers::ALT);
        let old = self.selected;
        let last = self.shown.len().saturating_sub(1);
        match k.code {
            KeyCode::Esc => return Outcome::Close,
            KeyCode::Char('c' | 'd') if ctrl => return Outcome::Close,
            KeyCode::Enter => {
                if let Some(r) = self.row() {
                    if r.id == self.current {
                        return Outcome::Close;
                    }
                    if let Some(e) = &r.unreadable {
                        self.error = Some(e.clone());
                    } else if r.in_use {
                        self.error = Some("Session is in use by another process; wait for it to exit.".into());
                    } else {
                        return Outcome::Pick(r.id.clone());
                    }
                }
            }
            KeyCode::Up | KeyCode::Char('p') if k.code == KeyCode::Up || ctrl => {
                self.selected = self.selected.checked_sub(1).unwrap_or(last);
            }
            KeyCode::Down | KeyCode::Char('n') if k.code == KeyCode::Down || ctrl => {
                self.selected = if self.selected >= last { 0 } else { self.selected + 1 };
            }
            KeyCode::PageUp if alt => self.detail_scroll = self.detail_scroll.saturating_sub(8),
            KeyCode::PageDown if alt => self.detail_scroll = self.detail_scroll.saturating_add(8),
            KeyCode::PageUp => self.selected = self.selected.saturating_sub(8),
            KeyCode::PageDown => self.selected = self.selected.saturating_add(8).min(last),
            KeyCode::Home => self.selected = 0,
            KeyCode::End => self.selected = last,
            KeyCode::Backspace => {
                self.query.pop();
                self.filter();
            }
            KeyCode::Char('u') if ctrl => {
                self.query.clear();
                self.filter();
            }
            KeyCode::Char(c) if !ctrl && !alt => {
                self.query.push(c);
                self.filter();
            }
            _ => {}
        }
        if self.selected != old {
            self.detail_scroll = 0;
            self.error = None;
        }
        Outcome::Stay
    }
}
