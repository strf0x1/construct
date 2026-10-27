//! The model picker: `/model` with no argument opens a box over the
//! transcript listing the catalog (`crate::models`), narrowed as you type,
//! with the highlighted model's limits beside it. It takes every key while
//! open, like a permission question. The rows are resolved the way a switch
//! would resolve them (`Config::select_model`), so the box shows the window
//! and endpoint the next request will really use.

mod draw;

pub(super) use draw::render;

use super::palette;
use crate::compact;
use crate::config::Config;
use crate::models::Pricing;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::widgets::TableState;

/// One selectable model, with everything the box shows about it.
pub(super) struct Row {
    pub(super) id: String,
    pub(super) label: String,
    pub(super) description: String,
    pub(super) tags: Vec<String>,
    pub(super) window: u64,
    pub(super) max_tokens: Option<u32>,
    /// Prompt size at which compaction starts for this window.
    pub(super) compacts_at: u64,
    pub(super) pricing: Option<Pricing>,
    pub(super) endpoint: String,
    pub(super) dialect: &'static str,
    pub(super) key: String,
    pub(super) current: bool,
    /// False for the running model when the catalog has no entry for it.
    pub(super) catalogued: bool,
}

impl Row {
    pub(super) fn of(cfg: &Config, id: &str) -> Self {
        let mut c = cfg.clone();
        c.select_model(id);
        let e = c.entry().cloned().unwrap_or_default();
        let window = c.context_window as u64;
        let budget = compact::budget(window, c.max_tokens);
        Self {
            id: id.to_string(),
            label: e.label,
            description: e.description,
            tags: e.tags,
            window,
            max_tokens: c.max_tokens,
            compacts_at: (budget as f64 * cfg.compaction.threshold as f64) as u64,
            pricing: e.pricing,
            endpoint: c.base_url.clone(),
            dialect: crate::llm::Provider::new(&c.provider, &c.base_url).name(),
            key: c.api_key_secret.clone(),
            current: id == cfg.model,
            catalogued: c.entry().is_some(),
        }
    }

    /// What a search looks in after the id: label, tags and description.
    pub(super) fn about(&self) -> String {
        format!("{} {} {}", self.label, self.tags.join(" "), self.description)
    }
}

pub(super) struct Picker {
    pub(super) rows: Vec<Row>,
    pub(super) query: String,
    /// Indexes into `rows` that match the query, best first.
    pub(super) shown: Vec<usize>,
    /// Position in `shown`.
    pub(super) selected: usize,
    pub(super) table: TableState,
}

/// What a key did to the picker.
#[derive(Debug, PartialEq)]
pub(super) enum Outcome {
    Stay,
    Close,
    /// Switch to this id.
    Pick(String),
}

impl Picker {
    /// The catalog as rows, the running model selected; none when the
    /// catalog is empty, since a list of one is no choice.
    pub(super) fn open(cfg: &Config) -> Option<Self> {
        if cfg.models.is_empty() && cfg.discovered.is_empty() {
            return None;
        }
        // The user's entries first, then the endpoint's list.
        let mut ids: Vec<&str> = cfg.models.keys().map(String::as_str).collect();
        ids.extend(cfg.discovered.keys().map(String::as_str).filter(|id| !cfg.models.contains_key(*id)));
        if !ids.contains(&cfg.model.as_str()) {
            ids.insert(0, &cfg.model);
        }
        let rows: Vec<Row> = ids.into_iter().map(|id| Row::of(cfg, id)).collect();
        let mut p = Self { rows, query: String::new(), shown: vec![], selected: 0, table: TableState::default() };
        p.refilter();
        p.selected = p.shown.iter().position(|&i| p.rows[i].current).unwrap_or(0);
        Some(p)
    }

    pub(super) fn current(&self) -> Option<&Row> {
        self.shown.get(self.selected).map(|&i| &self.rows[i])
    }

    /// Match the query: id prefix, then id substring, then letters in order,
    /// then the label, tags and description. Without a query the catalog's order.
    fn refilter(&mut self) {
        let q = self.query.to_lowercase();
        let mut hits: Vec<((u8, usize), usize)> = self
            .rows
            .iter()
            .enumerate()
            .filter_map(|(n, r)| palette::score(&q, &r.id.to_lowercase(), &r.about()).map(|s| (s, n)))
            .collect();
        hits.sort();
        self.shown = hits.into_iter().map(|(_, n)| n).collect();
        self.selected = 0;
    }

