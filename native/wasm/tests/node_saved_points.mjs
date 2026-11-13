// Saved points of a Run in the browser and seeking to a minute (Issue #251; js/run_store.mjs).
//
// Usage: node native/wasm/tests/node_saved_points.mjs PKG_DIR
//   PKG_DIR: the output of `wasm-bindgen --target nodejs` for sheltersim_wasm.wasm.
//
// For each world of native/core/tests/data/tick_core.json, it runs the world and stores it in memory
// (`memoryBackend`) with saved points every `every` minutes (small_replay is one day, saved every 6
// hours), and runs it again without storing, keeping the digests at each minute it seeks to. Then it
// seeks the stored Run to each saved point, to minutes between them and before the first, and to
// the end: the world has the uninterrupted Run's digests there. Seeking to a minute in the middle
// and going on to the end gives the native path's digests. A Run of the earlier format (v1, without
// saved points) seeks by computing from minute 0; a changed saved point, a minute out of the Run,
// and a format this code does not know are errors.

import { readFileSync } from "node:fs";
import { createRequire } from "node:module";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import assert from "node:assert/strict";

const pkg = process.argv[2];
if (!pkg) {
  console.error("usage: node node_saved_points.mjs PKG_DIR");
  process.exit(2);
}
const require = createRequire(import.meta.url);
const wasm = require(join(resolve(pkg), "sheltersim_wasm.js"));
const here = dirname(fileURLToPath(import.meta.url));
const raw = readFileSync(join(here, "..", "..", "core", "tests", "data", "tick_core.json"));
const fixture = JSON.parse(raw.toString("utf8"));
const store = await import(join(here, "..", "js", "run_store.mjs"));
const encoder = new TextEncoder();

const digestsOf = (simulation) => ({ parts: simulation.part_digests_json(), metrics: simulation.metrics_digest(), contracts: simulation.contracts_json() });

let failures = 0;
const check = (ok, message) => {
  if (!ok) {
    console.log(`  ${message}`);
    failures += 1;
  }
};

