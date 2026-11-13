// Builds a Project's bundle in Node with the browser's WASM and stores (Issue #265): the second
// half of scripts/build_browser_sample.py, which writes the inputs.
//
// Usage: node native/wasm/tools/build_project_bundle.mjs --pkg PKG_DIR --inputs DIR --output FILE
//          [--ticks 1440] [--expect SAMPLE_BUILD_JSON]
//   PKG_DIR  the output of `wasm-bindgen --target nodejs` for sheltersim_wasm.wasm
//   DIR      inputs.json (the records Python's server holds for the new Run, and their source),
//            scenario.json, evidence.json and export.json (the world at minute 0), as
//            build_browser_sample.py writes them
//   FILE     the bundle to write (js/project_store.mjs)
//   SAMPLE_BUILD_JSON  the summary scripts/build_project_sample.py wrote of the VM's sample: each
//            branch's world and log hashes and the child's fork origin must be the same here
//
// It makes the Project in a store in memory with js/project_store.mjs and checks that its records
// are Python's, computes the branch in WASM for the whole clock and stores it as it goes
// (js/run_store.mjs), marks the Run completed. Where the inputs name an intervention (#310), it
// then makes the child branch through the browser's own routes (js/browser_api.mjs), as the VM's
// sample does on the server: `intervention-preview` of the text at the fork minute on main,
// `intervention-apply`, and `resume` of the child to the end. It writes the bundle, and imports
// the bundle into an empty store to check that it reads back to the same answers and digests. It
// prints a summary.

import { readFileSync, writeFileSync } from "node:fs";
import { createRequire } from "node:module";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { parseArgs } from "node:util";
import assert from "node:assert/strict";

const { values: options } = parseArgs({
  options: {
    pkg: { type: "string" },
    inputs: { type: "string" },
    output: { type: "string" },
    ticks: { type: "string", default: "1440" },
    expect: { type: "string" },
  },
});
if (!options.pkg || !options.inputs || !options.output) {
  console.error("usage: node build_project_bundle.mjs --pkg PKG_DIR --inputs DIR --output FILE [--ticks 1440]");
  process.exit(2);
}
const require = createRequire(import.meta.url);
const wasm = require(join(resolve(options.pkg), "sheltersim_wasm.js"));
const here = dirname(fileURLToPath(import.meta.url));
const store = await import(join(here, "..", "js", "project_store.mjs"));
const { BrowserApi } = await import(join(here, "..", "js", "browser_api.mjs"));
const { forkLine } = await import(join(here, "..", "js", "interventions.mjs"));
const read = (name) => new Uint8Array(readFileSync(join(options.inputs, name)));
const inputs = JSON.parse(readFileSync(join(options.inputs, "inputs.json"), "utf8"));
if (inputs.kind !== "browser_sample_inputs" || ![1, 2].includes(inputs.schema_version)) throw new Error("not the inputs of build_browser_sample.py");

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

