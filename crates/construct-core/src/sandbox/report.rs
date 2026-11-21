//! Reading the helper's stderr: the names it swept, whether it killed the
//! tree for memory, and anything else it said (a sandbox failure). Shell
//! calls and background commands share it.

use super::memory;
use super::supervise::SWEPT;

/// What the helper said on its stderr.
#[derive(Debug, Default)]
pub struct Report {
    /// Names of the processes it swept, sorted.
    pub swept: Vec<String>,
    /// `(used, limit)` bytes when it killed the tree for memory.
    pub memory: Option<(u64, u64)>,
    /// Anything else, each line after a newline.
    pub other: String,
}

impl Report {
    /// The memory note for the model, or nothing when the ceiling held.
    pub fn memory_note(&self) -> String {
        self.memory.map(|(used, limit)| memory::note(used, limit)).unwrap_or_default()
    }
}

/// Split the helper's stderr into what it swept, the memory kill, and
/// anything else it said.
pub fn parse_report(report: &str) -> Report {
    let mut r = Report::default();
    for line in report.lines() {
        if let Some(m) = memory::parse(line) {
            r.memory = Some(m);
            continue;
        }
        match line.strip_prefix(SWEPT).and_then(|r| r.trim_start().split_once(' ')) {
            Some((_pid, comm)) => r.swept.push(comm.to_string()),
            None => {
                r.other.push('\n');
                r.other.push_str(line);
            }
        }
    }
    r.swept.sort();
    r
}

/// `[stopped N background process(es): names]`, or nothing when none were.
pub fn swept_note(mut swept: Vec<String>) -> String {
    if swept.is_empty() {
        return String::new();
    }
    let n = swept.len();
    swept.dedup();
    let s = if n == 1 { "" } else { "es" };
    format!("[stopped {n} background process{s}: {}]", swept.join(", "))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn report_separates_swept_names_from_helper_messages() {
        let r = format!(
            "{}\n{SWEPT} 12 sleep\n{SWEPT} 9 bf\nconstruct sandbox: landlock: nope\n",
            memory::line(2 << 30, 1 << 30)
        );
        let r = parse_report(&r);
        assert_eq!(r.swept, ["bf", "sleep"]);
        assert_eq!(r.other, "\nconstruct sandbox: landlock: nope");
        assert_eq!(r.memory, Some((2 << 30, 1 << 30)));
        assert!(r.memory_note().contains("2.0 GiB of memory, over the sandbox's 1.0 GiB limit"));
        assert_eq!(parse_report("").memory_note(), "");
    }

    #[test]
    fn the_note_counts_every_process_and_names_each_once() {
        assert_eq!(swept_note(vec![]), "");
        assert_eq!(swept_note(vec!["sleep".into(), "sleep".into()]), "[stopped 2 background processes: sleep]");
    }
}