for (const [name, every] of [["small_replay", 360], ["generated", 60]]) {
  const wanted = fixture.worlds[name].expected;
  const end = wanted.ticks;
  const targets = new Set([0, 1, every - 1, every, every + 1, Math.floor(end / 2) + 7, end - every, end - 1, end]);
  for (let minute = every; minute < end; minute += every) targets.add(minute);
  // The uninterrupted Run's digests at each target.
  const plain = wasm.Simulation.fromWorlds(raw, name);
  const at = new Map();
  if (targets.has(0)) at.set(0, digestsOf(plain));
  while (plain.minute() < end) {
    plain.advance(1);
    if (targets.has(plain.minute())) at.set(plain.minute(), digestsOf(plain));
  }
  plain.free();

  const backend = store.memoryBackend();
  const ran = await store.runAndStore({ wasm, exportBytes: raw, world: name, backend, ticks: 50, saveEvery: every });
  const back = await store.readAndVerify({ wasm, backend });
  check(back.verified && back.complete && !back.torn, `${name}: read back ${back.mismatches}`);
  const saved = ran.saved.map((point) => point.minute);
  const wantedSaved = [];
  for (let minute = every; minute < end; minute += every) wantedSaved.push(minute);
  wantedSaved.push(end);
  assert.deepEqual(saved, wantedSaved, name);
  check(back.savedPoints === saved.length, `${name}: ${back.savedPoints} saved points read back`);

  const reader = await store.RunReader.open({ wasm, backend });
  assert.deepEqual(reader.savedPoints().map((line) => line.minute), saved, name);
  let restored = 0;
  for (const minute of [...targets].sort((a, b) => a - b)) {
    const sought = await reader.seek(minute);
    const from = Math.max(0, ...saved.filter((point) => point <= minute));
    check(sought.from === from, `${name} at ${minute}: from ${sought.from}, not ${from}`);
    check(sought.simulation.minute() === minute, `${name} at ${minute}: the world is at ${sought.simulation.minute()}`);
    check(JSON.stringify(digestsOf(sought.simulation)) === JSON.stringify(at.get(minute)), `${name} at ${minute}: not the Run's digests`);
    if (sought.from > 0) restored += 1;
    sought.simulation.free();
  }
  check(restored >= saved.length, `${name}: ${restored} seeks restored a saved point`);
  // From a minute in the middle to the end: the native path's digests.
  const middle = await reader.seek(Math.floor(end / 2) + 7);
  middle.simulation.advance(end);
  const digests = JSON.parse(middle.simulation.digests_json());
  for (const [part, digest] of Object.entries(wanted.world)) check(digests[part] === digest, `${name}: ${part} from the middle`);
  check(middle.simulation.metrics_digest() === wanted.metrics, `${name}: metrics from the middle`);
  middle.simulation.free();
  // Minutes out of the Run are errors.
  await assert.rejects(reader.seek(-1), Error);
  await assert.rejects(reader.seek(end + 1), Error);
  await assert.rejects(reader.seek(1.5), Error);
  reader.close();

  // A changed saved point is an error.
  const lines = store.journalLines(wasm, new TextDecoder().decode(backend.files.get("journal.jsonl"))).lines;
  const point = lines.find((line) => line.kind === "saved" && line.minute === saved[1]);
  const data = backend.files.get("data.pack");
  const changed = data.slice();
  changed[point.offset + 2] ^= 0xff;
  backend.files.set("data.pack", changed);
  const broken = await store.RunReader.open({ wasm, backend });
  await assert.rejects(broken.seek(saved[1] + 1), Error, `${name}: a changed saved point`);
  broken.close();
  backend.files.set("data.pack", data);
  console.log(`${name}: ${failures === 0 ? "ok" : "FAILED"} (${saved.length} saved points, ${targets.size} minutes sought, saving ${ran.saveMs.toFixed(0)} ms)`);
}

// A Run of format v1 (#252: no saved points) reads back, and seeks by computing from minute 0.
{
  const backend = store.memoryBackend();
  await store.runAndStore({ wasm, exportBytes: raw, world: "small_replay", backend, ticks: 60, saveEvery: 0 });
  const journal = new TextDecoder().decode(backend.files.get("journal.jsonl")).split("\n");
  const rewrite = (format) => {
    const first = JSON.parse(journal[0].slice(0, journal[0].lastIndexOf("\t")));
    first.format = format;
    const json = JSON.stringify(first);
    backend.files.set("journal.jsonl", encoder.encode([`${json}\t${wasm.sha256Hex(encoder.encode(json))}`, ...journal.slice(1)].join("\n")));
  };
  rewrite("sheltersim-browser-run-v1");
  const back = await store.readAndVerify({ wasm, backend });
  check(back.verified && back.savedPoints === 0, `v1: read back ${back.mismatches}`);
  const reader = await store.RunReader.open({ wasm, backend });
  const sought = await reader.seek(700);
  check(sought.from === 0 && sought.simulation.minute() === 700, `v1: sought from ${sought.from} to ${sought.simulation.minute()}`);
  sought.simulation.free();
  reader.close();
  rewrite("sheltersim-browser-run-v9");
  await assert.rejects(store.readAndVerify({ wasm, backend }), Error, "an unknown format");
  await assert.rejects(store.RunReader.open({ wasm, backend }), Error, "an unknown format");
  console.log(`format v1: ${failures === 0 ? "ok" : "FAILED"}`);
}

// What is not a saved point is a JS error, and the module still works after it.
for (const bad of ["", "SHSP", "not a saved point"]) {
  assert.throws(() => new wasm.Restore(encoder.encode(bad)), Error, bad);
}

if (failures > 0) {
  console.error(`${failures} problem(s)`);
  process.exit(1);
}
console.log("every seek reaches the uninterrupted Run's digests");
