//! Finding out, before a shell command runs, which paths outside the sandbox
//! it names.
//!
//! Landlock shows a denial only as EACCES in the command's own output, and a
//! command that throws that output away (`2>/dev/null`, `|| echo missing`)
//! leaves nothing for `paths` to read (#64). So the command's words are read
//! first: absolute, `~`, `$HOME` and `..` paths the policy doesn't allow.
//!
//! The design choice: only paths that exist count. A word that only looks
//! like a path ("/api/users" in a grep pattern) never prompts, and a path that
//! really doesn't exist keeps its truthful "No such file". A path built at
//! run time (`$DIR/x`, `cd /srv && cat x`) is missed here; the output
//! heuristic still covers it when the command lets its errors through.

use super::grants::protected;
use crate::sandbox::Policy;
use crate::tools::history::JournalQuery;
use crate::tools::skill::{SkillManage, SkillView};
use std::path::{Path, PathBuf};

/// `find` tests whose next word is a pattern, not a place the command reads.
const PATTERN_FLAGS: &[&str] =
    &["-name", "-iname", "-path", "-ipath", "-wholename", "-iwholename", "-regex", "-iregex", "-lname", "-ilname"];

/// The paths a command names that its policy won't let it read, by what the
/// user can be asked about them.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Outside {
    /// Specific places a prompt may offer.
    pub ask: Vec<PathBuf>,
    /// `/`, `$HOME` or a folder above it: nobody wants to grant those.
    pub broad: Vec<PathBuf>,
    /// construct's own home: reached through its tools, never the shell.
    pub construct: Vec<PathBuf>,
    /// `~/.ssh` and the like (`grants::protected`).
    pub protected: Vec<PathBuf>,
}

impl Outside {
    pub fn is_empty(&self) -> bool {
        self.ask.is_empty() && self.broad.is_empty() && self.construct.is_empty() && self.protected.is_empty()
    }

    /// This set less what `policy` now allows (after the user approved some).
    pub fn still_blocked(self, policy: &Policy) -> Self {
        let keep = |v: Vec<PathBuf>| v.into_iter().filter(|p| policy.check_read(p).is_err()).collect();
        Self {
            ask: keep(self.ask),
            broad: keep(self.broad),
            construct: keep(self.construct),
            protected: keep(self.protected),
        }
    }

    /// What the model is told, one bracketed line per kind; empty when
    /// nothing was blocked. `skills` is where skill files can be read.
    pub fn note(&self, construct_home: &Path, skills: &Path) -> String {
        let mut out = vec![];
        let specific: Vec<&PathBuf> = self.ask.iter().chain(&self.protected).collect();
        if !specific.is_empty() {
            let (is, it) = if specific.len() == 1 { ("is", "it") } else { ("are", "them") };
            out.push(format!(
                "[sandbox: {} {is} outside the sandbox, so this command couldn't read {it}. A missing-file error, an empty result or a fallback message about {it} doesn't mean {it} isn't there.]",
                list(&specific)
            ));
        }
        if !self.broad.is_empty() {
            out.push(format!(
                "[sandbox: only the workspace, system paths and toolchains are visible under {}, so a search there misses everything else. Search the workspace, or ask the user where the file is.]",
                list(&self.broad.iter().collect::<Vec<_>>())
            ));
        }
        if !self.construct.is_empty() {
            out.push(format!(
                "[sandbox: construct's own files ({}) aren't reachable from the shell. Load a skill with {}, change it with {} (its files can be read under {}); past sessions are in {}.]",
                construct_home.display(),
                SkillView::NAME,
                SkillManage::NAME,
                skills.display(),
                JournalQuery::NAME
            ));
        }
        out.join("\n")
    }
}

fn list(paths: &[&PathBuf]) -> String {
    paths.iter().map(|p| p.display().to_string()).collect::<Vec<_>>().join(", ")
}

