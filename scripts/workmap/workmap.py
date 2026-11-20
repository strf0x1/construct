#!/usr/bin/env python3
"""Build the work map: construct's modules, issues, ADRs and features as one graph.

The page (`index.html` beside this file) draws `graph.json`, which this script
writes from two sources: GitHub's issues (fetched with `gh`) and the repo's own
`docs/architecture.toml`, `docs/adr/` and `docs/feature-map/`, read with
`git show <ref>:path` so the map describes one commit, not the working tree.

The one design choice to know: an issue link is either "native" (a GitHub
sub-issue or blocked-by relationship) or "inferred" from a phrase in the issue
body ("after #12", "depends on #40", "part of #243"). Inferred links are
suggestions for a person to confirm as native ones; the page draws them
dashed and shows the phrase each came from.

    python3 scripts/workmap/workmap.py              # fetch, build into .workmap/
    python3 scripts/workmap/workmap.py --serve      # ... and serve it on localhost
"""

import argparse
import datetime
import functools
import http.server
import json
from pathlib import Path
import re
import shutil
import subprocess
import tomllib

ROOT = Path(__file__).resolve().parents[2]
PAGE = Path(__file__).resolve().parent / "index.html"

TUI = "tui"  # the construct-tui crate is one node; its modules are listed on it
TOOLING = [("bench", "bench"), ("dev", "construct-dev"), ("workspace", "workspace & CI")]

# Words that point at a module beyond its own name. A guess, weighted below
# labels, `src/<module>` paths and an "Area:" title prefix.
ALIASES = {
    "compact": ["compaction", "compactor"],
    "tools": ["tool", "shell", "bash tool", "edit tool", "hashline", "grep", "web_search", "codemode", "code mode"],
    "sandbox": ["landlock", "seatbelt", "bwrap", "sandboxed"],
    "learn": ["learning", "learner", "curator", "playbook"],
    "llm": ["provider dialect", "dialect", "sse", "stream"],
    "router": ["routing", "routed"],
    "journal": ["session export", "replay"],
    "child": ["child process", "delegat", "subagent"],
    "agents": ["[agents"],
    "acp": ["deck"],
    "permission": ["permission prompt", "permissions"],
    "providers": ["openrouter", "provider"],
    "models": ["model catalog", "catalog"],
    "memory": ["memory.md", "user.md"],
    "skills": ["skill"],
    "jobs": ["background job", "watcher"],
    "secrets": ["secretspec", "secret"],
    "context": ["context strategy", "optchat"],
    TUI: ["ratatui", "status line", "screen"],
    "bench": ["swe-bench", "benchmark", "hermes", "opencode"],
    "dev": ["construct-dev", "xtask", "verify-construct", "feature map"],
    "workspace": ["crate", "cargo", "ci ", "binary size", "release"],
    "http": ["http client", "tls", "reqwest"],
    "home": ["construct_home"],
}

# "<phrase> #N": this issue waits on #N. Stops at punctuation so a list after
# "needs" does not pull in a later, unrelated reference.
AFTER = re.compile(
    r"(after|once|needs|waits? for|start after|gated on|depends on|blocked by|builds on|requires|follows|follow-up to|on top of)"
    r"[^.\n#(),;]{0,40}?([\w.-]+/[\w.-]+)?#(\d+)",
    re.I,
)
PART = re.compile(r"part of #(\d+)", re.I)
REF = re.compile(r"(?<![\w/&])#(\d+)\b")

ISSUES_QUERY = """
query($owner:String!, $name:String!, $endCursor:String) {
  repository(owner:$owner, name:$name) {
    issues(first:50, after:$endCursor, orderBy:{field:CREATED_AT, direction:ASC}) {
      pageInfo { hasNextPage endCursor }
      nodes {
        number title state stateReason createdAt closedAt body url
        labels(first:20) { nodes { name } }
        blockedBy(first:30) { nodes { number } }
        subIssues(first:30) { nodes { number } }
        closedByPullRequestsReferences(first:5, includeClosedPrs:true) { nodes { number } }
      }
    }
  }
}"""


