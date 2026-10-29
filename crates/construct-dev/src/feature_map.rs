//! The `map` gate: `docs/feature-map/` checked against the repo it describes.
//!
//! The map is prose an agent trusts, so drift must be found by a check, not
//! by a reader: an entry whose recipe, color or commit no longer exists, or a
//! command or recipe no entry covers. Everything here is read from files the
//! code already declares (`tui/help.rs`'s `COMMANDS`, the recipe directories,
//! the theme's `Rgb` constants); nothing runs the app. Whether an entry is
//! *true* is the recipe's job, not this one's.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

const MAP: &str = "docs/feature-map";
const RECIPES: &str = ".agents/skills/verify-construct/recipes";
const HELP: &str = "crates/construct/src/tui/help.rs";
const STATUSES: &[&str] = &["planned", "built", "verified"];
/// The H2s of a feature file, in order. A `planned` entry has no recipe yet,
/// so it needs only the first three.
const SECTIONS: &[&str] = &["Sub-features", "How to get to it (user POV)", "Done when", "Driving it", "Gotchas"];

/// One feature file, as much of it as the checks read.
struct Entry {
    file: String,
    text: String,
    front: BTreeMap<String, String>,
    sub_features: Vec<String>,
}

/// Every problem with the map, one line each, `path: what is wrong`.
/// `is_commit` answers whether a `verified_at` names a commit in this repo.
pub fn problems(root: &Path, is_commit: &dyn Fn(&str) -> bool) -> Vec<String> {
    let mut bad = Vec::new();
    let Ok(readme) = std::fs::read_to_string(root.join(MAP).join("README.md")) else {
        return vec![format!("{MAP}/README.md: missing")];
    };
    let entries = entries(root);
    let surfaces: BTreeSet<String> = section(&readme, "Surfaces").iter().filter_map(|l| ticked(l)).collect();
    let index: BTreeSet<String> = section(&readme, "Features")
        .iter()
        .filter_map(|l| Some(l.split("](./").nth(1)?.split(')').next()?.to_string()))
        .collect();

    let mut seen_ids = BTreeMap::new();
    for e in &entries {
        structure(e, &surfaces, is_commit, &mut bad);
        if !index.contains(&e.file) {
            bad.push(format!("{MAP}/README.md: Features does not list {}", e.file));
        }
        for id in &e.sub_features {
            if let Some(first) = seen_ids.insert(id.clone(), e.file.clone()) {
                bad.push(format!("{MAP}/{}: sub-feature `{id}` is also in {first}", e.file));
            }
        }
    }
    for f in index.iter().filter(|f| !entries.iter().any(|e| &e.file == *f)) {
        bad.push(format!("{MAP}/README.md: Features lists {f}, which does not exist"));
    }

    claims(root, &entries, &mut bad);
    coverage(root, &entries, &mut bad);
    bad
}

