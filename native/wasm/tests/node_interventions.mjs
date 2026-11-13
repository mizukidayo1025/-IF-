// The browser's answers to an intervention and its child branch are the Python server's (#306;
// js/interventions.mjs through js/browser_api.mjs).
//
// Usage: node native/wasm/tests/node_interventions.mjs PKG_DIR
//   PKG_DIR: the output of `wasm-bindgen --target nodejs` for sheltersim_wasm.wasm.
//
// native/core/tests/data/interventions.json (tests/support/core_interventions.py) holds a Run the
// Project screen made of a generated Scenario on the Python server (its worker a process, as the
// VM's), the world it started from and the server's answers to `intervention-preview`,
// `intervention-apply`, the Run's record with `last_fork`, and the child's frames and KPI history
// once computed. The Run is computed here in WASM and stored as the browser stores it, and the same
// requests go through BrowserApi: each body, masked as the builder masks it (`preview_id` and the
// branches' heads), has the server's sha256, and a refusal the server's body. Then what only the
// browser does: a typed `program` is refused as unsupported, the child's stored Run is of format
// v4 and starts at the fork, and it reads back to its digests.

import { readFileSync } from "node:fs";
import { createRequire } from "node:module";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import assert from "node:assert/strict";

const pkg = process.argv[2];
if (!pkg) {
  console.error("usage: node node_interventions.mjs PKG_DIR");
  process.exit(2);
}
const require = createRequire(import.meta.url);
const wasm = require(join(resolve(pkg), "sheltersim_wasm.js"));
const here = dirname(fileURLToPath(import.meta.url));
const store = await import(join(here, "..", "js", "project_store.mjs"));
const { BrowserApi } = await import(join(here, "..", "js", "browser_api.mjs"));
const runStore = await import(join(here, "..", "js", "run_store.mjs"));
const raw = readFileSync(join(here, "..", "..", "core", "tests", "data", "interventions.json"));
const fixture = JSON.parse(raw.toString("utf8"));
const encoder = new TextEncoder();
const decoder = new TextDecoder();

let failures = 0;
function check(ok, message) {
  if (!ok) {
    failures += 1;
    console.error(`FAIL: ${message}`);
  }
}

/// tests/support/core_api_answers.py's `normalize`, the head replaced by "HEAD".
function normalize(body, anchor) {
  const document = JSON.parse(body);
  const cursor = document?.next_cursor;
  if (typeof cursor === "string") {
    const decoded = Buffer.from(cursor, "base64url").toString("utf8");
    const again = Buffer.from(decoded.replaceAll(anchor, "HEAD"), "utf8").toString("base64url");
    const padded = again + "=".repeat((4 - (again.length % 4)) % 4);
    body = body.replace(`"next_cursor":"${cursor}"`, `"next_cursor":"${padded}"`);
  }
  return body.replaceAll(anchor, "HEAD");
}

/// tests/support/core_interventions.py's `mask`.
async function mask(body) {
  let text = body
    .replace(/"preview_id":"[0-9a-f]{32}"/g, '"preview_id":"PREVIEW"')
    .replace(/"checkpoint_hash":"[0-9a-f]{64}"/g, '"checkpoint_hash":"HEAD"')
    .replace(/"context_id":"(c[0-9a-f]{32})-[0-9a-f]{24}"/g, '"context_id":"$1-RANDOM"');
  for (const branch of await store.branches(tree, projectId, runId)) {
    if (branch.checkpoint_hash) text = normalize(text, branch.checkpoint_hash);
  }
  return text;
}

const { project_id: projectId, run_id: runId, child, minutes } = fixture.run;
const tree = store.memoryTree();
await store.createProject(tree, projectId);
const scenarioBytes = encoder.encode(fixture.scenario_json);
const scenario = await store.saveScenario(tree, projectId, { scenarioBytes, scenarioHash: await store.sha256Hex(scenarioBytes) });
await store.createRun(tree, projectId, {
  runId,
  scenarioId: scenario.scenario_id,
  scenarioVersion: scenario.scenario_version,
  seed: fixture.run.seed,
  modelVersions: fixture.run.model_versions,
  evidenceBytes: encoder.encode('{"schema_version":1}'),
});
let started = performance.now();
const { record } = await store.computeBranch({ wasm, tree, projectId, runId, branchId: "main", run: { exportBytes: raw, world: "interventions", ticks: 60 } });
await store.setRunStatus(tree, projectId, runId, "completed");
console.log(`main computed to ${record.computed_until} in ${((performance.now() - started) / 1000).toFixed(2)} s`);

const browser = new BrowserApi({ wasm, tree });
const base = `/api/v1/projects/${projectId}/runs/${runId}`;
const post = (path, body) => browser.answer({ method: "POST", path: `${base}${path}`, body });
const get = (path, search = "") => browser.answer({ method: "GET", path: `${base}${path}`, search });