class Tree:
    """Read files at one commit."""

    def __init__(self, repo, ref):
        self.repo, self.ref = repo, ref

    def _git(self, *args):
        return subprocess.run(["git", "-C", str(self.repo), *args], capture_output=True, text=True, check=True).stdout

    def show(self, path):
        return self._git("show", f"{self.ref}:{path}")

    def ls(self, path):
        return [p for p in self._git("ls-tree", "--name-only", self.ref, path + "/").split("\n") if p]

    def sha(self):
        return self._git("rev-parse", "--short", self.ref).strip()


def fetch_issues(slug):
    """Every issue in `owner/name`, open and closed, with its relationships."""
    owner, name = slug.split("/", 1)
    out = subprocess.run(
        ["gh", "api", "graphql", "--paginate", "--slurp", "-f", f"query={ISSUES_QUERY}", "-F", f"owner={owner}", "-F", f"name={name}"],
        capture_output=True, text=True, check=True,
    ).stdout
    return [n for page in json.loads(out) for n in page["data"]["repository"]["issues"]["nodes"]]


def modules_from(arch):
    """Module nodes and "may use" edges from architecture.toml."""
    modules, edges = {}, []
    for crate, cfg in arch.get("crates", {}).items():
        for name, m in cfg.get("modules", {}).items():
            if name in ("lib", "main"):
                continue
            if crate == "construct-tui":
                modules.setdefault(TUI, {"id": TUI, "crate": crate, "label": TUI, "submodules": []})["submodules"].append(name)
                continue
            if crate not in ("construct", "construct-core"):
                continue  # construct-dev and other tooling: the "dev" area
            modules[name] = {"id": name, "crate": crate, "label": name, "submodules": []}
            edges += [[name, dep] for dep in m.get("may_use", [])]
    if TUI in modules:
        edges += [[TUI, "agent"], ["cli", TUI]]
    edges = [e for e in edges if e[0] in modules and e[1] in modules and e[0] != e[1]]
    for mid, label in TOOLING:
        modules[mid] = {"id": mid, "crate": "tooling", "label": label, "submodules": []}
    return modules, edges


def area_scores(modules, title, text, labels=()):
    """How strongly an issue or document points at each module."""
    t, ti = text.lower(), title.lower()
    hits = {}

    def add(mid, n):
        hits[mid] = hits.get(mid, 0) + n

    for lab in labels:
        if lab in modules:
            add(lab, 5)
    prefix = re.match(r"\s*([a-z_ -]+):", ti)
    for mid in modules:
        words = [w for w in [mid] + ALIASES.get(mid, []) if len(w) >= 3]
        if prefix and prefix.group(1).strip() in words:
            add(mid, 8)
        for w in words:
            pat = r"(?<![a-z])" + re.escape(w)
            if re.search(pat, ti):
                add(mid, 4)
            n = min(2, len(re.findall(pat, t)))
            if n:
                add(mid, n)
    for m in re.findall(r"src/([a-z_]+)", t):
        if m in modules:
            add(m, 3)
    return hits


def top_areas(hits, k=3, floor=2):
    ranked = sorted(hits.items(), key=lambda kv: -kv[1])
    if not ranked:
        return []
    return [mid for mid, v in ranked[:k] if v >= max(floor, ranked[0][1] * 0.35)]