/// Frontmatter: the id is the filename, the status is known, `verified_at`
/// is present exactly when verified and names a real commit, and every
/// surface is declared in the README. Body: the status's sections, in order,
/// and every step a `Drive:` / `Expect:` pair.
fn structure(e: &Entry, surfaces: &BTreeSet<String>, is_commit: &dyn Fn(&str) -> bool, bad: &mut Vec<String>) {
    let at = |what: String| format!("{MAP}/{}: {what}", e.file);
    let field = |k: &str| e.front.get(k).map(String::as_str).unwrap_or("");
    if field("id") != e.file.trim_end_matches(".md") {
        bad.push(at(format!("id `{}` is not the filename", field("id"))));
    }
    let status = field("status");
    if !STATUSES.contains(&status) {
        bad.push(at(format!("status `{status}` is not one of {}", STATUSES.join(", "))));
    }
    match (status == "verified", field("verified_at")) {
        (true, "") => bad.push(at("verified with no verified_at".into())),
        (false, sha) if !sha.is_empty() => bad.push(at(format!("verified_at on a `{status}` entry"))),
        (true, sha) if !is_commit(sha) => bad.push(at(format!("verified_at `{sha}` is not a commit in this repo"))),
        _ => {}
    }
    let listed = field("surfaces").trim_matches(['[', ']']);
    for s in listed.split(',').map(str::trim).filter(|s| !s.is_empty() && !surfaces.contains(*s)) {
        bad.push(at(format!("surface `{s}` is not declared in README.md")));
    }

    let headings: Vec<&str> = e.text.lines().filter_map(|l| l.strip_prefix("## ")).collect();
    let required = if status == "planned" { &SECTIONS[..3] } else { SECTIONS };
    for name in required.iter().filter(|n| !headings.contains(n)) {
        bad.push(at(format!("no `## {name}` section (required when {status})")));
    }
    let known: Vec<usize> = headings.iter().filter_map(|h| SECTIONS.iter().position(|s| s == h)).collect();
    if known.windows(2).any(|w| w[0] > w[1]) {
        bad.push(at(format!("sections out of order; expected {}", SECTIONS.join(", "))));
    }
    for step in section(&e.text, "Driving it").iter().filter(|l| l.starts_with("- **")) {
        if !(step.contains("Drive:") && step.contains("Expect:")) {
            let name = step.trim_start_matches("- **").split("**").next().unwrap_or("");
            bad.push(at(format!("step `{name}` needs both Drive: and Expect:")));
        }
    }
}

/// What an entry names must exist: each recipe it points at, and each color
/// an `expect-style` asserts, as an `Rgb` in the code or as an assertion in
/// one of the entry's own recipes (blended colors are computed, and driving
/// the recipe proves them).
fn claims(root: &Path, entries: &[Entry], bad: &mut Vec<String>) {
    let known = known_colors(root);
    for e in entries {
        let mut colors = known.clone();
        for r in recipe_refs(&e.text) {
            match std::fs::read_to_string(root.join(RECIPES).join(&r).join("drive.txt")) {
                Ok(drive) => colors.extend(drive.lines().filter(|l| l.contains("expect-style")).flat_map(hex_colors)),
                Err(_) if !root.join(RECIPES).join(&r).is_dir() => {
                    bad.push(format!("{MAP}/{}: recipe `{r}` does not exist in {RECIPES}", e.file))
                }
                Err(_) => {}
            }
        }
        for line in e.text.lines().filter(|l| l.contains("expect-style")) {
            for hex in hex_colors(line) {
                if !colors.contains(&hex) {
                    bad.push(format!(
                        "{MAP}/{}: color #{hex} is neither an Rgb in crates/construct/src nor asserted by its recipe",
                        e.file
                    ));
                }
            }
        }
    }
}

/// What the code declares must have an entry: every recipe directory is
/// named by some entry, and every built-in slash command appears in one.
fn coverage(root: &Path, entries: &[Entry], bad: &mut Vec<String>) {
    let named: BTreeSet<String> = entries.iter().flat_map(|e| recipe_refs(&e.text)).collect();
    let mut recipes: Vec<String> = std::fs::read_dir(root.join(RECIPES))
        .map(|d| d.flatten().filter(|e| e.path().is_dir()).filter_map(|e| e.file_name().into_string().ok()).collect())
        .unwrap_or_default();
    recipes.sort();
    for r in recipes.iter().filter(|r| !named.contains(*r)) {
        bad.push(format!("{RECIPES}/{r}: no feature-map entry names this recipe"));
    }
    for cmd in commands(root) {
        if !entries.iter().any(|e| mentions_command(&e.text, &cmd)) {
            bad.push(format!("{HELP}: `{cmd}` is in COMMANDS but no feature-map entry mentions it"));
        }
    }
}

fn entries(root: &Path) -> Vec<Entry> {
    let mut files: Vec<_> =
        std::fs::read_dir(root.join(MAP)).map(|d| d.flatten().map(|e| e.path()).collect()).unwrap_or_default();
    files.retain(|p| p.extension().is_some_and(|x| x == "md") && !p.ends_with("README.md"));
    files.sort();
    files
        .into_iter()
        .filter_map(|p| {
            let text = std::fs::read_to_string(&p).ok()?;
            let front = frontmatter(&text);
            let sub_features = section(&text, "Sub-features").iter().filter_map(|l| ticked(l)).collect();
            Some(Entry { file: p.file_name()?.to_str()?.to_string(), text, front, sub_features })
        })
        .collect()
}