/// The paths outside `policy` that `command`, run in `cwd`, names.
pub fn outside(policy: &Policy, command: &str, cwd: &Path) -> Outside {
    let home = std::env::var_os("HOME").map(|h| Policy::real(Path::new(&h)));
    let construct = crate::config::Paths::resolve().ok().map(|p| Policy::real(&p.home));
    classify(policy, named(command, cwd, home.as_deref()), home.as_deref(), construct.as_deref())
}

fn classify(policy: &Policy, paths: Vec<PathBuf>, home: Option<&Path>, construct: Option<&Path>) -> Outside {
    let mut out = Outside::default();
    for p in paths.into_iter().filter(|p| policy.check_read(p).is_err()) {
        let list = if p == Path::new("/") || home.is_some_and(|h| h.starts_with(&p)) {
            &mut out.broad
        } else if construct.is_some_and(|c| p.starts_with(c)) {
            &mut out.construct
        } else if protected(&p) {
            &mut out.protected
        } else {
            &mut out.ask
        };
        list.push(p);
    }
    out
}

/// Existing places `command` names, real (symlinks resolved), in order, once.
fn named(command: &str, cwd: &Path, home: Option<&Path>) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = vec![];
    let mut skip_next = false;
    for (word, literal) in words(command) {
        if std::mem::take(&mut skip_next) {
            continue;
        }
        if PATTERN_FLAGS.contains(&word.as_str()) {
            skip_next = true;
            continue;
        }
        // `--file=/srv/x`: the value is the place.
        let value = match word.split_once('=') {
            Some((_, v)) if !word.starts_with('/') => v,
            _ => &word,
        };
        if value.contains(['*', '?', '[']) {
            continue;
        }
        let Some(p) = place(value, literal, cwd, home) else { continue };
        if std::fs::symlink_metadata(&p).is_err() {
            continue;
        }
        let real = Policy::real(&p);
        if !out.contains(&real) {
            out.push(real);
        }
    }
    out
}

/// `word` as a path, when it is one: absolute, `~`/`$HOME` (unless written
/// in single quotes), or climbing out of `cwd` with `..`.
fn place(word: &str, literal: bool, cwd: &Path, home: Option<&Path>) -> Option<PathBuf> {
    let from_home = |rest: &str| home.map(|h| h.join(rest.trim_start_matches('/')));
    let p = if word.starts_with('/') {
        PathBuf::from(word)
    } else if literal {
        return None;
    } else if let Some(rest) = word.strip_prefix("${HOME}").or_else(|| word.strip_prefix("$HOME")) {
        from_home(rest)?
    } else if word == "~" || word.starts_with("~/") {
        from_home(&word[1..])?
    } else if word == ".." || word.starts_with("../") {
        cwd.join(word)
    } else {
        return None;
    };
    let s = p.to_string_lossy();
    Some(if s.len() > 1 { PathBuf::from(s.trim_end_matches('/')) } else { p })
}

