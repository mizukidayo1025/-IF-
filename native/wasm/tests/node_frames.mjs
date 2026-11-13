// The frame of a minute in WASM is the native path's (#264).
//
// Usage: node native/wasm/tests/node_frames.mjs PKG_DIR
//   PKG_DIR: the output of `wasm-bindgen --target nodejs` for sheltersim_wasm.wasm.
//
// For each world of native/core/tests/data/frames.json (tests/support/core_frames.py: the worlds of
// tick_core.json and `arrivals`), it runs the world in WASM from minute 0 and checks, at each
// minute the fixture lists, the sha256 of `Simulation.frame` (the frame's canonical JSON bytes)
// and the world's digest (`stateDigest`) against what the native path's `world_frame` gave, as
// native/core/tests/frames.rs does in native code. A frame given a digest names it.

import { readFileSync } from "node:fs";
import { createRequire } from "node:module";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import assert from "node:assert/strict";

const pkg = process.argv[2];
if (!pkg) {
  console.error("usage: node node_frames.mjs PKG_DIR");
  process.exit(2);
}
const require = createRequire(import.meta.url);
const wasm = require(join(resolve(pkg), "sheltersim_wasm.js"));

const here = dirname(fileURLToPath(import.meta.url));
const data = (name) => readFileSync(join(here, "..", "..", "core", "tests", "data", `${name}.json`));
const fixture = JSON.parse(data("frames").toString("utf8"));
const files = {};

let failures = 0;
for (const [name, frames] of Object.entries(fixture.expected)) {
  const file = fixture.files[name];
  files[file] ??= data(file);
  const started = performance.now();
  const simulation = wasm.Simulation.fromWorlds(files[file], name);
  const problems = [];
  let framed = 0;
  for (const [minute, frameDigest, stateHash] of frames) {
    simulation.advance(minute - simulation.minute());
    assert.equal(simulation.minute(), minute, name);
    const before = performance.now();
    const frame = simulation.frame("run", "main", minute, undefined);
    framed += performance.now() - before;
    if (wasm.sha256Hex(frame) !== frameDigest) problems.push(`minute ${minute}: the frame is not the native path's`);
    if (simulation.stateDigest() !== stateHash) problems.push(`minute ${minute}: the digest is not the native path's`);
  }
  const last = frames[frames.length - 1][0];
  const named = JSON.parse(new TextDecoder().decode(simulation.frame("run", "main", last, "0".repeat(64))));
  if (named.state_hash !== "0".repeat(64)) problems.push("a frame given a digest does not name it");
  const verdict = problems.length === 0 ? "ok" : "FAILED";
  const seconds = (performance.now() - started) / 1000;
  console.log(`${name}: ${verdict} (${frames.length} frames to minute ${last} in ${seconds.toFixed(2)} s, frames ${framed.toFixed(0)} ms)`);
  for (const problem of problems) console.log(`  ${problem}`);
  failures += problems.length;
  simulation.free();
}

// A minute that is not a whole number of minutes is an error, not a crash.
const again = wasm.Simulation.fromWorlds(data("tick_core"), "small_replay");
assert.throws(() => again.frame("run", "main", 1.5, undefined), Error);
assert.throws(() => again.frame("run", "main", -1, undefined), Error);
again.free();

if (failures > 0) {
  console.error(`${failures} problem(s)`);
  process.exit(1);
}
