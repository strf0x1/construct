//! Engine-level search benchmark: today's sequential grep, the same ripgrep
//! crates run in parallel, fff-search, and the rg binary. No model involved.
use fff_search::{
    Casing, Constraint, FFFMode, FFFQuery, FilePicker, FilePickerOptions, FuzzyQuery, GrepMode,
    GrepSearchOptions, SharedFilePicker, SharedFrecency,
};
use grep_regex::RegexMatcherBuilder;
use grep_searcher::{BinaryDetection, SearcherBuilder, sinks::UTF8};
use ignore::{WalkBuilder, WalkState};
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

#[derive(Clone)]
struct Q {
    label: &'static str,
    pattern: String,
    regex: bool,
    icase: bool,
    include: Option<&'static str>,
}

#[derive(Default)]
struct Out {
    total: usize,
    files: usize,
    lines: BTreeSet<(String, u64)>,
}

fn walker(root: &Path) -> WalkBuilder {
    let mut w = WalkBuilder::new(root);
    w.follow_links(false);
    w.hidden(true).git_ignore(true).git_global(true).git_exclude(true).parents(true).require_git(false);
    w.filter_entry(|e| e.file_name() != ".git");
    w
}

fn matcher(q: &Q) -> grep_regex::RegexMatcher {
    let pat = if q.regex { q.pattern.clone() } else { regex_escape(&q.pattern) };
    RegexMatcherBuilder::new().case_insensitive(q.icase).build(&pat).unwrap()
}
fn regex_escape(s: &str) -> String {
    let mut o = String::new();
    for c in s.chars() {
        if "\\.+*?()|[]{}^$#&-~".contains(c) {
            o.push('\\');
        }
        o.push(c);
    }
    o
}
fn include_matcher(q: &Q) -> Option<globset::GlobMatcher> {
    q.include.map(|g| {
        let g = if g.contains('/') { g.to_string() } else { format!("**/{g}") };
        globset::Glob::new(&g).unwrap().compile_matcher()
    })
}

/// A copy of construct's grep today: one thread walks, then searches, in order.
fn seq(root: &Path, q: &Q, keep: bool) -> Out {
    let m = matcher(q);
    let inc = include_matcher(q);
    let mut s = SearcherBuilder::new().binary_detection(BinaryDetection::quit(0)).line_number(true).build();
    let mut out = Out::default();
    for e in walker(root).build().flatten() {
        if !e.file_type().is_some_and(|t| t.is_file()) {
            continue;
        }
        if let Some(i) = &inc {
            if !i.is_match(e.path().strip_prefix(root).unwrap_or(e.path())) {
                continue;
            }
        }
        let Ok(f) = std::fs::File::open(e.path()) else { continue };
        let mut found = false;
        let rel = e.path().strip_prefix(root).unwrap().display().to_string();
        let _ = s.search_file(&m, &f, UTF8(|ln, line| {
            out.total += 1;
            found = true;
            if keep { out.lines.insert((rel.clone(), ln)); } else { let _ = line; }
            Ok(true)
        }));
        out.files += found as usize;
    }
    out
}

/// The same crates, walking and searching on every core (what rg itself does).
fn par(root: &Path, q: &Q, keep: bool) -> Out {
    let m = matcher(q);
    let inc = include_matcher(q);
    let total = AtomicUsize::new(0);
    let files = AtomicUsize::new(0);
    let lines = Mutex::new(BTreeSet::new());
    walker(root).build_parallel().run(|| {
        let m = m.clone();
        let inc = inc.clone();
        let mut s = SearcherBuilder::new().binary_detection(BinaryDetection::quit(0)).line_number(true).build();
        let (total, files, lines) = (&total, &files, &lines);
        Box::new(move |e| {
            let Ok(e) = e else { return WalkState::Continue };
            if !e.file_type().is_some_and(|t| t.is_file()) {
                return WalkState::Continue;
            }
            if let Some(i) = &inc {
                if !i.is_match(e.path().strip_prefix(root).unwrap_or(e.path())) {
                    return WalkState::Continue;
                }
            }
            let Ok(f) = std::fs::File::open(e.path()) else { return WalkState::Continue };
            let rel = e.path().strip_prefix(root).unwrap().display().to_string();
            let mut n = 0;
            let mut mine = Vec::new();
            let _ = s.search_file(&m, &f, UTF8(|ln, _| {
                n += 1;
                if keep { mine.push((rel.clone(), ln)); }
                Ok(true)
            }));
            if n > 0 {
                total.fetch_add(n, Ordering::Relaxed);
                files.fetch_add(1, Ordering::Relaxed);
                if keep { lines.lock().unwrap().extend(mine); }
            }
            WalkState::Continue
        })
    });
    Out { total: total.into_inner(), files: files.into_inner(), lines: lines.into_inner().unwrap() }
}

