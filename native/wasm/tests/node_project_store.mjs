// The browser's Project records and its one-file bundle (Issue #265; js/project_store.mjs).
//
// Usage: node native/wasm/tests/node_project_store.mjs PKG_DIR
//   PKG_DIR: the output of `wasm-bindgen --target nodejs` for sheltersim_wasm.wasm.
//
// A Project is made in a store in memory: its records, a Scenario and evidence given as bytes (as
// Python writes them), and two branches computed in WASM from worlds of
// native/core/tests/data/tick_core.json and stored as they go. Its bundle is imported into an
// empty store: every route's answer is the same text, and each branch reads back to the digests it
// was computed to (and the native path's). Bundles that are cut, changed, of another version, or
// whose records or stored Runs do not hold together are refused with an error, and leave nothing.

import { readFileSync } from "node:fs";
import { createRequire } from "node:module";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import assert from "node:assert/strict";

const pkg = process.argv[2];
if (!pkg) {
  console.error("usage: node node_project_store.mjs PKG_DIR");
  process.exit(2);
}
const require = createRequire(import.meta.url);
const wasm = require(join(resolve(pkg), "sheltersim_wasm.js"));
const here = dirname(fileURLToPath(import.meta.url));
const store = await import(join(here, "..", "js", "project_store.mjs"));
const raw = readFileSync(join(here, "..", "..", "core", "tests", "data", "tick_core.json"));
const fixture = JSON.parse(raw.toString("utf8"));
const [mainWorld, childWorld] = Object.keys(fixture.worlds);
const encoder = new TextEncoder();

// A Scenario and evidence as Python writes them: numbers such as 1.0 stay as written.
const scenarioBytes = encoder.encode('{"id":"trial","seed":7,"share":1.0,"name":"試し"}');
const scenarioHash = await store.sha256Hex(scenarioBytes);
const evidenceBytes = encoder.encode('{"schema_version":1,"weight":0.50,"legend":"仮定"}');

async function makeProject(tree) {
  const project = await store.createProject(tree, "p1");
  assert.deepEqual(project, { schema_version: 1, project_id: "p1", archived: false });
  assert.deepEqual(await store.createProject(tree, "p1"), project, "creating it again gives the same record");
  await assert.rejects(store.createProject(tree, "runtime-accounts"), /reserved/);
  await assert.rejects(store.createProject(tree, "../x"), /invalid artifact identifier/);
  await assert.rejects(store.saveScenario(tree, "p1", { scenarioBytes, scenarioHash: "0".repeat(64) }), /hash does not match/);
  const scenario = await store.saveScenario(tree, "p1", { scenarioBytes, scenarioHash });
  assert.deepEqual(scenario, { schema_version: 1, scenario_id: "trial", scenario_version: 1, scenario_hash: scenarioHash, approved: true });
  const run = await store.createRun(tree, "p1", { runId: "r1", scenarioId: "trial", scenarioVersion: 1, seed: 7, modelVersions: { "engine:core": "3" }, evidenceBytes });
  assert.equal(run.status, "queued");
  assert.equal(run.scenario_hash, scenarioHash);
  const ticks = (name) => fixture.worlds[name].expected.ticks;
  const main = await store.computeBranch({ wasm, tree, projectId: "p1", runId: "r1", branchId: "main", run: { exportBytes: raw, world: mainWorld, ticks: 60 } });
  // The second branch stands for a fork (#251, #250): its own stored Run, with a parent.
  const child = await store.computeBranch({ wasm, tree, projectId: "p1", runId: "r1", branchId: "child", parentBranchId: "main", forkMinute: 0, run: { exportBytes: raw, world: childWorld, ticks: 60 } });
  for (const [record, name] of [[main.record, mainWorld], [child.record, childWorld]]) {
    assert.equal(record.computed_until, ticks(name));
    assert.match(record.checkpoint_hash, /^[a-f0-9]{64}$/);
  }
  await store.setRunStatus(tree, "p1", "r1", "completed");
  await assert.rejects(store.writeBranch(tree, "p1", "r1", { ...child.record, fork_minute: 1 }), /lineage is immutable/);
  return { main, child };
}