    fn step(&mut self, by: isize) {
        let n = self.shown.len();
        if n > 0 {
            self.selected = (self.selected as isize + by).rem_euclid(n as isize) as usize;
        }
    }

    pub(super) fn key(&mut self, k: KeyEvent) -> Outcome {
        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
        match k.code {
            KeyCode::Esc => return Outcome::Close,
            KeyCode::Char('c' | 'd') if ctrl => return Outcome::Close,
            KeyCode::Enter => {
                return match self.current() {
                    Some(r) if r.current => Outcome::Close,
                    Some(r) => Outcome::Pick(r.id.clone()),
                    None => Outcome::Stay,
                };
            }
            KeyCode::Up => self.step(-1),
            KeyCode::Down => self.step(1),
            KeyCode::Char('p') if ctrl => self.step(-1),
            KeyCode::Char('n') if ctrl => self.step(1),
            KeyCode::PageUp => self.step(-8),
            KeyCode::PageDown => self.step(8),
            KeyCode::Home => self.selected = 0,
            KeyCode::End => self.selected = self.shown.len().saturating_sub(1),
            KeyCode::Backspace => {
                self.query.pop();
                self.refilter();
            }
            KeyCode::Char('u') if ctrl => {
                self.query.clear();
                self.refilter();
            }
            KeyCode::Char(c) if !ctrl => {
                self.query.push(c);
                self.refilter();
            }
            _ => {}
        }
        Outcome::Stay
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg() -> Config {
        let mut c: Config = toml::from_str(
            r#"
            model = "b/two"
            [models."a/one"]
            label = "One — fast"
            tags = ["vision"]
            context_window = 200000
            [models."b/two"]
            label = "Two"
            context_window = 1000000
            [models."c/three"]
            description = "the cheap one"
            "#,
        )
        .unwrap();
        c.select_model("b/two");
        c
    }

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn typed(p: &mut Picker, s: &str) {
        for ch in s.chars() {
            p.key(key(KeyCode::Char(ch)));
        }
    }

    #[test]
    fn opens_on_the_running_model_and_an_empty_catalog_does_not_open() {
        let p = Picker::open(&cfg()).unwrap();
        assert_eq!(p.current().unwrap().id, "b/two");
        assert!(p.current().unwrap().current);
        assert!(Picker::open(&Config::default()).is_none());
    }

    #[test]
    fn an_uncatalogued_running_model_is_listed_first() {
        let mut c = cfg();
        c.select_model("x/adhoc");
        let p = Picker::open(&c).unwrap();
        assert_eq!(p.rows[0].id, "x/adhoc");
        assert!(!p.rows[0].catalogued && p.rows[0].current);
        assert_eq!(p.rows[0].window, 131_072, "unknown ids run on the defaults");
    }

    #[test]
    fn rows_carry_each_models_own_window() {
        let p = Picker::open(&cfg()).unwrap();
        let w: Vec<(String, u64)> = p.rows.iter().map(|r| (r.id.clone(), r.window)).collect();
        assert_eq!(w, [("a/one".into(), 200_000), ("b/two".into(), 1_000_000), ("c/three".into(), 131_072)]);
        assert!(p.rows[0].compacts_at < p.rows[0].window);
    }

    #[test]
    fn typing_narrows_by_id_then_label_tags_and_description() {
        let mut p = Picker::open(&cfg()).unwrap();
        typed(&mut p, "thr");
        assert_eq!(p.shown.len(), 1);
        let mut p = Picker::open(&cfg()).unwrap();
        typed(&mut p, "vision");
        assert_eq!(p.current().unwrap().id, "a/one", "tags are searched");
        let mut p = Picker::open(&cfg()).unwrap();
        typed(&mut p, "cheap");
        assert_eq!(p.current().unwrap().id, "c/three", "so is the description");
        typed(&mut p, "zzz");
        assert!(p.current().is_none());
        assert_eq!(p.key(key(KeyCode::Enter)), Outcome::Stay, "nothing to pick");
    }

    #[test]
    fn arrows_wrap_and_enter_picks_unless_it_is_the_running_model() {
        let mut p = Picker::open(&cfg()).unwrap();
        assert_eq!(p.key(key(KeyCode::Enter)), Outcome::Close, "the running model: no switch");
        p.key(key(KeyCode::Up));
        assert_eq!(p.key(key(KeyCode::Enter)), Outcome::Pick("a/one".into()));
        p.key(key(KeyCode::Up));
        assert_eq!(p.current().unwrap().id, "c/three", "wraps");
        assert_eq!(p.key(key(KeyCode::Esc)), Outcome::Close);
    }
}
