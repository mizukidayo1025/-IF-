// The WASM build of the core reaches the native path's worlds (Issue #244, PR 4-4).
//
// Usage: node native/wasm/tests/node_digests.mjs PKG_DIR
//   PKG_DIR: the output of `wasm-bindgen --target nodejs` for sheltersim_wasm.wasm.
//
// For each world of native/core/tests/data/tick_core.json (the worlds of tests/support/core_tick.py,
// at minute 0), it runs the fixture's ticks in WASM and checks the digests of the world's parts and
// of the metric samples against the ones the native path gave (`expected`), as
// native/core/tests/tick_core.rs does in native code. It also checks that what is not an exported
// world is an error, not a crash. It prints each world's time and the WASM memory's size.

import { readFileSync } from "node:fs";
import { createRequire } from "node:module";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import assert from "node:assert/strict";

const pkg = process.argv[2];
if (!pkg) {
  console.error("usage: node node_digests.mjs PKG_DIR");
  process.exit(2);
}
const require = createRequire(import.meta.url);
const wasm = require(join(resolve(pkg), "sheltersim_wasm.js"));

const here = dirname(fileURLToPath(import.meta.url));
const fixturePath = join(here, "..", "..", "core", "tests", "data", "tick_core.json");
const raw = readFileSync(fixturePath);
const fixture = JSON.parse(raw.toString("utf8"));

const memoryBytes = () => wasm.memoryBytes();

let failures = 0;
for (const [name, entry] of Object.entries(fixture.worlds)) {
  const wanted = entry.expected;
  const started = performance.now();
  const simulation = wasm.Simulation.fromWorlds(raw, name);
  assert.equal(simulation.minute(), 0, name);
  const ran = simulation.advance(wanted.ticks);
  const seconds = (performance.now() - started) / 1000;
  const digests = JSON.parse(simulation.digests_json());
  const problems = [];
  if (ran !== wanted.ticks) problems.push(`ran ${ran} ticks, not ${wanted.ticks}`);
  for (const [part, digest] of Object.entries(wanted.world)) {
    if (digests[part] !== digest) problems.push(`${part}: ${digests[part]} != ${digest}`);
  }
  if (Object.keys(digests).length !== Object.keys(wanted.world).length) {
    problems.push(`parts ${Object.keys(digests)} != ${Object.keys(wanted.world)}`);
  }
  if (simulation.metric_count() !== wanted.metric_count) problems.push(`metric_count ${simulation.metric_count()} != ${wanted.metric_count}`);
  if (simulation.metrics_digest() !== wanted.metrics) problems.push(`metrics ${simulation.metrics_digest()} != ${wanted.metrics}`);
  const verdict = problems.length === 0 ? "ok" : "FAILED";
  console.log(`${name}: ${verdict} (${ran} ticks in ${seconds.toFixed(2)} s, memory ${(memoryBytes() / 2 ** 20).toFixed(1)} MiB, contracts ${simulation.contracts_json()})`);
  for (const problem of problems) console.log(`  ${problem}`);
  failures += problems.length;
  simulation.free();
}

// What is not an exported world is a JS error, and the module still works after it.
for (const bad of ["not json", "{}", "[]", '{"world": 1, "slots": [], "run": {}}']) {
  assert.throws(() => new wasm.Simulation(new TextEncoder().encode(bad)), Error, bad);
}
assert.throws(() => wasm.Simulation.fromWorlds(raw, "no_such_world"), Error);
const again = wasm.Simulation.fromWorlds(raw, "small_replay");
assert.equal(again.advance(3), 3);
again.free();

if (failures > 0) {
  console.error(`${failures} problem(s)`);
  process.exit(1);
}
console.log("all worlds reach the native path's digests");
