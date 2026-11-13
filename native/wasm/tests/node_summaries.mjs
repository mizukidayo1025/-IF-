// The summary of a world for the intervention's reading, in WASM, is Python's (#302).
//
// Usage: node native/wasm/tests/node_summaries.mjs PKG_DIR
//   PKG_DIR: the output of `wasm-bindgen --target nodejs` for sheltersim_wasm.wasm.
//
// For each world of native/core/tests/data/summaries.json (tests/support/core_summaries.py), it
// runs the world in WASM from minute 0 and checks, at each minute the fixture lists, the sha256 of
// `Simulation.scenarioSummary` (the summary's canonical JSON bytes) against that of the summary
// Python's `scenario_summary_from_world` made, as native/core/tests/summaries.rs does in native
// code. Reading a summary changes nothing.

import { readFileSync } from "node:fs";
import { createRequire } from "node:module";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import assert from "node:assert/strict";

const pkg = process.argv[2];
if (!pkg) {
  console.error("usage: node node_summaries.mjs PKG_DIR");
  process.exit(2);
}
const require = createRequire(import.meta.url);
const wasm = require(join(resolve(pkg), "sheltersim_wasm.js"));

const here = dirname(fileURLToPath(import.meta.url));
const data = (name) => readFileSync(join(here, "..", "..", "core", "tests", "data", `${name}.json`));
const fixture = JSON.parse(data("summaries").toString("utf8"));
const files = {};

let failures = 0;
for (const [name, summaries] of Object.entries(fixture.expected)) {
  const file = fixture.files[name];
  files[file] ??= data(file);
  const simulation = wasm.Simulation.fromWorlds(files[file], name);
  const problems = [];
  for (const [minute, digest] of summaries) {
    simulation.advance(minute - simulation.minute());
    assert.equal(simulation.minute(), minute, name);
    const before = simulation.digests_json();
    const summary = simulation.scenarioSummary();
    if (wasm.sha256Hex(summary) !== digest) problems.push(`minute ${minute}: the summary is not Python's`);
    if (simulation.digests_json() !== before) problems.push(`minute ${minute}: reading the summary changed the world`);
  }
  const verdict = problems.length === 0 ? "ok" : "FAILED";
  console.log(`${name}: ${verdict} (${summaries.length} summaries to minute ${summaries[summaries.length - 1][0]})`);
  for (const problem of problems) console.log(`  ${problem}`);
  failures += problems.length;
  simulation.free();
}

if (failures > 0) {
  console.error(`${failures} problem(s)`);
  process.exit(1);
}