fn rg(root: &Path, q: &Q, keep: bool) -> Out {
    let mut c = std::process::Command::new("rg");
    c.current_dir(root).args(["-n", "--no-heading", "--color=never", "--with-filename"]);
    if q.icase { c.arg("-i"); } else { c.arg("-s"); }
    if !q.regex { c.arg("-F"); }
    if let Some(g) = q.include { c.args(["-g", g]); }
    c.arg("-e").arg(&q.pattern);
    let o = c.output().unwrap();
    let text = String::from_utf8_lossy(&o.stdout);
    let mut out = Out::default();
    let mut files = BTreeSet::new();
    for l in text.lines() {
        let mut it = l.splitn(3, ':');
        let (f, n) = (it.next().unwrap_or(""), it.next().unwrap_or(""));
        out.total += 1;
        files.insert(f.to_string());
        if keep { out.lines.insert((f.trim_start_matches("./").to_string(), n.parse().unwrap_or(0))); }
    }
    out.files = files.len();
    out
}

struct Fff {
    shared: SharedFilePicker,
    scan: Duration,
    index: Option<Duration>,
    cap: usize,
}
impl Fff {
    fn new(root: &Path, watch: bool, content_index: bool, mmap: bool) -> Fff {
        let t0 = Instant::now();
        let shared = SharedFilePicker::default();
        FilePicker::new_with_shared_state(
            shared.clone(),
            SharedFrecency::default(),
            FilePickerOptions {
                base_path: root.display().to_string(),
                mode: FFFMode::Ai,
                watch,
                enable_content_indexing: content_index,
                enable_mmap_cache: mmap,
                ..Default::default()
            },
        )
        .unwrap();
        assert!(shared.wait_for_scan(Duration::from_secs(120)), "scan timeout");
        let scan = t0.elapsed();
        let mut index = None;
        if content_index {
            // the bigram index is built after the scan; poll for it
            let deadline = Instant::now() + Duration::from_secs(300);
            while Instant::now() < deadline {
                if shared.read().unwrap().as_ref().is_some_and(|p| p.bigram_index().is_some()) {
                    index = Some(t0.elapsed());
                    break;
                }
                std::thread::sleep(Duration::from_millis(5));
            }
        }
        Fff { shared, scan, index, cap: 200 }
    }
    fn run(&self, q: &Q, keep: bool) -> Out {
        let guard = self.shared.read().unwrap();
        let picker = guard.as_ref().unwrap();
        let mut constraints = Vec::new();
        if let Some(g) = q.include {
            if let Some(ext) = g.strip_prefix("*.").filter(|e| e.chars().all(|c| c.is_ascii_alphanumeric())) {
                constraints.push(Constraint::Extension(ext));
            } else {
                constraints.push(Constraint::Glob(g));
            }
        }
        let query = FFFQuery {
            raw_query: &q.pattern,
            constraints,
            fuzzy_query: FuzzyQuery::Text(&q.pattern),
            location: None,
        };
        let opts = GrepSearchOptions {
            mode: if q.regex { GrepMode::Regex } else { GrepMode::PlainText },
            casing: Some(if q.icase { Casing::Insensitive } else { Casing::Sensitive }),
            page_limit: if keep { 10_000_000 } else { self.cap },
            max_matches_per_file: if keep { 1_000_000 } else { 200 },
            ..Default::default()
        };
        let r = picker.grep(&query, &opts);
        let mut out = Out { total: r.matches.len(), files: r.files_with_matches, ..Default::default() };
        if keep {
            for m in &r.matches {
                out.lines.insert((r.files[m.file_index].relative_path(picker), m.line_number));
            }
        }
        out
    }
}

fn queries(name: &str) -> Vec<Q> {
    let (rare, common, rx, anch, ic, inc_pat, inc) = match name {
        "linux" => ("kmalloc_array", "struct", r"static\s+int\s+\w+_probe\b", r"^\s*#include\s+<linux/slab\.h>", "spinlock", "kmalloc", "*.h"),
        "ripgrep" => ("SearcherBuilder", "self", r"fn\s+\w+_new\b", r"^\s*impl\b", "error", "ripgrep", "*.toml"),
        "tokio" => ("JoinHandle", "self", r"fn\s+\w+_new\b", r"^\s*impl\b", "timeout", "tokio", "*.toml"),
        _ => ("PackageId", "self", r"fn\s+\w+_new\b", r"^\s*impl\b", "error", "cargo", "*.toml"),
    };
    let q = |label, pattern: &str, regex, icase, include| Q { label, pattern: pattern.into(), regex, icase, include };
    vec![
        q("lit-rare", rare, false, false, None),
        q("lit-common", common, false, false, None),
        q("regex", rx, true, false, None),
        q("regex-anchored", anch, true, false, None),
        q("icase", ic, false, true, None),
        q("include-glob", inc_pat, false, false, Some(inc)),
        q("no-match", "zzqxjw_no_such_token_42", false, false, None),
    ]
}

