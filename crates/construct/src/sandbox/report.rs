//! Reading the helper's stderr: the names it swept, and anything else it said
//! (a sandbox failure). Shell calls and background commands share it.

use super::supervise::SWEPT;

/// Split the helper's stderr into the names it swept, sorted, and anything
/// else it said.
pub fn parse_report(report: &str) -> (Vec<String>, String) {
    let mut swept = Vec::new();
    let mut other = String::new();
    for line in report.lines() {
        match line.strip_prefix(SWEPT).and_then(|r| r.trim_start().split_once(' ')) {
            Some((_pid, comm)) => swept.push(comm.to_string()),
            None => {
                other.push('\n');
                other.push_str(line);
            }
        }
    }
    swept.sort();
    (swept, other)
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
        let r = format!("{SWEPT} 12 sleep\n{SWEPT} 9 bf\nconstruct sandbox: landlock: nope\n");
        let (swept, other) = parse_report(&r);
        assert_eq!(swept, ["bf", "sleep"]);
        assert_eq!(other, "\nconstruct sandbox: landlock: nope");
    }

    #[test]
    fn the_note_counts_every_process_and_names_each_once() {
        assert_eq!(swept_note(vec![]), "");
        assert_eq!(swept_note(vec!["sleep".into(), "sleep".into()]), "[stopped 2 background processes: sleep]");
    }
}
