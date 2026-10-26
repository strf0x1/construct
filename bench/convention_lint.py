#!/usr/bin/env python3
"""Check a change against the AGENTS.md conventions a machine can check.

    convention_lint.py [--repo DIR] [--base REV] [--head REV] [--clippy] [PATH ...]

Compares `base` (default HEAD) with `head` (default: the work tree, untracked
files included) and prints one JSON object: a finding per broken convention,
counts per rule, and the size of the diff. It only judges what the change
added, so a tree's old debt never counts against the change that touches it.

Rules (severity `violation` unless noted):
  unwrap            a new `.unwrap()` outside tests; `.lock().unwrap()` with
                    a comment on its line or the line above is allowed
  wire-name         a tool's wire name (`const NAME` in crates/construct/src/tools/)
                    as a string literal outside that directory, outside tests;
                    a plain-word name ("read") only where code matches on it
  module-doc        a new module that does not start with a `//!` paragraph
  file-length       (info) a source file that crosses the 300-line soft
                    ceiling (tui/view.rs and tools/mod.rs may run longer)
  dependency        a new crate dependency not taken from [workspace.dependencies]
  workspace-dependency  (info) a new entry in [workspace.dependencies]
  snap              a changed or deleted golden `.snap` file (a new one is not)
  clippy-pedantic   (info, --clippy) a `clippy::pedantic` warning on an added line

Stdlib only: the bench runs it as a scorer in a work dir with no venv.
"""

import argparse
import json
import re
import subprocess
import sys
from pathlib import Path

CEILING = 300
LONG_OK = ("crates/construct/src/tui/view.rs", "crates/construct/src/tools/mod.rs")
TOOLS_DIR = "crates/construct/src/tools/"
RULES = ("unwrap", "wire-name", "module-doc", "file-length", "dependency", "workspace-dependency", "snap", "clippy-pedantic")
# `info` is counted but is not a violation: the 300-line ceiling is soft
# (AGENTS.md: split by concern, never by line count).
SEVERITY = {"workspace-dependency": "info", "clippy-pedantic": "info", "file-length": "info"}
# Lockfiles and goldens are generated: they count as files changed, not lines.
GENERATED = re.compile(r"(^|/)Cargo\.lock$|\.snap$")
NAME_CONST = re.compile(r'const\s+NAME\s*:\s*&(?:\'static\s+)?str\s*=\s*"([^"]+)"')
STRING = re.compile(r'"(?:\\.|[^"\\])*"')
DEP_SECTION = re.compile(r"^\[(?:target\..+\.)?(?:dev-|build-)?dependencies\]$")
DEP_KEY = re.compile(r"^([A-Za-z0-9_-]+)(\.workspace)?\s*=")


def git(repo, *args, check=True):
    r = subprocess.run(["git", *args], cwd=repo, capture_output=True, text=True)
    if check and r.returncode != 0:
        raise RuntimeError(f"git {' '.join(args)}: {r.stderr.strip()}")
    return r


class Tree:
    """File contents at a revision, or in the work tree when rev is None."""

    def __init__(self, repo: Path, rev):
        self.repo, self.rev = repo, rev

    def read(self, path):
        if self.rev is None:
            p = self.repo / path
            return p.read_text(errors="replace") if p.is_file() else None
        r = git(self.repo, "show", f"{self.rev}:{path}", check=False)
        return r.stdout if r.returncode == 0 else None

    def files(self, prefix):
        if self.rev is None:
            out = git(self.repo, "ls-files", "--cached", "--others", "--exclude-standard", "--", prefix).stdout
        else:
            out = git(self.repo, "ls-tree", "-r", "--name-only", self.rev, "--", prefix).stdout
        return [l for l in out.splitlines() if l]


def parse_diff(text):
    """{path: {"status": A|M|D, "added": {line_no: text}, "removed": n}} from
    `git diff -U0`."""
    files, cur, new_line = {}, None, 0
    for line in text.splitlines():
        if line.startswith("diff --git "):
            path = line.split(" b/", 1)[1]
            cur = files.setdefault(path, {"status": "M", "added": {}, "removed": 0})
        elif cur is None:
            continue
        elif line.startswith("new file mode"):
            cur["status"] = "A"
        elif line.startswith("deleted file mode"):
            cur["status"] = "D"
        elif line.startswith("@@"):
            m = re.match(r"@@ -\d+(?:,\d+)? \+(\d+)(?:,\d+)? @@", line)
            new_line = int(m.group(1))
        elif line.startswith("+++") or line.startswith("---"):
            continue
        elif line.startswith("+"):
            cur["added"][new_line] = line[1:]
            new_line += 1
        elif line.startswith("-"):
            cur["removed"] += 1
    return files


