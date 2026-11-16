export function render(rows, align = []) {
  const cells = rows.map((r) => r.map((c) => c.replaceAll("|", "\\|").replace(/\r?\n/g, "<br>")));
  cells.forEach((r, i) => {
    if (r.length !== cells[0].length) throw new Error(`row ${i + 1} has ${r.length} fields, expected ${cells[0].length}`);
  });
  const width = cells[0].map((_, j) => Math.max(3, ...cells.map((r) => r[j].length)));
  const pad = (c, j) => {
    const n = width[j] - c.length;
    if (align[j] === "r") return " ".repeat(n) + c;
    if (align[j] === "c") return " ".repeat(Math.floor(n / 2)) + c + " ".repeat(Math.ceil(n / 2));
    return c + " ".repeat(n);
  };
  const sep = width.map((w, j) => (align[j] === "l" ? ":" + "-".repeat(w - 1) : align[j] === "r" ? "-".repeat(w - 1) + ":" : align[j] === "c" ? ":" + "-".repeat(w - 2) + ":" : "-".repeat(w)));
  const line = (r) => `| ${r.join(" | ")} |`;
  return [line(cells[0].map(pad)), line(sep), ...cells.slice(1).map((r) => line(r.map(pad)))].join("\n");
}

export function sortRows(rows, spec) {
  const [name, dir] = spec.split(":");
  const j = rows[0].indexOf(name);
  if (j < 0) throw new Error(`no column ${name}`);
  const data = rows.slice(1);
  const numeric = data.every((r) => r[j].trim() !== "" && Number.isFinite(Number(r[j])));
  const cmp = numeric ? (a, b) => Number(a[j]) - Number(b[j]) : (a, b) => (a[j] < b[j] ? -1 : a[j] > b[j] ? 1 : 0);
  return [rows[0], ...data.sort((a, b) => (dir === "desc" ? cmp(b, a) : cmp(a, b)))];
}