async function compare(name, got, expected) {
  if (got.status !== expected.status) {
    check(false, `${name}: status ${got.status} (${got.body.slice(0, 300)}), not ${expected.status} (${(expected.body ?? expected.text ?? "").slice(0, 300)})`);
    return;
  }
  if (expected.status !== 200) {
    check(got.body === expected.body, `${name}: ${got.body}, not ${expected.body}`);
    return;
  }
  const text = await mask(got.body);
  const ok = wasm.sha256Hex(encoder.encode(text)) === expected.sha256;
  check(ok, `${name}: the body is not the server's\n  browser: ${text.slice(0, 1500)}\n  server:  ${(expected.text ?? "(long)").slice(0, 1500)}`);
}

/// Waits until no computation runs.
async function settle() {
  for (const job of [...browser.computing.jobs.values()]) await job.finished;
}

started = performance.now();
let browserPreview = null;
let agent = null;
let firstContext = null;
for (const item of fixture.cases) {
  let got;
  if (item.route === "preview") {
    got = await post("/branches/main/intervention-preview", item.request);
    if (browserPreview === null && item.status === 200 && item.text?.includes('"can_apply":true')) browserPreview = JSON.parse(got.body);
  } else if (item.route === "apply") {
    // The browser's own preview_id where the server's was ("$PREVIEW").
    const document = { ...item.document };
    if (document.preview_id === "$PREVIEW") document.preview_id = browserPreview.preview_id;
    got = await post("/branches/main/intervention-apply", JSON.stringify(document));
  } else if (item.route === "run") {
    got = await get("");
  } else if (item.route === "timeline") {
    // The child is computed to the end before its first frame is asked for.
    if (browser.computing.jobs.size === 0 && (await store.branchRecord(tree, projectId, runId, child)).computed_until < minutes) {
      const resumed = await post("/resume", JSON.stringify({ branch_id: child, target_minute: minutes }));
      check(resumed.status === 200, `resume the child: ${resumed.body}`);
      await settle();
    }
    got = await get(`/branches/${child}/timeline`, `minute=${item.minute}`);
  } else if (item.route === "history") {
    got = await get(`/branches/${child}/history`, item.query);
  } else if (item.route === "questions") {
    if (agent === null) agent = JSON.parse((await get(`/branches/main/timeline`, `minute=${minutes}`)).body).people[0].id;
    const document = Object.fromEntries(Object.entries(item.document).map(([key, value]) => [key, value === "$AGENT" ? agent : value === "$CONTEXT" ? firstContext : value]));
    got = await post(`/branches/${item.branch}/questions`, JSON.stringify(document));
    if (firstContext === null && got.status === 200) firstContext = JSON.parse(got.body).context_id;
  } else if (item.route === "compare" || item.route === "replay-frames") {
    got = await get(`/${item.route}`, item.query);
  } else {
    throw new Error(`no route ${item.route}`);
  }
  await compare(`${item.route} ${item.name}`, got, item);
}
console.log(`${fixture.cases.length} answers compared with the server's in ${((performance.now() - started) / 1000).toFixed(2)} s`);

// Each question answered is kept as a turn beside its branch, its question as the server keeps it.
{
  const lines = decoder.decode((await tree.read([projectId, "runs", runId, "branches", "main", "conversations.jsonl"])) ?? new Uint8Array(0)).trim().split("\n");
  const questions = lines.map((line) => JSON.parse(line).question);
  check(JSON.stringify(questions) === JSON.stringify(fixture.stored_questions), `the questions kept: ${JSON.stringify(questions)}, not ${JSON.stringify(fixture.stored_questions)}`);
}

// A typed program is refused as unsupported, after the checks of the minute.
{
  const got = await post("/branches/main/intervention-preview", JSON.stringify({ minute: fixture.run.fork_minute, program: {} }));
  check(got.status === 409 && JSON.parse(got.body).code === "browser_unsupported", `a typed program: ${got.body}`);
}

// The child's stored Run: format v4, its "run" line holds the fork, it starts at the fork's minute,
// and it reads back to its digests.
{
  const backend = await store.branchBackend(tree, projectId, runId, child);
  const reader = await runStore.RunReader.open({ wasm, backend });
  try {
    assert.equal(reader.run.format, runStore.FORMAT);
    assert.equal(reader.run.fork_minute, fixture.run.fork_minute);
    const forkLine = reader.lines.find((line) => line.kind === "fork");
    check(forkLine?.fork_origin?.branch_id === "main" && forkLine.minute === fixture.run.fork_minute, "the fork line names its origin");
    await assert.rejects(reader.seek(fixture.run.fork_minute - 1));
  } finally {
    reader.close();
  }
  const back = await runStore.readAndVerify({ wasm, backend });
  check(back.complete && back.verified, `the child's stored Run reads back to its digests: ${JSON.stringify(back).slice(0, 300)}`);
}

browser.close();
if (failures > 0) {
  console.error(`${failures} failure(s)`);
  process.exit(1);
}
console.log("the intervention's answers are the server's");
