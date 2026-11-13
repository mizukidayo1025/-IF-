// The browser's answers to the screen's API (Issue #266; js/browser_api.mjs).
//
// Usage: node native/wasm/tests/node_browser_api.mjs PKG_DIR
//   PKG_DIR: the output of `wasm-bindgen --target nodejs` for sheltersim_wasm.wasm.
//
// A Project is made in a store in memory with one branch computed in WASM from the world
// `small_replay` of native/core/tests/data/tick_core.json (one day, saved points every 6 hours).
// Its routes answer the records' texts; `timeline` and `replay-frames` answer, at each minute
// asked (saved points, minutes between, before the first, the end, going back), the frame a Run
// computed without stopping gives there, byte for byte, with the world's digest as its state_hash;
// consecutive minutes and replay windows step the kept Simulation and never restore. Bad queries
// get the Python server's errors, and the routes stage A leaves out get "未対応" (409). The sample
// of a site, split into parts, is installed once, and parts that are not the manifest's are refused
// and leave nothing.

import { readFileSync } from "node:fs";
import { createRequire } from "node:module";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import assert from "node:assert/strict";

const pkg = process.argv[2];
if (!pkg) {
  console.error("usage: node node_browser_api.mjs PKG_DIR");
  process.exit(2);
}
const require = createRequire(import.meta.url);
const wasm = require(join(resolve(pkg), "sheltersim_wasm.js"));
const here = dirname(fileURLToPath(import.meta.url));
const store = await import(join(here, "..", "js", "project_store.mjs"));
const api = await import(join(here, "..", "js", "browser_api.mjs"));
const raw = readFileSync(join(here, "..", "..", "core", "tests", "data", "tick_core.json"));
const WORLD = "small_replay";
const encoder = new TextEncoder();
const decoder = new TextDecoder();
const sha = (text) => wasm.sha256Hex(encoder.encode(text));

async function makeProject(tree) {
  await store.createProject(tree, "p1");
  const scenarioBytes = encoder.encode('{"id":"trial","seed":7}');
  await store.saveScenario(tree, "p1", { scenarioBytes, scenarioHash: await store.sha256Hex(scenarioBytes) });
  await store.createRun(tree, "p1", { runId: "r1", scenarioId: "trial", scenarioVersion: 1, seed: 7, modelVersions: {}, evidenceBytes: encoder.encode('{"schema_version":1,"weight":0.50}') });
  const { record } = await store.computeBranch({ wasm, tree, projectId: "p1", runId: "r1", branchId: "main", run: { exportBytes: raw, world: WORLD, ticks: 120 } });
  // A second branch (a fork's stand-in, as in node_project_store.mjs), for the comparison's replay.
  await store.computeBranch({ wasm, tree, projectId: "p1", runId: "r1", branchId: "child", parentBranchId: "main", forkMinute: 0, run: { exportBytes: raw, world: "generated", ticks: 120 } });
  await store.setRunStatus(tree, "p1", "r1", "completed");
  return record;
}

const tree = store.memoryTree();
const record = await makeProject(tree);
const end = record.computed_until;
const browser = new api.BrowserApi({ wasm, tree, quota: async () => 1234 });
const get = (path, search = "") => browser.answer({ method: "GET", path: `/api/v1/projects${path}`, search });
const post = (path, body) => browser.answer({ method: "POST", path: `/api/v1/projects${path}`, body: JSON.stringify(body) });

// The records.
assert.deepEqual(await get(""), { status: 200, type: "application/json; charset=utf-8", body: await store.projectsResponse(tree, { limitBytes: 1234 }) });
assert.equal((await get("/p1")).body, await store.projectResponse(tree, "p1"));
assert.equal((await get("/p1/runs")).body, await store.runsResponse(tree, "p1"));
assert.equal((await get("/p1/runs/r1")).body, await store.runResponse(tree, "p1", "r1"));
assert.equal((await get("/p1/runs/r1/evidence")).body, '{"schema_version":1,"weight":0.50}');
assert.equal((await post("", { project_id: "p2" })).body, '{"schema_version":1,"project_id":"p2","archived":false}');
assert.equal((await post("", { project_id: "p2", x: 1 })).status, 409);
assert.equal((await get("/nope")).status, 409);
assert.equal((await get("/p1/runs/nope")).status, 409);