/// The command's words, quotes removed, split where the shell would split
/// them (whitespace, `;`, `|`, `&`, redirections, parentheses, backticks).
/// The flag says the word was written in single quotes, so `~` and `$HOME`
/// in it are not expanded.
fn words(command: &str) -> Vec<(String, bool)> {
    let mut out = vec![];
    let (mut cur, mut literal, mut started) = (String::new(), false, false);
    let mut chars = command.chars();
    let mut flush = |cur: &mut String, literal: &mut bool, started: &mut bool| {
        if *started {
            out.push((std::mem::take(cur), *literal));
        }
        (*literal, *started) = (false, false);
    };
    while let Some(c) = chars.next() {
        match c {
            '\'' => {
                started = true;
                literal = true;
                cur.extend(chars.by_ref().take_while(|&c| c != '\''));
            }
            '"' => {
                started = true;
                while let Some(c) = chars.next() {
                    match c {
                        '"' => break,
                        '\\' => cur.extend(chars.next()),
                        c => cur.push(c),
                    }
                }
            }
            '\\' => {
                started = true;
                cur.extend(chars.next());
            }
            c if c.is_whitespace() || ";|&()<>`".contains(c) => flush(&mut cur, &mut literal, &mut started),
            c => {
                started = true;
                cur.push(c);
            }
        }
    }
    flush(&mut cur, &mut literal, &mut started);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn words_split_like_a_shell() {
        let w: Vec<String> =
            words(r#"ls -la "/a b/" 2>/dev/null || echo 'x y'; cat ~/f\ g"#).into_iter().map(|(w, _)| w).collect();
        assert_eq!(w, ["ls", "-la", "/a b/", "2", "/dev/null", "echo", "x y", "cat", "~/f g"]);
        assert_eq!(words("echo '~/x'"), [("echo".into(), false), ("~/x".into(), true)]);
    }

    /// The layout of the user-testing session (#64), under a temp dir: the
    /// workspace was /tmp/ptest/work, with a sibling folder beside it.
    struct World {
        root: PathBuf,
        home: PathBuf,
        construct: PathBuf,
        work: PathBuf,
        ptest: PathBuf,
        policy: Policy,
    }

    fn world(name: &str) -> World {
        let root = std::env::temp_dir().join(format!("construct-named-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let (home, ptest) = (root.join("home"), root.join("ptest"));
        let (construct, work) = (home.join(".construct"), ptest.join("work"));
        for d in [
            construct.join("skills/create-verification"),
            home.join(".config"),
            home.join(".local"),
            work.clone(),
            ptest.join("sibling"),
        ] {
            std::fs::create_dir_all(d).unwrap();
        }
        std::fs::write(ptest.join("sibling/other.txt"), "more\n").unwrap();
        std::fs::write(ptest.join("notes.txt"), "n\n").unwrap();
        let real = |p: &Path| Policy::real(p);
        let policy = Policy {
            read: vec![real(&work)],
            write: vec![real(&work), "/dev/null".into()],
            system: ["/usr", "/bin", "/etc", "/proc", "/sys"].iter().map(PathBuf::from).collect(),
            ..Default::default()
        };
        let (home, construct, work, ptest) = (real(&home), real(&construct), real(&work), real(&ptest));
        World { root, home, construct, work, ptest, policy }
    }

    impl World {
        /// A command from the session's journal, with its paths moved here.
        fn run(&self, cmd: &str) -> Outside {
            let cmd = cmd
                .replace("/tmp/ptest", &self.ptest.display().to_string())
                .replace("/home/brandon", &self.home.display().to_string());
            let found = named(&cmd, &self.work, Some(&self.home));
            classify(&self.policy, found, Some(&self.home), Some(&self.construct))
        }
    }

    impl Drop for World {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }

    /// Every shell command of session 20261023-034723-c3ca (and its
    /// neighbours), as the journal recorded it, with what each one names.
    #[test]
    fn the_user_testing_session_replays_as_expected() {
        let w = world("replay");
        let (ptest, sibling) = (w.ptest.clone(), w.ptest.join("sibling"));
        let ask = |v: Vec<PathBuf>| Outside { ask: v, ..Default::default() };
        let broad = Outside { broad: vec!["/".into()], ..Default::default() };
        let broad_and_home =
            Outside { broad: vec!["/".into()], construct: vec![w.construct.clone()], ..Default::default() };
        let cases: &[(&str, Outside)] = &[
            // seq 37, 43: the fallback said "does not exist" for a folder that did.
            (
                r#"ls -la /tmp/ptest/sibling/ 2>/dev/null || echo "Directory does not exist"; echo "---"; ls -la /tmp/ptest/ 2>/dev/null || echo "ptest dir does not exist""#,
                ask(vec![sibling.clone(), ptest.clone()]),
            ),
            (r#"ls -la /tmp/ptest/ 2>/dev/null || echo "ptest directory not found""#, ask(vec![ptest.clone()])),
            // seq 68: a typo; "No such file" is the truth, so nothing is said.
            (r#"ls -la /tmp/ptest/subling/ 2>&1 || echo "Directory does not exist""#, Outside::default()),
            ("ls -la /tmp/ptest/ 2>&1", ask(vec![ptest.clone()])),
            ("ls -la /tmp/ptest/sibling/ 2>&1", ask(vec![sibling.clone()])),
            (
                r#"find /tmp/ptest -name "notes.txt" 2>&1; echo "---"; cat /tmp/ptest/sibling/other.txt"#,
                ask(vec![ptest.clone(), sibling.join("other.txt")]),
            ),
            // seq 165-268: hunting the skill across the whole disk.
            (
                r#"find / -name "SKILL.md" -not -path "/proc/*" -not -path "/sys/*" 2>/dev/null | head -20; echo "---"; ls -la /home/brandon/.construct/ 2>/dev/null"#,
                broad_and_home.clone(),
            ),
            (
                r#"ls -la /tmp/ptest/work/ 2>&1; echo "==="; find / -type d -name "references" -not -path "/proc/*" -not -path "/sys/*" 2>/dev/null | head -10; echo "==="; find / -type d -name "skills" -not -path "/proc/*" -not -path "/sys/*" 2>/dev/null | head -10"#,
                broad.clone(),
            ),
            (
                r#"find / -path /proc -prune -o -path /sys -prune -o -type d -iname "*skill*" -print 2>/dev/null | grep -v proc | grep -v sys | head -20"#,
                broad.clone(),
            ),
            (
                r#"find / -path /proc -prune -o -path /sys -prune -o -name "create-verification*" -print 2>/dev/null | head -20"#,
                broad.clone(),
            ),
            (
                r#"find / -path /proc -prune -o -path /sys -prune -o -name "SKILL.md" -print 2>/dev/null; echo "---"; ls -la /home/brandon/.construct/ 2>/dev/null; echo "---"; ls -la /tmp/ptest/work/ 2>/dev/null"#,
                broad_and_home,
            ),
            (
                r#"ls -la ~/.config/ 2>/dev/null | head -30; echo "==="; ls -la ~/.local/ 2>/dev/null | head -30"#,
                ask(vec![w.home.join(".config"), w.home.join(".local")]),
            ),
            // A child's call (session 20261023-051028-b93b).
            (r#"ls -la /tmp/ptest/ 2>&1 || echo "Cannot list /tmp/ptest/""#, ask(vec![ptest.clone()])),
        ];
        for (cmd, want) in cases {
            assert_eq!(&w.run(cmd), want, "{cmd}");
        }
    }

    #[test]
    fn inside_the_policy_patterns_and_quoted_tildes_name_nothing() {
        let w = world("inside");
        assert!(w.run("cat /tmp/ptest/work/x; grep -r /api/users src").is_empty());
        assert!(w.run("find /usr -name '*.so' 2>/dev/null").is_empty());
        assert!(w.run("echo '~/.config'").is_empty());
        assert_eq!(w.run("ls ../sibling").ask, [w.ptest.join("sibling")]);
        assert_eq!(w.run("ls $HOME/.config").ask, [w.home.join(".config")]);
        assert_eq!(w.run("ls ~").broad, std::slice::from_ref(&w.home));
        assert_eq!(w.run("tool --input=/tmp/ptest/notes.txt").ask, [w.ptest.join("notes.txt")]);
    }

    #[test]
    fn approving_some_leaves_the_rest_in_the_note() {
        let w = world("note");
        let out = w
            .run("ls /tmp/ptest/sibling /tmp/ptest/notes.txt")
            .still_blocked(&w.policy.widened(&[w.ptest.join("sibling")], &[]));
        assert_eq!(out.ask, [w.ptest.join("notes.txt")]);
        let note = out.note(&w.construct, &w.construct.join("skills"));
        assert!(
            note.contains("notes.txt is outside the sandbox") && note.contains("doesn't mean it isn't there"),
            "{note}"
        );
        assert_eq!(Outside::default().note(&w.construct, &w.construct), "");
    }
}
