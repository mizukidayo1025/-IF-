// The Scenario text parsed in WASM as the server parses it without an API key (Issue #294).
//
// Usage: node native/wasm/tests/node_scenario_text.mjs PKG_DIR
//   PKG_DIR: the output of `wasm-bindgen --target nodejs` for sheltersim_wasm.wasm.
//
// For each case of native/core/tests/data/scenario_corpus.json (#293) that posts a text, the
// request `parseScenarioText` reads must be the one the Python server previewed (field origins
// compared without their order, which Python takes from a set), and a text missing a required
// field must miss the same fields. tests/unit/llm/test_region_prepare.py compares every field of
// the parse with Python's, through the PyO3 crate, on more texts.

import { readFileSync } from "node:fs";
import { createRequire } from "node:module";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import assert from "node:assert/strict";

const pkg = process.argv[2];
if (!pkg) {
  console.error("usage: node node_scenario_text.mjs PKG_DIR");
  process.exit(2);
}
const require = createRequire(import.meta.url);
const wasm = require(join(resolve(pkg), "sheltersim_wasm.js"));
const here = dirname(fileURLToPath(import.meta.url));
const corpus = JSON.parse(readFileSync(join(here, "..", "..", "core", "tests", "data", "scenario_corpus.json"), "utf8"));

const sorted = (object) => Object.fromEntries(Object.entries(object).sort(([a], [b]) => (a < b ? -1 : a > b ? 1 : 0)));
let compared = 0;
for (const [name, entry] of Object.entries(corpus.cases)) {
  const text = entry.body?.text;
  if (typeof text !== "string" || entry.body.request || !text.trim()) continue;
  const answer = entry.answer ?? {};
  let parsed;
  try {
    parsed = JSON.parse(wasm.parseScenarioText(text));
  } catch (error) {
    assert.ok(entry.status !== 200, `${name}: ${error.message}`);
    continue;
  }
  if (answer.request) {
    assert.deepEqual({ ...parsed.request, field_origins: sorted(parsed.request.field_origins) }, { ...answer.request, field_origins: sorted(answer.request.field_origins) }, name);
    assert.equal(parsed.source, "rules_fallback", name);
    assert.equal(parsed.input_sha256, wasm.sha256Hex(new TextEncoder().encode(text)), name);
  } else if (answer.missing_fields) {
    assert.equal(parsed.request, null, name);
    assert.deepEqual(parsed.missing_fields, answer.missing_fields, name);
    assert.deepEqual(parsed.warnings, answer.warnings, name);
  }
  compared += 1;
}
assert.ok(compared >= 25, `only ${compared} texts compared`);
assert.throws(() => wasm.parseScenarioText(""), /^Error: invalid: /);
assert.throws(() => wasm.parseScenarioText("あ".repeat(4001)), /^Error: invalid: /);
console.log(`scenario texts: ok (${compared} corpus texts)`);