// The frames a Run computed without stopping gives, at each minute asked.
const minutes = [0, 1, 359, 360, 361, 800, 801, 802, 803, 804, 805, 806, 807, 808, 809, 810, 100, 1080, 1439, end];
const wanted = new Map();
{
  const simulation = wasm.Simulation.fromWorlds(raw, WORLD);
  for (const minute of [...new Set(minutes)].sort((a, b) => a - b)) {
    simulation.advance(minute - simulation.minute());
    const frame = decoder.decode(simulation.frame("r1", "main", end, undefined));
    assert.equal(JSON.parse(frame).state_hash, simulation.stateDigest());
    wanted.set(minute, sha(frame));
  }
  simulation.free();
}
let failures = 0;
const check = (ok, message) => {
  if (!ok) {
    console.log(`  ${message}`);
    failures += 1;
  }
};
for (const minute of minutes) {
  const answer = await get("/p1/runs/r1/branches/main/timeline", `?minute=${minute}`);
  check(answer.status === 200 && sha(answer.body) === wanted.get(minute), `timeline at ${minute}: not the uninterrupted Run's frame (${answer.status})`);
}
// 0 (seek), 1, 359, 360, 361 (steps), 800 (seek: 439 ticks, past the point at 720), 801..810 (steps),
// 100 (seek back), 1080 (seek), 1439 and the end (steps).
check(browser.stats.seeks === 4, `timeline: ${browser.stats.seeks} seeks, not 4`);

// Replay windows step the kept Simulation: no seek after the first window's.
const seeks = browser.stats.seeks;
for (let start = 1000; start <= 1030; start += 8) {
  const answer = await get("/p1/runs/r1/replay-frames", `?branch_ids=main&start=${start}&end=${start + 7}`);
  const document = JSON.parse(answer.body);
  check(answer.status === 200 && document.start === start && document.end === start + 7 && document.samples.length === 8, `replay from ${start}: ${answer.status}`);
  check(document.schema_version === 1 && document.branch_ids.join() === "main", `replay from ${start}: not the route's shape`);
  for (const [index, sample] of document.samples.entries()) {
    const simulation = wasm.Simulation.fromWorlds(raw, WORLD);
    simulation.advance(start + index);
    const frame = decoder.decode(simulation.frame("r1", "main", end, undefined));
    simulation.free();
    check(JSON.stringify(sample) === JSON.stringify(JSON.parse(frame)), `replay at ${start + index}: not the uninterrupted Run's frame`);
  }
}
check(browser.stats.seeks === seeks + 1, `replay: ${browser.stats.seeks - seeks} seeks, not 1`);

// The Python server's errors.
const status = async (path, search, code = null) => {
  const answer = await get(path, search);
  const error = JSON.parse(answer.body);
  return [answer.status, typeof error.error === "string", code === null || error.code === code];
};
assert.deepEqual(await status("/p1/runs/r1/branches/main/timeline", `?minute=${end + 1}`), [409, true, true]);
assert.deepEqual(await status("/p1/runs/r1/branches/main/timeline", "?minute=x"), [409, true, true]);
assert.deepEqual(await status("/p1/runs/r1/branches/main/timeline", "?minute=1&minute=2"), [409, true, true]);
assert.deepEqual(await status("/p1/runs/r1/branches/nope/timeline", "?minute=1"), [409, true, true]);
assert.deepEqual(await status("/p1/runs/r1/replay-frames", "?branch_ids=main&start=0&end=8"), [409, true, true]);
assert.deepEqual(await status("/p1/runs/r1/replay-frames", "?branch_ids=main&start=0"), [409, true, true]);
assert.deepEqual(await status("/p1/runs/r1/replay-frames", `?branch_ids=main&start=${end}&end=${end + 1}`), [409, true, true]);

// Two branches (#307; node_interventions.mjs checks them against the server's): each minute's
// comparison, with no receipt for a branch not made by a fork.
{
  const replay = await get("/p1/runs/r1/replay-frames", "?branch_ids=main,child&start=0&end=1");
  const compare = await get("/p1/runs/r1/compare", "?branch_ids=main,child&minute=1");
  check(replay.status === 200 && JSON.parse(replay.body).samples.length === 2, `replay of two branches: ${replay.body.slice(0, 200)}`);
  check(compare.status === 200 && JSON.stringify(JSON.parse(compare.body).origin_receipts) === "[null,null]", `compare: ${compare.body.slice(0, 200)}`);
}