fn pctl(mut v: Vec<f64>, p: f64) -> f64 {
    v.sort_by(|a, b| a.partial_cmp(b).unwrap());
    v[((v.len() - 1) as f64 * p).round() as usize]
}

fn time<F: FnMut() -> Out>(reps: usize, mut f: F) -> (f64, f64, Out) {
    let mut last = f();
    let mut ts = Vec::new();
    for _ in 0..reps {
        let t = Instant::now();
        last = f();
        ts.push(t.elapsed().as_secs_f64() * 1000.0);
    }
    (pctl(ts.clone(), 0.5), pctl(ts, 0.9), last)
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let root: PathBuf = std::fs::canonicalize(&args[1]).unwrap();
    let name = root.file_name().unwrap().to_str().unwrap().to_string();
    let reps: usize = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(10);
    let qs = queries(&name);
    let nfiles = walker(&root).build().flatten().filter(|e| e.file_type().is_some_and(|t| t.is_file())).count();
    println!("# corpus {name}: {nfiles} files (after ignore rules), {reps} reps, {} cores", std::thread::available_parallelism().unwrap());

    // fff cold start: build the picker and wait for the scan
    let configs = [("fff", false, false, false), ("fff+watch", true, false, false), ("fff+idx", false, true, false)];
    let mut ffs = Vec::new();
    for (label, watch, idx, mmap) in configs {
        let f = Fff::new(&root, watch, idx, mmap);
        println!("# {label}: scan {:.0} ms{}", f.scan.as_secs_f64() * 1e3, f.index.map(|i| format!(", content index ready at {:.0} ms", i.as_secs_f64() * 1e3)).unwrap_or_default());
        ffs.push((label, f));
    }
    // cold query on a fresh picker, to show first-call cost
    for (label, watch, idx, _) in [("fff", false, false, false), ("fff+idx", false, true, false)] {
        let t = Instant::now();
        let f = Fff::new(&root, watch, idx, false);
        let setup = t.elapsed();
        let t = Instant::now();
        let o = f.run(&qs[0], false);
        println!("# cold {label}: setup {:.0} ms + first query {:.1} ms ({} matches)", setup.as_secs_f64() * 1e3, t.elapsed().as_secs_f64() * 1e3, o.total);
    }

    println!("{:16} {:>11} {:>11} {:>11} {:>11} {:>11} {:>11}   matches(seq/par/rg/fff)", "query", "seq", "par", "rg(bin)", "fff", "fff+watch", "fff+idx");
    let mut correct = Vec::new();
    for q in &qs {
        let a = time(reps, || seq(&root, q, false));
        let b = time(reps, || par(&root, q, false));
        let c = time(reps, || rg(&root, q, false));
        let fr: Vec<_> = ffs.iter().map(|(_, f)| time(reps, || f.run(q, false))).collect();
        println!(
            "{:16} {:>8.1} ms {:>8.1} ms {:>8.1} ms {:>8.1} ms {:>8.1} ms {:>8.1} ms   {}/{}/{}/{}",
            q.label, a.0, b.0, c.0, fr[0].0, fr[1].0, fr[2].0, a.2.total, b.2.total, c.2.total, fr[0].2.total
        );
        // correctness: uncapped sets against the rg binary
        let truth = rg(&root, q, true);
        let s = seq(&root, q, true);
        let p = par(&root, q, true);
        let f = ffs[0].1.run(q, true);
        let fi = ffs[2].1.run(q, true);
        if std::env::var("DIFF").is_ok() {
            for (label, x) in [("fff", &f)] {
                let extra: Vec<_> = x.lines.difference(&truth.lines).take(4).collect();
                let missing: Vec<_> = truth.lines.difference(&x.lines).take(4).collect();
                println!("  [{}] {label} extra e.g. {:?}", q.label, extra);
                println!("  [{}] {label} missing e.g. {:?}", q.label, missing);
            }
        }
        let diff = |x: &Out| (x.lines.difference(&truth.lines).count(), truth.lines.difference(&x.lines).count());
        correct.push(format!(
            "{:16} truth(rg)={:>6}  seq +{}/-{}  par +{}/-{}  fff +{}/-{}  fff+idx +{}/-{}",
            q.label, truth.lines.len(), diff(&s).0, diff(&s).1, diff(&p).0, diff(&p).1, diff(&f).0, diff(&f).1, diff(&fi).0, diff(&fi).1
        ));
    }
    println!("# correctness vs rg binary (+extra lines / -missing lines)");
    for c in correct { println!("{c}"); }
}
