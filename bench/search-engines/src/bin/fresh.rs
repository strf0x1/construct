//! Does the index see files the agent just wrote? How long until it does, and what does it cost in memory?
use fff_search::{Casing, Constraint, FFFMode, FFFQuery, FilePicker, FilePickerOptions, FuzzyQuery, GrepMode, GrepSearchOptions, SharedFilePicker, SharedFrecency};
use std::time::{Duration, Instant};

fn rss_mb() -> f64 {
    let s = std::fs::read_to_string("/proc/self/status").unwrap();
    let kb: f64 = s.lines().find(|l| l.starts_with("VmRSS")).unwrap().split_whitespace().nth(1).unwrap().parse().unwrap();
    kb / 1024.0
}
fn threads() -> usize { std::fs::read_dir("/proc/self/task").unwrap().count() }

fn find(shared: &SharedFilePicker, needle: &str) -> usize {
    let g = shared.read().unwrap();
    let p = g.as_ref().unwrap();
    let q = FFFQuery { raw_query: needle, constraints: Vec::<Constraint>::new(), fuzzy_query: FuzzyQuery::Text(needle), location: None };
    let o = GrepSearchOptions { mode: GrepMode::PlainText, casing: Some(Casing::Sensitive), page_limit: 50, ..Default::default() };
    p.grep(&q, &o).matches.len()
}

fn main() {
    let root = std::fs::canonicalize(std::env::args().nth(1).unwrap()).unwrap();
    let watch = std::env::args().nth(2).as_deref() != Some("nowatch");
    let idx = std::env::args().nth(3).as_deref() == Some("idx");
    println!("baseline rss {:.0} MB, {} threads", rss_mb(), threads());
    let t0 = Instant::now();
    let shared = SharedFilePicker::default();
    FilePicker::new_with_shared_state(shared.clone(), SharedFrecency::default(), FilePickerOptions {
        base_path: root.display().to_string(), mode: FFFMode::Ai, watch, enable_content_indexing: idx, ..Default::default()
    }).unwrap();
    shared.wait_for_scan(Duration::from_secs(60));
    if watch { shared.wait_for_watcher(Duration::from_secs(60)); }
    if idx { while !shared.read().unwrap().as_ref().is_some_and(|p| p.bigram_index().is_some()) { std::thread::sleep(Duration::from_millis(5)); } }
    println!("ready in {:.0} ms (watch={watch}, idx={idx}); rss {:.0} MB, {} threads", t0.elapsed().as_secs_f64() * 1e3, rss_mb(), threads());
    // a search to warm and show rss after use
    find(&shared, "kmalloc_array");
    println!("after one search: rss {:.0} MB", rss_mb());

    // freshness: new file, then edit of an existing file, then delete
    let new = root.join("zz_fresh_new.txt");
    let token = format!("freshtoken_{}", std::process::id());
    std::fs::write(&new, format!("hello {token}\n")).unwrap();
    let t = Instant::now();
    let mut seen = None;
    while t.elapsed() < Duration::from_secs(5) { if find(&shared, &token) > 0 { seen = Some(t.elapsed()); break; } std::thread::sleep(Duration::from_millis(2)); }
    println!("new file visible after: {}", seen.map(|d| format!("{:.0} ms", d.as_secs_f64() * 1e3)).unwrap_or("NEVER (5 s)".into()));

    let tok2 = format!("editedtoken_{}", std::process::id());
    let existing = root.join(std::env::var("EDIT_FILE").unwrap_or("README".into()));
    let orig = std::fs::read(&existing).unwrap();
    let mut edited = orig.clone(); edited.extend_from_slice(format!("\n{tok2}\n").as_bytes());
    std::fs::write(&existing, &edited).unwrap();
    let t = Instant::now();
    let mut seen = None;
    while t.elapsed() < Duration::from_secs(5) { if find(&shared, &tok2) > 0 { seen = Some(t.elapsed()); break; } std::thread::sleep(Duration::from_millis(2)); }
    println!("edit to existing file visible after: {}", seen.map(|d| format!("{:.0} ms", d.as_secs_f64() * 1e3)).unwrap_or("NEVER (5 s)".into()));
    std::fs::write(&existing, &orig).unwrap();

    // immediate (no wait) check, the case an agent hits: write then grep in the next tool call
    let tok3 = format!("immediate_{}", std::process::id());
    std::fs::write(root.join("zz_fresh_new.txt"), format!("{tok3}\n")).unwrap();
    println!("immediate grep after rewrite sees it: {}", find(&shared, &tok3) > 0);
    std::fs::remove_file(&new).unwrap();
    std::thread::sleep(Duration::from_millis(500));
    println!("deleted file gone after 500ms: {}", find(&shared, &tok3) == 0);
}
