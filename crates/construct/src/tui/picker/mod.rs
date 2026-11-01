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
    /// The provider serving it (`default` for the top-level endpoint).
    pub(super) provider: String,
    pub(super) dialect: &'static str,
    pub(super) key: String,
    pub(super) current: bool,
    /// False for the running model when the catalog has no entry for it.
    pub(super) catalogued: bool,
}

impl Row {
    pub(super) fn of(cfg: &Config, id: &str) -> Self {
        let mut c = cfg.clone();
        let _ = c.select_model(id);
        let e = c.entry().cloned().unwrap_or_default();
        let window = c.context_window as u64;
        let budget = compact::budget(window, c.max_tokens);
        let ep = c.endpoint();
        Self {
            id: id.to_string(),
            label: e.label,
            description: e.description,
            tags: e.tags,
            window,
            max_tokens: c.max_tokens,
            compacts_at: (budget as f64 * cfg.compaction.threshold as f64) as u64,
            pricing: e.pricing,
            dialect: crate::llm::Provider::of(&ep).name(),
            endpoint: ep.base_url,
            key: ep.api_key_secret,
            provider: ep.name,
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
    /// Per provider: its heading, shown above its rows while nothing is
    /// typed, and what it has and how it was reached, shown beside a row.
    /// Empty when there is only `default`, which needs neither.
    pub(super) groups: std::collections::BTreeMap<String, Group>,
}

/// What a key did to the picker.
#[derive(Debug, PartialEq)]
pub(super) enum Outcome {
    Stay,
    Close,
    /// Switch to this id.
    Pick(String),
}

/// One provider's part of the box: its heading, what it has, and how the
/// last fetch of its list went.
pub(super) struct Group {
    /// `name · host`, or `name · unreachable` when the last fetch failed.
    pub(super) title: String,
    /// `1 catalogued · 26 listed`, or `· no list`.
    pub(super) has: String,
    /// `listed 3m ago`, or `unreachable 20s ago: Connection refused`; none
    /// for a provider nobody has asked yet.
    pub(super) reach: Option<String>,
    /// The last fetch failed: the heading and the reach line are a warning.
    pub(super) down: bool,
}

/// Each provider's [`Group`], built from what is in memory (the lists and
/// what the cache recorded of the fetches): opening the box asks no server.
fn headers(cfg: &Config) -> std::collections::BTreeMap<String, Group> {
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs());
    let ago = |at: u64| match now.saturating_sub(at) {
        s @ 0..60 => format!("{s}s ago"),
        s @ 60..3600 => format!("{}m ago", s / 60),
        s @ 3600..86_400 => format!("{}h ago", s / 3600),
        s => format!("{}d ago", s / 86_400),
    };
    let mut out = std::collections::BTreeMap::new();
    for name in cfg.provider_names() {
        let Some((ep, ids)) = cfg.provider(&name) else { continue };
        let listed = cfg.listed(&name);
        let host = ep.base_url.split_once("://").map_or(ep.base_url.as_str(), |(_, r)| r);
        let host = host.split('/').next().unwrap_or_default();
        let list = if listed == 0 { "no list".to_string() } else { format!("{listed} listed") };
        let reach = cfg.providers.get(&name).map(|p| p.reach.clone()).unwrap_or_default();
        let down = reach.failed.is_some();
        let reach = match (reach.failed, reach.fetched_at) {
            (Some((at, why)), _) => Some(format!("unreachable {}: {why}", ago(at))),
            (None, Some(at)) => Some(format!("listed {}", ago(at))),
            (None, None) => None,
        };
        // The column is narrow: a down provider's heading says so instead of its host.
        let title = if down { format!("{name} · unreachable") } else { format!("{name} · {host}") };
        out.insert(name.clone(), Group { title, has: format!("{} catalogued · {list}", ids.len()), reach, down });
    }
    out
}

impl Picker {
    /// The catalog as rows, the running model selected; none when the
    /// catalog is empty, since a list of one is no choice.
    pub(super) fn open(cfg: &Config) -> Option<Self> {
        // The user's entries first, each provider's, then the endpoint's list.
        let mut ids = cfg.catalog_ids();
        if ids.is_empty() {
            return None;
        }
        if !ids.contains(&cfg.model) {
            ids.insert(0, cfg.model.clone());
        }
        let rows: Vec<Row> = ids.iter().map(|id| Row::of(cfg, id)).collect();
        let groups = if cfg.providers.is_empty() { Default::default() } else { headers(cfg) };
        let mut p =
            Self { rows, query: String::new(), shown: vec![], selected: 0, table: TableState::default(), groups };
        p.refilter();
        p.selected = p.shown.iter().position(|&i| p.rows[i].current).unwrap_or(0);
        Some(p)
    }

    /// Where each row of the table is: a provider's header, or `shown[n]`.
    pub(super) fn lines(&self) -> Vec<Result<&str, usize>> {
        let mut out = vec![];
        let mut last = None;
        for (n, &i) in self.shown.iter().enumerate() {
            let p = self.rows[i].provider.as_str();
            if !self.groups.is_empty() && self.query.is_empty() && last != Some(p) {
                out.push(Ok(p));
                last = Some(p);
            }
            out.push(Err(n));
        }
        out
    }

    /// Rebuild the rows from `cfg`, keeping what is typed and the highlighted
    /// model. Startup discovery can lay a provider's list under the catalog
    /// after the box opened; without this the box would show the windows
    /// and models from before it until reopened.
    pub(super) fn refresh(&mut self, cfg: &Config) {
        let keep = self.current().map(|r| r.id.clone());
        let Some(mut fresh) = Self::open(cfg) else { return };
        fresh.query = std::mem::take(&mut self.query);
        if !fresh.query.is_empty() {
            fresh.refilter();
        }
        if let Some(n) = keep.and_then(|id| fresh.shown.iter().position(|&i| fresh.rows[i].id == id)) {
            fresh.selected = n;
        }
        *self = fresh;
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
        c.select_model("b/two").unwrap();
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
    fn a_list_that_lands_while_open_refreshes_the_rows_and_keeps_query_and_highlight() {
        let mut p = Picker::open(&cfg()).unwrap();
        typed(&mut p, "o");
        p.key(key(KeyCode::Down));
        let before = p.current().unwrap().id.clone();
        let mut later = cfg();
        let entry = |w: u32| crate::models::ModelEntry { context_window: Some(w), ..Default::default() };
        later.discovered.insert("a/one".into(), entry(1));
        later.discovered.insert("d/four".into(), entry(64_000));
        later.select_model("b/two").unwrap();
        p.refresh(&later);
        assert_eq!(p.query, "o", "what was typed stays");
        assert_eq!(p.current().unwrap().id, before, "the highlight stays on the same model");
        assert!(p.rows.iter().any(|r| r.id == "d/four" && r.window == 64_000), "the list's model is a row now");

        let mut idle = Picker::open(&cfg()).unwrap();
        idle.refresh(&later);
        assert_eq!(idle.current().unwrap().id, "b/two", "with nothing typed, still on the running model");
        assert_eq!(idle.shown.len(), 4);
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
        c.select_model("x/adhoc").unwrap();
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
