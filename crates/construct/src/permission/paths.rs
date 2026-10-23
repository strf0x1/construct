//! Finding out, after the fact, which paths a shell command was refused.
//!
//! A Landlock denial is only visible as EACCES in the command's own output
//! ("cat: /srv/x: Permission denied"), so this reads the paths out of those
//! lines and keeps the ones the policy really doesn't allow. It is a heuristic
//! over prose: a miss means the model gets the plain sandbox hint, as before,
//! and a wrong guess costs the user one prompt that names what it will grant.

use super::Access;
use super::grants::protected;
use crate::sandbox::{Policy, expand_home};
use std::path::PathBuf;

/// Words in a refusal line that mean the command was changing something.
const WRITING: &[&str] =
    &["create", "creat", "touch", "remove", "mkdir", "write", "writing", "rename", "unlink", "move"];

/// The paths in `output`'s permission errors that `policy` doesn't allow, and
/// whether they were wanted for reading or writing. Protected places are
/// never listed. `None` when nothing is left to ask about.
pub fn blocked_in(policy: &Policy, output: &str) -> Option<(Access, Vec<PathBuf>)> {
    let mut access = Access::Read;
    let mut found: Vec<PathBuf> = vec![];
    for line in output.lines().filter(|l| l.contains("Permission denied") || l.contains("Operation not permitted")) {
        let lower = line.to_ascii_lowercase();
        let want = if WRITING.iter().any(|w| lower.contains(w)) { Access::Write } else { Access::Read };
        for tok in
            line.split(|c: char| c.is_whitespace() || matches!(c, ':' | '\'' | '"' | '`' | '(' | ')' | ',' | ';' | '='))
        {
            if !(tok.starts_with('/') || tok.starts_with("~/")) {
                continue;
            }
            let real = Policy::real(&expand_home(tok.trim_end_matches('.')));
            let allowed = match want {
                Access::Read => policy.check_read(&real).is_ok(),
                Access::Write => policy.check_write(&real).is_ok(),
            };
            if allowed || protected(&real) || found.contains(&real) {
                continue;
            }
            if want == Access::Write {
                access = Access::Write;
            }
            found.push(real);
        }
    }
    (!found.is_empty()).then_some((access, found))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn policy() -> Policy {
        Policy {
            read: vec!["/work".into()],
            write: vec!["/work".into()],
            system: vec!["/usr".into()],
            ..Default::default()
        }
    }

    #[test]
    fn a_read_refusal_names_the_path_outside_the_policy() {
        let (access, paths) = blocked_in(&policy(), "cat: /srv/data/x.txt: Permission denied\n[exit 1]").unwrap();
        assert_eq!(access, Access::Read);
        assert_eq!(paths, [PathBuf::from("/srv/data/x.txt")]);
    }

    #[test]
    fn a_write_refusal_asks_for_write() {
        let (access, paths) = blocked_in(&policy(), "touch: cannot touch '/srv/out/a': Permission denied").unwrap();
        assert_eq!(access, Access::Write);
        assert_eq!(paths, [PathBuf::from("/srv/out/a")]);
    }

    #[test]
    fn denials_inside_the_policy_and_in_protected_places_are_not_asked_about() {
        let home = std::env::var("HOME").unwrap();
        let out = format!("cat: /work/f: Permission denied\ncat: {home}/.ssh/id_rsa: Permission denied");
        assert_eq!(blocked_in(&policy(), &out), None);
        assert_eq!(blocked_in(&policy(), "no errors here: /srv/x"), None);
    }
}