def changes(repo: Path, base: str, head, paths=()):
    args = ["diff", "--no-color", "--no-renames", "--no-ext-diff", "--src-prefix=a/", "--dst-prefix=b/", "-U0", base] + ([head] if head else [])
    files = parse_diff(git(repo, *args, "--", *paths).stdout)
    if head is None:  # untracked files are whole new files
        for path in git(repo, "ls-files", "--others", "--exclude-standard", "--", *paths).stdout.splitlines():
            p = repo / path
            try:
                text = p.read_text()
            except (UnicodeDecodeError, OSError):
                files[path] = {"status": "A", "added": {}, "removed": 0}
                continue
            files[path] = {"status": "A", "added": dict(enumerate(text.splitlines(), 1)), "removed": 0}
    return files


def strip_code(line):
    """The line without its `//` comment and with string contents blanked, so
    braces and `.unwrap()` inside strings or comments don't count."""
    line = STRING.sub('""', line)
    line = re.sub(r"'(?:\\.|[^'\\])'", "' '", line)
    return line.split("//", 1)[0]


def test_lines(text):
    """Line numbers (1-based) inside `#[cfg(test)]` items."""
    lines = text.splitlines()
    out, i = set(), 0
    while i < len(lines):
        if lines[i].strip().startswith("#[cfg(test)]"):
            depth, opened, j = 0, False, i
            while j < len(lines):
                code = strip_code(lines[j])
                depth += code.count("{") - code.count("}")
                opened = opened or "{" in code
                out.add(j + 1)
                if (opened and depth <= 0) or (not opened and code.rstrip().endswith(";") and j > i):
                    break
                j += 1
            i = j + 1
        else:
            i += 1
    return out


def is_test_path(path):
    p = "/" + path
    return "/tests/" in p or p.endswith("/tests.rs") or "/benches/" in p or "/examples/" in p


def wire_names(tree: Tree):
    names = set()
    for f in tree.files(TOOLS_DIR):
        if f.endswith(".rs"):
            names.update(NAME_CONST.findall(tree.read(f) or ""))
    return names


def retyped_names(line, names):
    """Tool names written as literals on this line. A name that is also a
    plain word ("read", "shell") counts only where code matches on it (a match
    arm or a comparison); a distinctive one ("journal_query") counts anywhere."""
    code = line.split("//", 1)[0]
    out = []
    for m in STRING.finditer(code):
        name = m.group(0)[1:-1]
        if name not in names:
            continue
        before, after = code[: m.start()].rstrip(), code[m.end() :].lstrip()
        matched = after.startswith(("=>", "|", "==", "!=")) or before.endswith(("==", "!=", "|", "Some("))
        if "_" in name or matched:
            out.append(name)
    return out


def dep_names(text):
    """{(section, name): line_text} for every dependency in a Cargo.toml."""
    out, section = {}, None
    for line in (text or "").splitlines():
        s = line.strip()
        if s.startswith("["):
            section = s
            continue
        m = DEP_KEY.match(s)
        if m and section and (DEP_SECTION.match(section) or section == "[workspace.dependencies]"):
            out[(section, m.group(1))] = s
    return out


