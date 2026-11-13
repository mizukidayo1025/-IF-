// Builds a Project's bundle in Node with the browser's WASM and stores (Issue #265): the second
// half of scripts/build_browser_sample.py, which writes the inputs.
//
// Usage: node native/wasm/tools/build_project_bundle.mjs --pkg PKG_DIR --inputs DIR --output FILE
//          [--ticks 1440]
//   PKG_DIR  the output of `wasm-bindgen --target nodejs` for sheltersim_wasm.wasm
//   DIR      inputs.json (the records Python's server holds for the new Run, and their source),
//            scenario.json, evidence.json and export.json (the world at minute 0), as
//            build_browser_sample.py writes them
//   FILE     the bundle to write (js/project_store.mjs)
//
// It makes the Project in a store in memory with js/project_store.mjs and checks that its records
// are Python's, computes the branch in WASM for the whole clock and stores it as it goes
// (js/run_store.mjs), marks the Run completed, writes the bundle, and imports the bundle into an
// empty store to check that it reads back to the same answers and digests. It prints a summary.

import { readFileSync, writeFileSync } from "node:fs";
import { createRequire } from "node:module";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { parseArgs } from "node:util";
import assert from "node:assert/strict";

const { values: options } = parseArgs({
  options: { pkg: { type: "string" }, inputs: { type: "string" }, output: { type: "string" }, ticks: { type: "string", default: "1440" } },
});
if (!options.pkg || !options.inputs || !options.output) {
  console.error("usage: node build_project_bundle.mjs --pkg PKG_DIR --inputs DIR --output FILE [--ticks 1440]");
  process.exit(2);
}
const require = createRequire(import.meta.url);
const wasm = require(join(resolve(options.pkg), "sheltersim_wasm.js"));
const here = dirname(fileURLToPath(import.meta.url));
const store = await import(join(here, "..", "js", "project_store.mjs"));
const read = (name) => new Uint8Array(readFileSync(join(options.inputs, name)));
const inputs = JSON.parse(readFileSync(join(options.inputs, "inputs.json"), "utf8"));
if (inputs.kind !== "browser_sample_inputs" || inputs.schema_version !== 1) throw new Error("not the inputs of build_browser_sample.py");

const started = performance.now();
const tree = store.memoryTree();
const projectId = inputs.project.project_id;
const runId = inputs.run.run_id;
// The records are the ones Python's server holds for the same Project, Scenario and Run.
assert.deepEqual(await store.createProject(tree, projectId), inputs.project, "the Project's record");
const scenario = await store.saveScenario(tree, projectId, { scenarioBytes: read("scenario.json"), scenarioHash: inputs.scenario_record.scenario_hash });
assert.deepEqual(scenario, inputs.scenario_record, "the Scenario's record");
const run = await store.createRun(tree, projectId, {
  runId,
  scenarioId: inputs.run.scenario_id,
  scenarioVersion: inputs.run.scenario_version,
  seed: inputs.run.seed,
  modelVersions: inputs.run.model_versions,
  evidenceBytes: read("evidence.json"),
});
assert.deepEqual(run, inputs.run, "the Run's record");

let lastDay = -1;
const computed = await store.computeBranch({
  wasm,
  tree,
  projectId,
  runId,
  branchId: inputs.branch_id,
  run: {
    exportBytes: read("export.json"),
    ticks: Number(options.ticks),
    progress(minute, duration) {
      const day = Math.floor(minute / 1440);
      if (day !== lastDay) console.error(`minute ${minute} / ${duration}`);
      lastDay = day;
    },
  },
});
if (computed.record.status !== "completed") throw new Error(`the branch ended ${computed.record.status} at minute ${computed.record.computed_until}`);
await store.setRunStatus(tree, projectId, runId, "completed");

const bundle = await store.exportProject({ tree, projectId, source: inputs.source });
const bytes = new Uint8Array(await bundle.arrayBuffer());
writeFileSync(options.output, bytes);

// The bundle reads back into an empty store to the same answers and digests.
const again = store.memoryTree();
const imported = await store.importProject({ wasm, tree: again, bundle: bytes });
for (const route of ["projectResponse", "runsResponse"]) assert.equal(await store[route](again, projectId), await store[route](tree, projectId), route);
assert.equal(await store.runResponse(again, projectId, runId), await store.runResponse(tree, projectId, runId));
const [branch] = imported.branches;
assert.ok(branch.verified, "the imported branch reads back to its digests");
assert.deepEqual(branch.parts, computed.ran.end.parts);

const sizes = Object.fromEntries(Object.entries(computed.ran.sizes).map(([kind, size]) => [kind, size.stored]));
console.log(
  JSON.stringify(
    {
      output: resolve(options.output),
      bytes: bytes.length,
      sha256: await store.sha256Hex(bytes),
      project_id: projectId,
      run_id: runId,
      branch: computed.record,
      parts: branch.parts,
      metrics: branch.metrics,
      records: branch.records,
      samples: branch.samples,
      stored_bytes: sizes,
      tick_seconds: Math.round(computed.ran.tickMs) / 1000,
      total_seconds: Math.round(performance.now() - started) / 1000,
      wasm_memory_bytes: computed.ran.memoryBytes,
    },
    null,
    1,
  ),
);
