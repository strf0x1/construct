// The reference answer: proves the hidden tests can be passed (bench/test_greenfield.py).
export function parse(text) {
  const rows = [];
  let row = [], field = "", quoted = false, i = 0;
  while (i < text.length) {
    const c = text[i];
    if (quoted) {
      if (c === '"' && text[i + 1] === '"') (field += '"'), (i += 2);
      else if (c === '"') (quoted = false), i++;
      else (field += c), i++;
    } else if (c === '"') (quoted = true), i++;
    else if (c === ",") row.push(field), (field = ""), i++;
    else if (c === "\n" || (c === "\r" && text[i + 1] === "\n")) {
      row.push(field), rows.push(row), (row = []), (field = "");
      i += c === "\r" ? 2 : 1;
    } else (field += c), i++;
  }
  if (field !== "" || row.length) row.push(field), rows.push(row);
  return rows;
}