def issue_edges(raw, slug):
    """Links between issues: `from` waits on (`after`) or is part of (`part`) `to`."""
    known = {i["number"] for i in raw}
    edges, seen = [], set()

    def add(src, dst, kind, rel, why):
        if src == dst or dst not in known or (src, dst, rel) in seen:
            return
        seen.add((src, dst, rel))
        edges.append({"from": src, "to": dst, "kind": kind, "rel": rel, "why": why.strip()[:140]})

    for i in raw:
        n, body = i["number"], i.get("body") or ""
        for b in i["blockedBy"]["nodes"]:
            add(n, b["number"], "native", "after", "GitHub blocked-by")
        for s in i["subIssues"]["nodes"]:
            add(s["number"], n, "native", "part", "GitHub sub-issue")
        for m in PART.finditer(body):
            add(n, int(m.group(1)), "inferred", "part", m.group(0))
        for m in AFTER.finditer(body):
            if m.group(2) and m.group(2) != slug:
                continue  # another repository's issue
            tgt = int(m.group(3))
            if re.search(rf"PR #{tgt}\b", body[max(0, m.start() - 4):m.end()]):
                continue  # a pull request, not an issue
            add(n, tgt, "inferred", "after", m.group(0))
    # "after #N" where #N is one's own sub-issue describes timing, not order
    parts = {(e["from"], e["to"]) for e in edges if e["rel"] == "part"}
    return [e for e in edges if not (e["rel"] == "after" and (e["to"], e["from"]) in parts)]


def stage(issue, open_blockers):
    """Where an issue is in idea → decision → build, from state and labels."""
    labels = set(issue["labels"])
    if issue["state"] == "closed":
        return "dropped" if issue["reason"] == "not_planned" else "done"
    if open_blockers:
        return "waiting"
    if "question" in labels:
        return "decide"
    if "experiment" in labels:
        return "experiment"
    if "agent-ready" in labels or "bug" in labels:
        return "ready"
    return "shape"


def downstream(edges, n, open_nums):
    """How many open issues transitively wait on issue `n`."""
    rev = {}
    for e in edges:
        if e["rel"] == "after":
            rev.setdefault(e["to"], set()).add(e["from"])
    seen, stack = set(), [n]
    while stack:
        for d in rev.get(stack.pop(), ()):
            if d not in seen:
                seen.add(d)
                stack.append(d)
    return len(seen & open_nums)


def front_matter(text):
    fm = re.match(r"---\n(.*?)\n---", text, re.S)
    meta = {}
    for line in (fm.group(1).split("\n") if fm else []):
        if ":" in line:
            k, v = line.split(":", 1)
            meta[k.strip()] = v.strip()
    return meta