async function responses(tree) {
  return {
    projects: await store.projectsResponse(tree, { limitBytes: 1000 }),
    project: await store.projectResponse(tree, "p1"),
    runs: await store.runsResponse(tree, "p1"),
    run: await store.runResponse(tree, "p1", "r1"),
    evidence: await store.evidenceResponse(tree, "p1", "r1"),
  };
}

const tree = store.memoryTree();
const made = await makeProject(tree);
const before = await responses(tree);

// The answers have the Python server's shapes.
const run = JSON.parse(before.run);
assert.deepEqual(Object.keys(run).sort(), ["archived", "branches", "model_versions", "project_id", "run_id", "scenario_hash", "scenario_id", "scenario_version", "schema_version", "seed", "status"]);
assert.deepEqual(run.branches.map((branch) => [branch.branch_id, branch.parent_branch_id, branch.status]), [["child", "main", "completed"], ["main", null, "completed"]]);
assert.deepEqual(Object.keys(run.branches[0]).sort(), ["branch_id", "checkpoint_hash", "computed_until", "fork_minute", "parent_branch_id", "run_id", "schema_version", "status"]);
const project = JSON.parse(before.project);
assert.equal(project.scenarios[0].scenario.share, 1);
assert.ok(before.project.includes('"share":1.0'), "the Scenario's text is Python's");
assert.equal(before.evidence, new TextDecoder().decode(evidenceBytes));
assert.deepEqual(JSON.parse(before.runs).runs, [run]);
assert.deepEqual(JSON.parse(before.projects).projects, [{ schema_version: 1, project_id: "p1", archived: false }]);
assert.equal(JSON.parse(before.projects).storage.limit_bytes, 1000);
await assert.rejects(store.runResponse(tree, "p1", "nope"), /missing or corrupt/);

// Exported and imported into an empty store: the same answers and the same digests.
const bundle = await store.exportProject({ tree, projectId: "p1", source: { test: "node_project_store" } });
const bytes = new Uint8Array(await bundle.arrayBuffer());
const empty = store.memoryTree();
const imported = await store.importProject({ wasm, tree: empty, bundle });
assert.deepEqual(imported.source, { test: "node_project_store" });
assert.deepEqual(await responses(empty), before);
for (const branch of imported.branches) {
  assert.ok(branch.stored && branch.verified, branch.branch_id);
  const world = branch.branch_id === "main" ? mainWorld : childWorld;
  const ran = branch.branch_id === "main" ? made.main.ran : made.child.ran;
  assert.deepEqual(branch.parts, ran.end.parts, `${branch.branch_id}: the parts' digests`);
  assert.equal(branch.metrics, fixture.worlds[world].expected.metrics, `${branch.branch_id}: the native path's metrics`);
  for (const [part, digest] of Object.entries(fixture.worlds[world].expected.world)) {
    if (part !== "scalars") assert.equal(branch.parts[part], digest, `${branch.branch_id}: the native path's ${part}`);
  }
}
// From bytes as from a Blob; again into the same store only with `replace`.
await assert.rejects(store.importProject({ wasm, tree: empty, bundle: bytes }), (error) => error.code === "project_exists");
await store.importProject({ wasm, tree: empty, bundle: bytes, replace: true });
assert.deepEqual(await responses(empty), before);

