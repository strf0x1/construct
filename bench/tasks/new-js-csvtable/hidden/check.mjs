// Scores a `csvtable` in the current directory by calling and running it.
import { spawnSync } from "node:child_process";
import { existsSync, readdirSync } from "node:fs";
import { pathToFileURL } from "node:url";
import path from "node:path";

const here = (p) => pathToFileURL(path.resolve(p)).href;
const same = (a, b) => JSON.stringify(a) === JSON.stringify(b);
const cli = (input, ...args) => {
  const r = spawnSync("node", ["bin/csvtable.js", ...args], { input, encoding: "utf8", timeout: 30000 });
  return [r.status, r.stdout, r.stderr.trim()];
};
const CSV = 'name,qty,note\nkiwi,12,"a|b"\napple,3,"two\nlines"\nfig,3,"say ""hi"", ok"\n';

const groups = {
  async parse() {
    const { parse } = await import(here("src/csv.js"));
    return (
      same(parse(CSV), [["name", "qty", "note"], ["kiwi", "12", "a|b"], ["apple", "3", "two\nlines"], ["fig", "3", 'say "hi", ok']]) &&
      same(parse("a,b\r\n1,\r\n"), [["a", "b"], ["1", ""]]) &&
      same(parse(""), []) &&
      same(parse('x\n"1\r\n2",'), [["x"], ["1\r\n2", ""]])
    );
  },
  async render() {
    const { render } = await import(here("src/table.js"));
    const got = render([["name", "qty", "n"], ["apple", "3", "a|b"], ["kiwi", "12", "x\ny"]], ["l", "r", "c"]);
    const wide = ["| name  | qty |   n    |", "| :---- | --: | :----: |", "| apple |   3 |  a\\|b  |", "| kiwi  |  12 | x<br>y |"];
    let threw = "";
    try {
      render([["a", "b"], ["1", "2"], ["1"]]);
    } catch (e) {
      threw = e.message;
    }
    return got === wide.join("\n") && render([["a"], ["b"]]) === "| a   |\n| --- |\n| b   |" && threw === "row 3 has 1 fields, expected 2";
  },
  cli() {
    const [rc, out] = cli("a,b\nx,10\n", "--align", ",r");
    return rc === 0 && out === "| a   |   b |\n| --- | --: |\n| x   |  10 |\n";
  },
  errors() {
    const [rc, out, err] = cli("a,b\n1\n");
    return rc === 1 && out === "" && err === "error: row 2 has 1 fields, expected 2";
  },
  sort() {
    const rows = (out) => out.trim().split("\n").slice(2).map((l) => l.split("|")[1].trim());
    const [rc, out] = cli(CSV, "--sort", "qty");
    const [rc2, out2] = cli(CSV, "--sort", "qty:desc");
    const [rc3, out3] = cli("k,v\nb,10\na,9\nc,x\n", "--sort", "v");
    const [rc4, , err4] = cli(CSV, "--sort", "price");
    return (
      rc === 0 && same(rows(out), ["apple", "fig", "kiwi"]) &&
      rc2 === 0 && same(rows(out2), ["kiwi", "apple", "fig"]) &&
      rc3 === 0 && same(rows(out3), ["b", "a", "c"]) &&
      rc4 === 1 && err4 === "error: no column price"
    );
  },
  own_tests() {
    const has = (d) => existsSync(d) && readdirSync(d, { recursive: true }).some((f) => /\.(test|spec)\.[cm]?js$|^test.*\.[cm]?js$/.test(path.basename(String(f))));
    if (!has("test") && !has("tests")) return false;
    const r = spawnSync("node", ["--test"], { encoding: "utf8", timeout: 120000 });
    return r.status === 0 && !/# tests 0\b/.test(r.stdout);
  },
};

const verdict = {};
for (const [name, fn] of Object.entries(groups)) {
  try {
    verdict[name] = Boolean(await fn());
  } catch {
    verdict[name] = false;
  }
}
console.log(JSON.stringify(verdict));