def lint(repo: Path, base="HEAD", head=None, clippy=False, paths=()):
    """The findings, counts and diff size; `paths` limits it to those pathspecs."""
    repo = Path(repo).resolve()
    files = changes(repo, base, head, paths)
    before, after = Tree(repo, base), Tree(repo, head)
    names = wire_names(after)
    findings = []

    def add(rule, path, line, message):
        findings.append({"rule": rule, "severity": SEVERITY.get(rule, "violation"), "file": path, "line": line, "message": message})

    for path, ch in sorted(files.items()):
        if path.endswith(".snap"):
            if ch["status"] != "A":  # a new test's new golden is not an edit
                add("snap", path, None, "golden snapshot changed: only `cargo xtask snapshots` may write one, and only for an intended change")
            continue
        if ch["status"] == "D":
            continue
        if path.endswith("Cargo.toml"):
            old, new = dep_names(before.read(path)), dep_names(after.read(path))
            old_names = {n for _, n in old}
            for (section, name), line in new.items():
                if name in old_names:
                    continue
                if section == "[workspace.dependencies]":
                    add("workspace-dependency", path, None, f"new workspace dependency `{name}`: it must remove more code than it adds")
                elif "workspace" not in line:
                    add("dependency", path, None, f"new dependency `{name}` is not pinned in [workspace.dependencies]")
            continue
        if not path.endswith(".rs"):
            continue
        text = after.read(path) or ""
        lines = text.splitlines()
        tests = is_test_path(path)
        in_test = test_lines(text)
        for n, raw in sorted(ch["added"].items()):
            if tests or n in in_test:
                continue
            code = strip_code(raw)
            if ".unwrap()" in code:
                above = lines[n - 2] if n >= 2 else ""
                lock_ok = ".lock().unwrap()" in code and code.count(".unwrap()") == 1 and ("//" in raw or above.strip().startswith("//"))
                if not lock_ok:
                    add("unwrap", path, n, "`.unwrap()` outside tests: return an error (Result<T, String> at the tool boundary)")
            if not path.startswith(TOOLS_DIR):
                for name in retyped_names(raw, names):
                    add("wire-name", path, n, f'tool wire name "{name}" retyped as a literal: reference the tool\'s NAME const')
        if ch["status"] == "A" and "/src/" in "/" + path and not tests and not path.endswith(("/main.rs", "/build.rs")):
            first = next((l for l in lines if l.strip() and not l.lstrip().startswith("#![")), "")
            if not first.lstrip().startswith("//!"):
                add("module-doc", path, 1, "new module does not start with a `//!` paragraph")
        if "/src/" in "/" + path and not tests and path not in LONG_OK:
            old_n = len((before.read(path) or "").splitlines())
            if old_n <= CEILING < len(lines):
                add("file-length", path, None, f"{old_n} -> {len(lines)} lines, past the {CEILING}-line soft ceiling: split by concern")

    if clippy:
        if head is not None:
            raise SystemExit("--clippy lints the work tree; it cannot be combined with --head")
        findings += clippy_findings(repo, files)

    counts = dict.fromkeys(RULES, 0)  # every rule, so runs line up in `bench.py compare`
    for f in findings:
        counts[f["rule"]] = counts.get(f["rule"], 0) + 1
    code_files = {p: c for p, c in files.items() if not GENERATED.search(p)}
    return {
        "base": base,
        "head": head or "worktree",
        "findings": findings,
        "counts": counts,
        "violations": sum(1 for f in findings if f["severity"] == "violation"),
        "diff": {
            "files": len(files),
            "added": sum(len(c["added"]) for c in code_files.values()),
            "removed": sum(c["removed"] for c in code_files.values()),
            "test_added": sum(len(c["added"]) for p, c in code_files.items() if is_test_path(p)),
        },
    }


def clippy_findings(repo: Path, files):
    """Pedantic warnings whose primary span is a line this change added."""
    added = {(p, n) for p, c in files.items() for n in c["added"]}
    cmd = ["cargo", "clippy", "--workspace", "--all-targets", "--message-format=json", "--", "-W", "clippy::pedantic"]
    r = subprocess.run(cmd, cwd=repo, capture_output=True, text=True)
    out, seen = [], set()
    for line in r.stdout.splitlines():
        try:
            msg = json.loads(line)
        except ValueError:
            continue
        m = msg.get("message") or {}
        code = (m.get("code") or {}).get("code") or ""
        if msg.get("reason") != "compiler-message" or m.get("level") != "warning" or not code.startswith("clippy::"):
            continue
        for span in m.get("spans") or []:
            key = (span.get("file_name"), span.get("line_start"), code)
            if span.get("is_primary") and key[:2] in added and key not in seen:
                seen.add(key)
                out.append({"rule": "clippy-pedantic", "severity": "info", "file": key[0], "line": key[1], "message": f"{code}: {m.get('message')}"})
    if r.returncode != 0 and not out:
        out.append({"rule": "clippy-pedantic", "severity": "info", "file": None, "line": None, "message": "clippy did not run: " + r.stderr.strip()[-300:]})
    return out


def main(argv=None):
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--repo", type=Path, default=Path("."))
    ap.add_argument("--base", default="HEAD")
    ap.add_argument("--head", default=None, help="a revision; default: the work tree")
    ap.add_argument("--clippy", action="store_true", help="also run clippy -W clippy::pedantic (builds the workspace)")
    ap.add_argument("paths", nargs="*", help="limit to these pathspecs (e.g. crates)")
    a = ap.parse_args(argv)
    json.dump(lint(a.repo, a.base, a.head, a.clippy, a.paths), sys.stdout, indent=1)
    print()


if __name__ == "__main__":
    main()
