// The offline intervention compiler in WASM answers as Python's (#303).
//
// Usage: node native/wasm/tests/node_interpret.mjs PKG_DIR
//   PKG_DIR: the output of `wasm-bindgen --target nodejs` for sheltersim_wasm.wasm.
//
// For each request of native/core/tests/data/offline_interpreter.json (tests/support/core_interpret.py),
// it calls `compileInterventionOffline` and checks the sha256 of the answer against what Python's
// `compile_natural_language` gave without an API key, or that it refuses where Python raised a
// ValueError, as native/core/tests/interpret.rs does in native code. The summaries hold floats, so
// the request is written from the file's own number text (a float such as 1.0 stays 1.0).

import { createHash } from "node:crypto";
import { readFileSync } from "node:fs";
import { createRequire } from "node:module";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const pkg = process.argv[2];
if (!pkg) {
  console.error("usage: node node_interpret.mjs PKG_DIR");
  process.exit(2);
}
const require = createRequire(import.meta.url);
const wasm = require(join(resolve(pkg), "sheltersim_wasm.js"));

const here = dirname(fileURLToPath(import.meta.url));
const text = readFileSync(join(here, "..", "..", "core", "tests", "data", "offline_interpreter.json"), "utf8");

/// A number keeps the text it had in the file.
class NumberText {
  constructor(source) {
    this.source = source;
  }
}
const fixture = JSON.parse(text, (key, value, context) => (typeof value === "number" ? new NumberText(context.source) : value));

function write(value) {
  if (value instanceof NumberText) return value.source;
  if (Array.isArray(value)) return `[${value.map(write).join(",")}]`;
  if (value !== null && typeof value === "object") {
    return `{${Object.entries(value).map(([key, item]) => `${JSON.stringify(key)}:${write(item)}`).join(",")}}`;
  }
  return JSON.stringify(value);
}

const encoder = new TextEncoder();
const started = performance.now();
let failures = 0;
for (const item of fixture.cases) {
  const request = write({
    text: item.text,
    branch_id: item.branch_id,
    selected_minute: item.minute,
    scenario_summary: fixture.summaries[item.summary],
  });
  const answer = wasm.compileInterventionOffline(encoder.encode(request));
  const got = answer === undefined || answer === null ? "invalid" : createHash("sha256").update(answer, "utf8").digest("hex");
  if (got !== item.answer) {
    failures += 1;
    console.error(`differs: ${item.summary} / ${JSON.stringify(item.text)}: expected ${item.answer}, got ${got}`);
  }
}
const seconds = ((performance.now() - started) / 1000).toFixed(2);
console.log(`${fixture.cases.length - failures} of ${fixture.cases.length} answers as Python's (${seconds} s)`);
process.exit(failures === 0 ? 0 : 1);
