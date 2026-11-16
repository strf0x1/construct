#!/usr/bin/env node
import { readFileSync } from "node:fs";
import { parse } from "../src/csv.js";
import { render, sortRows } from "../src/table.js";

const args = process.argv.slice(2);
let align = [], sort = null, file = 0;
for (let i = 0; i < args.length; i++) {
  if (args[i] === "--align") align = args[++i].split(",");
  else if (args[i] === "--sort") sort = args[++i];
  else file = args[i];
}
try {
  let rows = parse(readFileSync(file, "utf8"));
  if (sort) rows = sortRows(rows, sort);
  console.log(render(rows, align));
} catch (e) {
  console.error(`error: ${e.message}`);
  process.exit(1);
}