def build(tree, raw, slug):
    """The whole graph.json document."""
    modules, mod_edges = modules_from(tomllib.loads(tree.show("docs/architecture.toml")))
    by_num = {i["number"] for i in raw}
    edges = issue_edges(raw, slug)

    issues = []
    for i in raw:
        body, labels = i.get("body") or "", [lab["name"] for lab in i["labels"]["nodes"]]
        issues.append({
            "n": i["number"], "title": i["title"], "state": i["state"].lower(),
            "reason": (i.get("stateReason") or "").lower(),
            "created": i["createdAt"][:10], "closed": (i.get("closedAt") or "")[:10], "url": i["url"],
            "labels": labels, "areas": top_areas(area_scores(modules, i["title"], body, labels)),
            "related": sorted({int(x) for x in REF.findall(body)} & by_num - {i["number"]}),
            "prs": [p["number"] for p in i["closedByPullRequestsReferences"]["nodes"]],
            "excerpt": re.sub(r"\s+", " ", re.sub(r"[#>*`\[\]|]", "", body)).strip()[:320],
        })

    adrs = []
    for path in tree.ls("docs/adr"):
        fname = path.rsplit("/", 1)[-1]
        if not re.match(r"\d{4}-.*\.md$", fname):
            continue
        text = tree.show(path)
        title = re.sub(r"^#\s*", "", text.split("\n", 1)[0]).strip()
        status = re.search(r"^Status:\s*([a-z]+)", text, re.M | re.I)
        implements = {int(x) for x in re.findall(r"(?:Implements|Completes|Follow-up to|for)\s+(?:construct)?#(\d+)", text[:1500])}
        adrs.append({
            "id": fname[:4], "file": fname, "title": title, "status": status.group(1).lower() if status else "unknown",
            "implements": sorted(implements & by_num),
            "refs": sorted({int(x) for x in re.findall(r"(?:construct)?#(\d+)", text)} & by_num),
            "areas": top_areas(area_scores(modules, title, text[:4000]), k=2),
        })

    features = []
    for path in tree.ls("docs/feature-map"):
        if not path.endswith(".md") or path.endswith("README.md"):
            continue
        text = tree.show(path)
        meta = front_matter(text)
        areas = set()
        for dep in re.findall(r"[\w./-]+", meta.get("depends", "")):
            m = re.search(r"construct(?:-core)?/src/([a-z_]+)", dep)
            if "construct-tui" in dep:
                areas.add(TUI)
            elif m and m.group(1) in modules:
                areas.add(m.group(1))
        heading = re.search(r"^# (.+)$", text, re.M)
        features.append({
            "id": meta.get("id", path), "title": heading.group(1) if heading else meta.get("id", path),
            "status": meta.get("status", "planned"), "verified_at": meta.get("verified_at", ""), "areas": sorted(areas),
        })

    open_nums = {x["n"] for x in issues if x["state"] == "open"}
    for x in issues:
        x["blockers"] = [e["to"] for e in edges if e["from"] == x["n"] and e["rel"] == "after"]
        x["open_blockers"] = [b for b in x["blockers"] if b in open_nums]
        x["adrs"] = [a["id"] for a in adrs if x["n"] in a["implements"]]
        x["stage"] = stage(x, x["open_blockers"])
        x["unblocks"] = downstream(edges, x["n"], open_nums)

    return {
        "generated": datetime.datetime.now().isoformat(timespec="minutes"), "ref": tree.ref, "sha": tree.sha(),
        "repo": slug, "modules": list(modules.values()), "module_edges": mod_edges,
        "issues": issues, "edges": edges, "adrs": adrs, "features": features,
    }


class Utf8Handler(http.server.SimpleHTTPRequestHandler):
    """The page has no <head> of its own (it is also published as an artifact), so the charset comes from here."""

    extensions_map = {**http.server.SimpleHTTPRequestHandler.extensions_map,
                      ".html": "text/html; charset=utf-8", ".json": "application/json; charset=utf-8"}


def main():
    ap =argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    ap.add_argument("--ref", default="origin/main", help="commit the repo files are read at (default origin/main)")
    ap.add_argument("--repo", default="strf0x1/construct", help="GitHub owner/name for issues")
    ap.add_argument("--issues", type=Path, help="read issues from this JSON file instead of fetching")
    ap.add_argument("--out", type=Path, default=ROOT / ".workmap", help="output directory (default .workmap/)")
    ap.add_argument("--serve", type=int, nargs="?", const=8765, metavar="PORT", help="serve the map on 127.0.0.1")
    args = ap.parse_args()

    raw = json.loads(args.issues.read_text()) if args.issues else fetch_issues(args.repo)
    graph = build(Tree(ROOT, args.ref), raw, args.repo)
    args.out.mkdir(parents=True, exist_ok=True)
    (args.out / "issues.json").write_text(json.dumps(raw))
    (args.out / "graph.json").write_text(json.dumps(graph))
    shutil.copy(PAGE, args.out / "index.html")
    inferred = sum(e["kind"] == "inferred" for e in graph["edges"])
    print(f"{len(graph['issues'])} issues, {len(graph['edges'])} links ({inferred} inferred), "
          f"{len(graph['modules'])} modules, {len(graph['adrs'])} ADRs at {graph['sha']} -> {args.out}", flush=True)
    if args.serve:
        handler = functools.partial(Utf8Handler, directory=str(args.out))
        print(f"http://127.0.0.1:{args.serve}/", flush=True)
        http.server.ThreadingHTTPServer(("127.0.0.1", args.serve), handler).serve_forever()


if __name__ == "__main__":
    main()