// Bundles that are not right are refused, and leave nothing.
async function refused(bad, pattern, code) {
  const target = store.memoryTree();
  await assert.rejects(store.importProject({ wasm, tree: target, bundle: bad }), (error) => {
    assert.match(error.message, pattern);
    if (code) assert.equal(error.code, code);
    return true;
  });
  assert.equal(target.files.size, 0, `${pattern}: nothing is left`);
}
const changed = (at, value = null) => {
  const copy = bytes.slice();
  copy[at] = value ?? copy[at] ^ 0xff;
  return copy;
};
await refused(changed(1), /magic/, "bundle_invalid");
await refused(changed(19, 2), /version 2: this page reads version 1/, "bundle_version_unsupported");
await refused(changed(60), /manifest is not the one written/, "bundle_invalid");
await refused(changed(bytes.length - 3), /not the ones the bundle lists/, "bundle_digest_mismatch");
await refused(bytes.subarray(0, bytes.length - 1), /past the file's end/, "bundle_invalid");
await refused(bytes.subarray(0, 20), /shorter than its header/, "bundle_invalid");
await refused(new Uint8Array([...bytes, 0]), /bytes follow the last entry/, "bundle_invalid");

// Bundles packed right whose contents are not: entries rewritten, with their sha256 listed anew.
const { entries } = await store.readBundle(bytes);
async function repacked(edit) {
  const edited = new Map(entries);
  edit(edited);
  return store.packBundle({ projectId: "p1", entries: [...edited] });
}
await refused(await repacked((e) => e.set("../evil", new Uint8Array(1))), /path is not one a bundle holds/);
await refused(await repacked((e) => e.delete("project.json")), /no project.json/);
await refused(await repacked((e) => e.set("scenarios/trial/v1.scenario.json", encoder.encode('{"id":"trial"}'))), /sha256 is not its hash/);
await refused(await repacked((e) => e.delete("runs/r1/evidence.json")), /no evidence/);
const branchPath = "runs/r1/branches/child/branch.json";
const childRecord = JSON.parse(new TextDecoder().decode(entries.get(branchPath)));
const withRecord = (record) => (e) => e.set(branchPath, encoder.encode(JSON.stringify(record)));
await refused(await repacked(withRecord({ ...childRecord, parent_branch_id: "gone" })), /has no parent/);
await refused(await repacked(withRecord({ ...childRecord, checkpoint_hash: "a".repeat(64) })), /does not end at its record's head/, "bundle_run_mismatch");
await refused(await repacked(withRecord({ ...childRecord, computed_until: childRecord.computed_until - 1 })), /does not end at its record's head/, "bundle_run_mismatch");
await refused(await repacked(withRecord({ ...childRecord, schema_version: 2 })), /migration required/);
// A stored Run whose piece was changed (its sha256 in the manifest made anew) does not read back.
await refused(
  await repacked((e) => {
    const data = e.get("runs/r1/branches/main/data.pack").slice();
    data[data.length - 3] ^= 0xff;
    e.set("runs/r1/branches/main/data.pack", data);
  }),
  /does not read back/,
  "bundle_run_mismatch",
);
// Its journal cut before its end line: the Run is no longer at the record's head.
await refused(
  await repacked((e) => {
    const journal = e.get("runs/r1/branches/main/journal.jsonl");
    const text = new TextDecoder().decode(journal);
    e.set("runs/r1/branches/main/journal.jsonl", encoder.encode(text.slice(0, text.lastIndexOf("\n", text.length - 2) + 1)));
  }),
  /does not end at its record's head/,
  "bundle_run_mismatch",
);
// With `replace`, a bundle whose stored Run does not read back leaves the Project already there as
// it was (#273): its stored Runs are read back before the old one is removed. The files are compared
// by their sha256: node:assert's deepEqual builds a diff of the bytes when it fails, and for files of
// hundreds of KiB that diff takes tens of GiB.
const fileDigests = async (tree) => {
  const lines = [];
  for (const [path, file] of tree.files) lines.push(`${path} ${await store.sha256Hex(file.bytes.slice(0, file.length))}`);
  return lines.sort();
};
const kept = await fileDigests(empty);
const unreadable = await repacked((e) => {
  const data = e.get("runs/r1/branches/child/data.pack").slice();
  data[data.length - 3] ^= 0xff;
  e.set("runs/r1/branches/child/data.pack", data);
});
await assert.rejects(store.importProject({ wasm, tree: empty, bundle: unreadable, replace: true }), (error) => error.code === "bundle_run_mismatch");
assert.deepEqual(await fileDigests(empty), kept, "the old Project is kept");
assert.deepEqual(await responses(empty), before);
// A file the branch's stored Run does not know is carried as it is (a later piece file).
const extra = await repacked((e) => e.set("runs/r1/branches/main/later.bin", Uint8Array.of(1, 2, 3)));
const carried = store.memoryTree();
await store.importProject({ wasm, tree: carried, bundle: extra });
assert.deepEqual(carried.files.get("p1/runs/r1/branches/main/later.bin").bytes.slice(0, 3), Uint8Array.of(1, 2, 3));

console.log(`a Project of ${made.main.record.computed_until} + ${made.child.record.computed_until} minutes: bundle ${(bytes.length / 1024).toFixed(1)} KiB, imported with the same answers and digests; bad bundles refused`);