// The child branch, through the routes the screen calls (#310).
let child = null;
if (inputs.intervention) {
  const { text, minute, child_id: childId } = inputs.intervention;
  const api = new BrowserApi({ wasm, tree });
  const base = `/api/v1/projects/${projectId}/runs/${runId}`;
  const call = async (method, path, body = null, search = "") => {
    const got = await api.answer({ method, path: `${base}${path}`, search, body: body === null ? null : JSON.stringify(body) });
    if (got.status !== 200) throw new Error(`${method} ${path}: ${got.status} ${got.body}`);
    return JSON.parse(got.body);
  };
  const forkStarted = performance.now();
  const preview = await call("POST", `/branches/${inputs.branch_id}/intervention-preview`, { text, minute });
  if (preview.status !== "compiled" || !preview.can_apply) throw new Error(`the intervention preview cannot be applied: ${JSON.stringify(preview)}`);
  const applied = await call("POST", `/branches/${inputs.branch_id}/intervention-apply`, {
    preview_id: preview.preview_id,
    approved_program_hash: preview.approved_program_hash,
    approved: true,
    child_id: childId,
  });
  assert.equal(applied.status, "running");
  const fork = (await call("GET", "")).last_fork;
  if (fork?.status !== "created") throw new Error(`the fork failed: ${JSON.stringify(fork)}`);
  const duration = (await store.branchRecord(tree, projectId, runId, inputs.branch_id)).computed_until;
  await call("POST", "/resume", { branch_id: childId, target_minute: duration });
  for (const job of [...api.computing.jobs.values()]) await job.finished;
  const record = await store.branchRecord(tree, projectId, runId, childId);
  if (record.status !== "completed" || record.computed_until !== duration) throw new Error(`the child ended ${record.status} at minute ${record.computed_until}`);
  await store.setRunStatus(tree, projectId, runId, "completed");
  // The world's digest at the end of each branch: its frame's state_hash.
  const ends = {};
  for (const branchId of [inputs.branch_id, childId]) {
    ends[branchId] = (await call("GET", `/branches/${branchId}/timeline`, null, `minute=${duration}`)).state_hash;
  }
  // The child's origin, as its stored Run's "fork" line holds it (the Checkpoint's fork_origin).
  const origin = (await forkLine(api, projectId, runId, childId))?.fork_origin;
  api.freeLive?.();
  child = {
    text,
    minute,
    record,
    approved_program_hash: preview.approved_program_hash,
    fork_origin_state_hash: origin?.state_hash ?? null,
    fork_state_hash: fork.state_hash,
    ends,
    seconds: Math.round(performance.now() - forkStarted) / 1000,
  };
}

const bundle = await store.exportProject({ tree, projectId, source: inputs.source });
const bytes = new Uint8Array(await bundle.arrayBuffer());
writeFileSync(options.output, bytes);

// The bundle reads back into an empty store to the same answers and digests.
const again = store.memoryTree();
const imported = await store.importProject({ wasm, tree: again, bundle: bytes });
for (const route of ["projectResponse", "runsResponse"]) assert.equal(await store[route](again, projectId), await store[route](tree, projectId), route);
assert.equal(await store.runResponse(again, projectId, runId), await store.runResponse(tree, projectId, runId));
const branch = imported.branches.find((item) => item.branch_id === inputs.branch_id);
for (const item of imported.branches) assert.ok(item.verified, `the imported branch ${item.branch_id} reads back to its digests`);
assert.deepEqual(branch.parts, computed.ran.end.parts);
assert.equal(imported.branches.length, child === null ? 1 : 2);

// The VM's sample, where given: the same worlds and logs at the end of each branch, the same fork.
let expected = null;
if (options.expect) {
  const built = JSON.parse(readFileSync(options.expect, "utf8"));
  const differences = [];
  for (const item of imported.branches) {
    const python = built.branches.find((other) => other.branch_id === item.branch_id);
    if (!python) {
      differences.push(`${item.branch_id}: not in the VM's sample`);
      continue;
    }
    if (child !== null && child.ends[item.branch_id] !== python.world_hash) differences.push(`${item.branch_id}: world_hash`);
    if (item.parts.event_log !== python.events_hash) differences.push(`${item.branch_id}: events_hash`);
    if (python.parent_branch_id !== null && child?.fork_origin_state_hash !== python.fork_origin_state_hash) differences.push(`${item.branch_id}: fork_origin_state_hash`);
  }
  if (child !== null && child.approved_program_hash !== built.intervention.approved_program_hash) differences.push("approved_program_hash");
  if (built.branches.length !== imported.branches.length) differences.push("the number of branches");
  if (differences.length > 0) throw new Error(`not the VM's sample: ${differences.join(", ")}`);
  expected = { compared: built.branches.map((item) => item.branch_id), equal: true };
}

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
      child,
      vm_sample: expected,
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