// What stage A leaves out: "未対応", 409.
const unsupported = [
  await post("/p1/scenario-preview", { text: "x" }),
  await post("/p1/runs/r1/pdf-report", { branch_ids: ["main"], minute: 1 }),
  await browser.answer({ method: "GET", path: "/api/v1/demo", search: "" }),
];
for (const answer of unsupported) {
  const error = JSON.parse(answer.body);
  check(answer.status === 409 && error.code === "browser_unsupported" && error.error.startsWith(api.UNSUPPORTED), `not refused as unsupported: ${answer.body}`);
}
// History of another kind than the KPIs: api_answers.mjs's refusal.
const events = await get("/p1/runs/r1/branches/main/history", "?kind=events&start=0&end=10");
check(events.status === 409 && JSON.parse(events.body).code === "browser_unsupported", `history of events: ${events.body}`);
// The history, the report and the LLM status are api_answers.mjs's (node_api_answers.mjs checks
// their JSON against the server's).
{
  const history = await get("/p1/runs/r1/branches/main/history", `?kind=kpis&start=0&end=${end}&step=60&limit=10`);
  const page = JSON.parse(history.body);
  check(history.status === 200 && page.items.length === 10 && typeof page.next_cursor === "string", `history: ${history.status} ${history.body.slice(0, 200)}`);
  const next = await get("/p1/runs/r1/branches/main/history", `?kind=kpis&start=0&end=${end}&step=60&limit=10&cursor=${page.next_cursor}`);
  check(next.status === 200 && JSON.parse(next.body).items[0].minute_index > page.items.at(-1).minute_index, `history's next page: ${next.status} ${next.body.slice(0, 200)}`);
  const status = await get("/p1/runs/r1/branches/main/llm-status");
  check(status.status === 200 && JSON.parse(status.body).branch_id === "main", `llm-status: ${status.status} ${status.body.slice(0, 200)}`);
  const bad = await get("/p1/runs/r1/branches/main/history", "?kind=kpis&start=0&end=99999");
  check(bad.status === 409, `history out of range: ${bad.status}`);
}
// A route another module answers is called with the request.
const extra = new api.BrowserApi({ wasm, tree, extra: { report: async ({ projectId, runId, branchId }) => JSON.stringify({ projectId, runId, branchId }) } });
assert.equal((await extra.answer({ method: "GET", path: "/api/v1/projects/p1/runs/r1/branches/main/report" })).body, '{"projectId":"p1","runId":"r1","branchId":"main"}');
browser.close();
extra.close();

// The site's sample: a bundle split into parts.
const bundle = new Uint8Array(await (await store.exportProject({ tree, projectId: "p1" })).arrayBuffer());
const size = Math.ceil(bundle.length / 3);
const files = new Map();
const parts = [];
for (let at = 0, index = 0; at < bundle.length; at += size, index += 1) {
  const bytes = bundle.slice(at, at + size);
  const path = `sample.shelterproj.${String(index).padStart(3, "0")}`;
  files.set(path, bytes);
  parts.push({ path, bytes: bytes.length, sha256: await store.sha256Hex(bytes) });
}
const manifest = { format: api.SAMPLE_FORMAT, version: api.SAMPLE_VERSION, project_id: "p1", bytes: bundle.length, sha256: await store.sha256Hex(bundle), parts };
async function install(target, marker, given = files) {
  const phases = [];
  const result = await api.installSample({
    wasm,
    tree: target,
    manifest,
    fetchBytes: async (path) => given.get(path),
    readMarker: async () => (marker.bytes ? JSON.parse(decoder.decode(marker.bytes)) : null),
    writeMarker: async (bytes) => {
      marker.bytes = bytes;
    },
    progress: ({ phase }) => phases.push(phase),
  });
  return { ...result, phases: [...new Set(phases)].join() };
}
const fresh = store.memoryTree();
const marker = {};
const first = await install(fresh, marker);
assert.equal(first.installed, true);
assert.equal(first.phases, "fetch,import,done");
assert.equal(await store.runResponse(fresh, "p1", "r1"), await store.runResponse(tree, "p1", "r1"));
assert.equal((await install(fresh, marker)).installed, false, "a store holding the sample is left as it is");
// A changed part is refused before anything is written.
const changed = new Map(files);
const broken = Uint8Array.from(files.get(parts[1].path));
broken[10] ^= 1;
changed.set(parts[1].path, broken);
const empty = store.memoryTree();
await assert.rejects(install(empty, {}, changed), /is not the one its manifest lists/);
assert.equal(empty.files.size, 0);
// Another sample (the marker names another bundle) replaces the Project.
marker.bytes = encoder.encode('{"sha256":"00"}');
assert.equal((await install(fresh, marker)).installed, true);
assert.throws(() => api.checkSampleManifest({ ...manifest, bytes: 1 }), /do not add up/);
assert.throws(() => api.checkSampleManifest({ ...manifest, parts: [{ ...parts[0], path: "../x" }] }), /without its name/);

if (failures > 0) {
  console.error(`${failures} problem(s)`);
  process.exit(1);
}
console.log(`browser API: records, ${minutes.length} timeline minutes and 4 replay windows answer the uninterrupted Run's frames (${browser.stats.seeks} seeks, ${browser.stats.steps} ticks stepped); unsupported routes refused; sample installed from ${parts.length} parts`);