/// Flat `key: value` lines between the leading `---` fences.
fn frontmatter(text: &str) -> BTreeMap<String, String> {
    let Some(rest) = text.strip_prefix("---\n") else { return BTreeMap::new() };
    rest.lines()
        .take_while(|l| *l != "---")
        .filter_map(|l| l.split_once(':'))
        .map(|(k, v)| (k.trim().to_string(), v.trim().to_string()))
        .collect()
}

/// The lines under `## name`, up to the next H2.
fn section<'a>(text: &'a str, name: &str) -> Vec<&'a str> {
    text.lines()
        .skip_while(|l| l.strip_prefix("## ").is_none_or(|h| !h.starts_with(name)))
        .skip(1)
        .take_while(|l| !l.starts_with("## "))
        .collect()
}

/// The name in a `` - `name` … `` list item.
fn ticked(line: &str) -> Option<String> {
    Some(line.strip_prefix("- `")?.split('`').next()?.to_string())
}

fn recipe_refs(text: &str) -> BTreeSet<String> {
    let marker = format!("{RECIPES}/");
    text.match_indices(&marker)
        .map(|(i, _)| text[i + marker.len()..].chars().take_while(|c| c.is_ascii_alphanumeric() || *c == '-'))
        .map(String::from_iter)
        .filter(|r| !r.is_empty())
        .collect()
}

/// Lowercase hex of each `fg=#rrggbb` / `bg=#rrggbb` on a line.
fn hex_colors(line: &str) -> Vec<String> {
    line.match_indices("g=#")
        .filter_map(|(i, _)| line.get(i + 3..i + 9))
        .filter(|h| h.chars().all(|c| c.is_ascii_hexdigit()))
        .map(str::to_ascii_lowercase)
        .collect()
}

/// Every `Rgb(r, g, b)` in construct's sources, as lowercase hex.
fn known_colors(root: &Path) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    for file in crate::xtask::rust_sources(&root.join("crates")) {
        let Ok(src) = std::fs::read_to_string(&file) else { continue };
        for (i, _) in src.match_indices("Rgb(") {
            let args = src[i + 4..].split(')').next().unwrap_or("");
            let rgb: Vec<u8> = args.split(',').filter_map(|n| n.trim().parse().ok()).collect();
            if let [r, g, b] = rgb[..] {
                out.insert(format!("{r:02x}{g:02x}{b:02x}"));
            }
        }
    }
    out
}

/// The names in `help.rs`'s `COMMANDS` table, e.g. `/model`.
fn commands(root: &Path) -> Vec<String> {
    let Ok(src) = std::fs::read_to_string(root.join(HELP)) else { return Vec::new() };
    let Some(table) = src.split("COMMANDS: &[Entry] = &[").nth(1).and_then(|t| t.split("];").next()) else {
        return Vec::new();
    };
    table.split("entry(\"").skip(1).filter_map(|s| Some(s.split('"').next()?.to_string())).collect()
}

