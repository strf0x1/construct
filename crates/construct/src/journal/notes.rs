//! The anti-spam rules for `Event::Note`, in one place. A subsystem records a
//! condition with `record(KIND, detail)`; the coalescer turns a model call's
//! worth of those into one row per kind, caps each kind per session, and folds
//! the overflow into a `suppressed` row, so a flaky stream is one line in the
//! journal rather than two hundred.
//!
//! The design choice a reader must know: call sites never format, truncate,
//! count or decide whether to emit. They name a `Kind` (declared once, in the
//! module that owns the condition) and pass the detail; everything else is
//! here. Clean calls record nothing, so `drain` yields nothing.

use super::Writer;
use crate::event::{Event, Events};
use std::collections::BTreeMap;

/// The `kind` of the row that reports notes dropped past a cap.
pub const SUPPRESSED: &str = "suppressed";
/// The longest `detail` a row keeps, in bytes.
const DETAIL_MAX: usize = 300;

/// One condition worth a journal row: which subsystem saw it, what it is, and
/// how many rows of it a session keeps.
#[derive(Debug, Clone, Copy)]
pub struct Kind {
    pub class: &'static str,
    pub name: &'static str,
    pub cap: u32,
}

#[derive(Default)]
pub struct Coalescer {
    /// This model call so far: occurrences and the first detail, by kind.
    pending: BTreeMap<&'static str, Pending>,
    /// Rows written this session, by kind name.
    written: BTreeMap<&'static str, u32>,
    /// Occurrences dropped past a cap since the last `finish`.
    dropped: BTreeMap<&'static str, (Kind, u32)>,
}

struct Pending {
    kind: Kind,
    count: u32,
    detail: String,
}

impl Coalescer {
    /// Count one occurrence. Only the first detail of a call is kept.
    pub fn record(&mut self, kind: Kind, detail: &str) {
        let p = self.pending.entry(kind.name).or_insert_with(|| Pending { kind, count: 0, detail: scrub(detail) });
        p.count += 1;
    }

    /// End of a model call: one `Note` per kind recorded, past-cap kinds
    /// counted instead of written. Clears the call's counts.
    pub fn drain(&mut self) -> Vec<Event> {
        let mut out = vec![];
        for (name, p) in std::mem::take(&mut self.pending) {
            let written = self.written.entry(name).or_default();
            if *written >= p.kind.cap {
                self.dropped.entry(name).or_insert((p.kind, 0)).1 += p.count;
            } else {
                *written += 1;
                out.push(Event::Note {
                    class: p.kind.class.into(),
                    kind: name.into(),
                    detail: p.detail,
                    count: p.count,
                });
            }
        }
        out
    }

    /// The `suppressed` rows for what `drain` dropped since the last call:
    /// one per kind, `detail` naming the kind it folds. Called at the end of
    /// a turn, the nearest thing a session has to an end.
    pub fn finish(&mut self) -> Vec<Event> {
        std::mem::take(&mut self.dropped)
            .into_values()
            .map(|(k, count)| Event::Note {
                class: k.class.into(),
                kind: SUPPRESSED.into(),
                detail: k.name.into(),
                count,
            })
            .collect()
    }

    /// Journal and send what the call recorded.
    pub fn flush(&mut self, journal: &Writer, tx: &Events) {
        for ev in self.drain() {
            journal.emit(ev, tx);
        }
    }

    /// Journal and send the turn's `suppressed` rows.
    pub fn flush_suppressed(&mut self, journal: &Writer, tx: &Events) {
        for ev in self.finish() {
            journal.emit(ev, tx);
        }
    }
}

/// One line, bounded: details end up in a journal read by eye and a report
/// that gets pasted into issues.
fn scrub(detail: &str) -> String {
    let one_line = detail.split_whitespace().collect::<Vec<_>>().join(" ");
    crate::llm::truncate(&one_line, DETAIL_MAX)
}

#[cfg(test)]
mod tests {
    use super::*;

    const BAD: Kind = Kind { class: "provider", name: "bad_chunk", cap: 2 };
    const RETRY: Kind = Kind { class: "provider", name: "retry", cap: 20 };

    fn note(ev: &Event) -> (&str, &str, &str, u32) {
        match ev {
            Event::Note { class, kind, detail, count } => (class, kind, detail, *count),
            other => panic!("not a note: {other:?}"),
        }
    }

    #[test]
    fn a_clean_call_emits_nothing() {
        assert!(Coalescer::default().drain().is_empty());
    }

    #[test]
    fn two_hundred_occurrences_are_one_row_with_the_first_detail() {
        let mut c = Coalescer::default();
        for i in 0..200 {
            c.record(BAD, &format!("chunk {i}"));
        }
        let rows = c.drain();
        assert_eq!(rows.len(), 1);
        assert_eq!(note(&rows[0]), ("provider", "bad_chunk", "chunk 0", 200));
        assert!(c.drain().is_empty(), "drain clears the call");
    }

    #[test]
    fn kinds_are_separate_rows() {
        let mut c = Coalescer::default();
        c.record(BAD, "a");
        c.record(RETRY, "429");
        c.record(RETRY, "503");
        let rows = c.drain();
        assert_eq!(rows.iter().map(|r| note(r).1).collect::<Vec<_>>(), ["bad_chunk", "retry"]);
        assert_eq!(note(&rows[1]).2, "429");
    }

    #[test]
    fn past_the_cap_rows_fold_into_one_suppressed_row() {
        let mut c = Coalescer::default();
        let mut written = 0;
        for _ in 0..5 {
            c.record(BAD, "x");
            c.record(BAD, "y");
            written += c.drain().len();
        }
        assert_eq!(written, 2, "cap is rows per session");
        let rows = c.finish();
        assert_eq!(rows.len(), 1);
        assert_eq!(note(&rows[0]), ("provider", SUPPRESSED, "bad_chunk", 6), "three calls of two occurrences");
        assert!(c.finish().is_empty(), "finish reports each drop once");
    }

    #[test]
    fn detail_is_one_bounded_line() {
        let mut c = Coalescer::default();
        c.record(BAD, &format!("line one\n  line two\t{}", "é".repeat(500)));
        let rows = c.drain();
        let detail = note(&rows[0]).2;
        assert!(detail.starts_with("line one line two é"));
        assert!(!detail.contains('\n') && detail.len() <= DETAIL_MAX + '…'.len_utf8());
    }
}
