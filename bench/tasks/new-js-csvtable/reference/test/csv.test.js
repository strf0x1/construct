import test from "node:test";
import assert from "node:assert/strict";
import { parse } from "../src/csv.js";

test("parses a quoted comma", () => assert.deepEqual(parse('a,"b,c"\n'), [["a", "b,c"]]));