/// `` `/model `` counts for `/model`; `` `/models `` does not.
fn mentions_command(text: &str, cmd: &str) -> bool {
    let needle = format!("`{cmd}");
    text.match_indices(&needle).any(|(i, _)| {
        text[i + needle.len()..].chars().next().is_none_or(|c| !(c.is_ascii_alphanumeric() || c == '-' || c == '_'))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn tree(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("feature-map-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        for d in [MAP, &format!("{RECIPES}/turn"), "crates/construct/src/tui"] {
            std::fs::create_dir_all(dir.join(d)).unwrap();
        }
        write(&dir, HELP, "pub(super) const COMMANDS: &[Entry] = &[\n    entry(\"/new\", \"\", \"fresh\"),\n];\n");
        write(&dir, "crates/construct/src/tui/theme.rs", "pub const GREEN: Color = Color::Rgb(158, 206, 106);\n");
        write(
            &dir,
            &format!("{MAP}/README.md"),
            "## Surfaces\n\n- `tui` — the TUI\n\n## Features\n\n- [Turn](./turn.md) — x\n",
        );
        dir
    }

    fn write(root: &Path, path: &str, text: &str) {
        std::fs::write(root.join(path), text).unwrap();
    }

    const GOOD: &str = "---\nid: turn\nstatus: verified\nsurfaces: [tui]\nverified_at: abc1234\n---\n# Turn\n\n\
        ## Sub-features\n\n- `turn-new` `/new` clears.\n\n## How to get to it (user POV)\n\n- Type.\n\n\
        ## Done when\n\n- Cleared.\n\n## Driving it\n\nRecipe: `.agents/skills/verify-construct/recipes/turn`.\n\n\
        - **Look.** Drive: `expect-style \"ok\" fg=#9ECE6A`. Expect: green.\n\n## Gotchas\n\n- None.\n";

    #[test]
    fn a_map_that_matches_the_repo_has_no_complaints() {
        let root = tree("good");
        write(&root, &format!("{MAP}/turn.md"), GOOD);
        assert_eq!(problems(&root, &|sha| sha == "abc1234"), Vec::<String>::new());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn drift_in_either_direction_is_named() {
        let root = tree("bad");
        let entry = GOOD
            .replace("recipes/turn`", "recipes/gone`")
            .replace("#9ECE6A", "#123456")
            .replace("[tui]", "[tui, web]")
            .replace("`/new` clears", "clears")
            .replace(" Expect: green.", "")
            .replace("## Gotchas\n\n- None.\n", "");
        write(&root, &format!("{MAP}/turn.md"), &entry);
        write(
            &root,
            &format!("{MAP}/extra.md"),
            "---\nid: other\nstatus: done\n---\n## Sub-features\n\n- `turn-new` x\n",
        );
        let bad = problems(&root, &|_| false);
        for want in [
            "turn.md: verified_at `abc1234` is not a commit",
            "turn.md: surface `web` is not declared",
            "turn.md: recipe `gone` does not exist",
            "turn.md: color #123456 is neither an Rgb",
            "recipes/turn: no feature-map entry names this recipe",
            "`/new` is in COMMANDS but no feature-map entry mentions it",
            "turn.md: step `Look.` needs both Drive: and Expect:",
            "turn.md: no `## Gotchas` section (required when verified)",
            "extra.md: id `other` is not the filename",
            "extra.md: status `done` is not one of",
            "README.md: Features does not list extra.md",
            "sub-feature `turn-new` is also in extra.md",
        ] {
            assert!(bad.iter().any(|p| p.contains(want)), "missing {want:?} in {bad:#?}");
        }
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_planned_entry_needs_no_recipe_but_sections_keep_their_order() {
        let root = tree("planned");
        let entry = "---\nid: turn\nstatus: planned\nsurfaces: [tui]\n---\n# Turn\n\n## Done when\n\n- `/new` x\n\n\
            ## Sub-features\n\n- `turn-new` x\n\n## How to get to it (user POV)\n\n- x\n\n\
            Recipe: `.agents/skills/verify-construct/recipes/turn`.\n";
        write(&root, &format!("{MAP}/turn.md"), entry);
        let bad = problems(&root, &|_| true);
        assert_eq!(bad.len(), 1, "{bad:#?}");
        assert!(bad[0].contains("sections out of order"), "{bad:#?}");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_color_the_entrys_own_recipe_asserts_needs_no_rgb() {
        let root = tree("blend");
        write(&root, &format!("{RECIPES}/turn/drive.txt"), "expect-style \"<1%\" fg=#7694d9\n");
        write(&root, &format!("{MAP}/turn.md"), &GOOD.replace("#9ECE6A", "#7694D9"));
        assert_eq!(problems(&root, &|_| true), Vec::<String>::new());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_longer_command_name_is_not_a_mention() {
        assert!(mentions_command("run `/model x`", "/model"));
        assert!(!mentions_command("run `/models x`", "/model"));
    }
}
