// The world of a Scenario at minute 0, built in WASM (Issue #249).
//
// Usage: node native/wasm/tests/node_initial.mjs PKG_DIR
//   PKG_DIR: the output of `wasm-bindgen --target nodejs` for sheltersim_wasm.wasm.
//
// For each Scenario of native/core/tests/data/initial_worlds.json (tests/support/core_initial.py),
// `initialEntry` of its canonical JSON must be the entry Python exports of its world at minute 0
// (the sha256 of its bytes), and that entry, run for the fixture's ticks, must reach the native
// path's world (the digests of its parts and of the metric samples), as
// native/core/tests/initial.rs checks in native code. What is not a Scenario is a JS error, and
// configs the core's table lacks are an error that says "unsupported: ".

import { readFileSync } from "node:fs";
import { createRequire } from "node:module";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import assert from "node:assert/strict";

const pkg = process.argv[2];
if (!pkg) {
  console.error("usage: node node_initial.mjs PKG_DIR");
  process.exit(2);
}
const require = createRequire(import.meta.url);
const wasm = require(join(resolve(pkg), "sheltersim_wasm.js"));

const here = dirname(fileURLToPath(import.meta.url));
const fixture = JSON.parse(readFileSync(join(here, "..", "..", "core", "tests", "data", "initial_worlds.json"), "utf8"));
const encode = (text) => new TextEncoder().encode(text);

let failures = 0;
for (const [name, world] of Object.entries(fixture.initial_worlds)) {
  const wanted = world.expected;
  const started = performance.now();
  const entry = wasm.initialEntry(encode(world.scenario));
  const built = (performance.now() - started) / 1000;
  const problems = [];
  if (entry.length !== wanted.entry_bytes) problems.push(`entry has ${entry.length} bytes, not ${wanted.entry_bytes}`);
  if (wasm.sha256Hex(entry) !== wanted.entry_sha256) problems.push("the entry is not the one Python exports");
  const scenarioBytes = encode(world.scenario);
  const versions = JSON.parse(wasm.modelVersions(scenarioBytes));
  const wantedVersions = wanted.model_versions;
  if (Object.keys(versions).length !== Object.keys(wantedVersions).length || Object.entries(wantedVersions).some(([key, value]) => versions[key] !== value)) {
    problems.push("the model versions are not Python's");
  }
  if (wasm.sha256Hex(wasm.runEvidence(scenarioBytes)) !== wanted.evidence_sha256) problems.push("the evidence is not Python's");
  const simulation = new wasm.Simulation(entry);
  const initial = JSON.parse(simulation.digests_json());
  for (const [part, digest] of Object.entries(wanted.initial)) {
    if (initial[part] !== digest) problems.push(`minute 0 ${part}: ${initial[part]} != ${digest}`);
  }
  const ran = simulation.advance(wanted.ticks);
  const digests = JSON.parse(simulation.digests_json());
  if (ran !== wanted.ticks) problems.push(`ran ${ran} ticks, not ${wanted.ticks}`);
  for (const [part, digest] of Object.entries(wanted.world)) {
    if (digests[part] !== digest) problems.push(`${part}: ${digests[part]} != ${digest}`);
  }
  if (simulation.metric_count() !== wanted.metric_count) problems.push(`metric_count ${simulation.metric_count()} != ${wanted.metric_count}`);
  if (simulation.metrics_digest() !== wanted.metrics) problems.push("the metric samples are not the native path's");
  const seconds = (performance.now() - started) / 1000;
  console.log(`${name}: ${problems.length === 0 ? "ok" : "FAILED"} (entry ${entry.length} bytes in ${built.toFixed(3)} s, ${ran} ticks, ${seconds.toFixed(2)} s in all)`);
  for (const problem of problems) console.log(`  ${problem}`);
  failures += problems.length;
  simulation.free();
}

// What is not a Scenario is a JS error, and the module still works after it.
for (const bad of ["", "not json", "{}", "[]", '{"seed": "1"}']) {
  assert.throws(() => wasm.initialEntry(encode(bad)), (error) => error instanceof Error && !error.message.startsWith("unsupported: "), bad);
}
const scenario = JSON.parse(fixture.initial_worlds.twenty.scenario);
scenario.model_configs.behavior.temperature = 0.3;
assert.throws(() => wasm.initialEntry(encode(JSON.stringify(scenario))), /^Error: unsupported: .*fingerprint/);
assert.equal(wasm.sha256Hex(wasm.initialEntry(encode(fixture.initial_worlds.twenty.scenario))), fixture.initial_worlds.twenty.expected.entry_sha256);

if (failures > 0) {
  console.error(`${failures} problems`);
  process.exit(1);
}
console.log("initial worlds: ok");
