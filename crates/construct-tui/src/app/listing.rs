//! What the listing commands (`/models`, `/memory`, `/skills`, `/status`)
//! print: each builds the rows and `boxed.rs` draws them, so they read like
//! `/help`. An empty or failed listing stays a plain notice, built by the caller.

use crate::boxed::{self, Row, Section};
use crate::theme;
use crate::transcript::Item;
use construct_core::llm::catalog::{self, ModelInfo};
use construct_core::memory::{self, Target};
use construct_core::skills::Skill;

/// Models matching `query`, 20 at most; `Err` is the sentence for no match.
pub(super) fn models(all: &[ModelInfo], query: &str) -> Result<Item, String> {
    let (hits, more) = catalog::page(all, query, 20)?;
    let rows = hits.iter().map(|m| catalog::cells(m)).map(|(id, rest)| (id, "", rest)).collect();
    let note = if more > 0 { format!("… and {more} more; narrow the query") } else { String::new() };
    let title = "models · window · USD per M tokens in/out";
    Ok(Item::Boxes { max: boxed::WIDE, sections: vec![Section::new(title, theme::ACCENT, rows)], note })
}

/// One box per non-empty store, with its use of the cap in the title.
pub(super) fn memory(sections: Vec<memory::Section>) -> Item {
    let sections = sections
        .into_iter()
        .map(|s| {
            let color = match s.target {
                Target::User => theme::ACCENT2,
                Target::Memory => theme::ACCENT,
                Target::Project => theme::GREEN,
            };
            let rows = s.entries.into_iter().map(|e| ("•".to_string(), "", e)).collect();
            Section::new(format!("{} · {}/{} chars", s.title, s.used, s.limit), color, rows)
        })
        .collect();
    Item::Boxes { max: boxed::NARROW, sections, note: String::new() }
}

/// The skill library, as `/help` lists it.
pub(super) fn skills(list: &[Skill]) -> Item {
    let rows = list.iter().map(|s| (format!("/{}", s.name), "", s.description.clone())).collect();
    Item::Boxes {
        max: boxed::NARROW,
        sections: vec![Section::new("skills", theme::GREEN, rows)],
        note: "run one with /<name> <request>".into(),
    }
}

/// `/status` rows (see `journal::status_rows`).
pub(super) fn status(rows: &[(&'static str, String)]) -> Item {
    let rows: Vec<Row> = rows.iter().map(|(k, v)| (k.to_string(), "", v.clone())).collect();
    Item::Boxes { max: boxed::NARROW, sections: vec![Section::new("status", theme::ACCENT, rows)], note: String::new() }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(item: Item, width: usize) -> String {
        let Item::Boxes { max, sections, note } = item else { return String::new() };
        boxed::lines(width, max, &sections, &note)
            .iter()
            .map(|l| l.spans.iter().map(|s| s.content.to_string()).collect::<String>())
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn status_aligns_its_labels() {
        let item = status(&[("session", "s-1".into()), ("requests", "2".into())]);
        let t = text(item, 80);
        assert!(t.contains("╭─ status") && t.contains("│ session   s-1") && t.contains("│ requests  2"), "{t}");
    }

    #[test]
    fn memory_titles_carry_the_cap() {
        let s = memory::Section {
            target: Target::Memory,
            title: "Notes".into(),
            used: 5,
            limit: 100,
            entries: vec!["likes tabs".into()],
        };
        let t = text(memory(vec![s]), 80);
        assert!(t.contains("╭─ Notes · 5/100 chars") && t.contains("• ") && t.contains("likes tabs"), "{t}");
    }
}
