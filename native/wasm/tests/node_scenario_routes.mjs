// The Scenario screen's routes in the browser against the Python server (Issue #301).
//
// Usage: node native/wasm/tests/node_scenario_routes.mjs PKG_DIR [CASE ...]
//   PKG_DIR: the output of `wasm-bindgen --target nodejs` for sheltersim_wasm.wasm.
//
// For each case of native/core/tests/data/scenario_corpus.json (#293), js/scenario_routes.mjs
// answers `scenario-preview` with the corpus's body, clock and public services (the recorded
// address search and shelter CSV of native/core/tests/data/research_cases.json), in a memory store.
// The answer must be the server's: its status and error, or its fields (but `draft_id` and
// `preview_id`, uuid4 in both), the Scenario's and the FacilityDesign's bytes (sha256) and the
// research. A case that can start is then approved: the Run's `model_versions`, its evidence and
// its world at minute 0 must be the server's (sha256).

import { readFileSync } from "node:fs";
import { createRequire } from "node:module";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import assert from "node:assert/strict";

const pkg = process.argv[2];
if (!pkg) {
  console.error("usage: node node_scenario_routes.mjs PKG_DIR [CASE ...]");
  process.exit(2);
}
const only = new Set(process.argv.slice(3));
const require = createRequire(import.meta.url);
const wasm = require(join(resolve(pkg), "sheltersim_wasm.js"));
const here = dirname(fileURLToPath(import.meta.url));
const data = join(here, "..", "..", "core", "tests", "data");
const corpus = JSON.parse(readFileSync(join(data, "scenario_corpus.json"), "utf8"));
const recorded = JSON.parse(readFileSync(join(data, "research_cases.json"), "utf8"));
const { ScenarioRoutes } = await import(join(here, "..", "js", "scenario_routes.mjs"));
const store = await import(join(here, "..", "js", "project_store.mjs"));
const research = await import(join(here, "..", "js", "research.mjs"));
const { errorAnswer } = await import(join(here, "..", "js", "api_answers.mjs"));

const encoder = new TextEncoder();
const sha = (bytes) => wasm.sha256Hex(typeof bytes === "string" ? encoder.encode(bytes) : bytes);
async function fakeFetch(url) {
  const query = new URL(url).searchParams.get("q");
  const answer = Object.hasOwn(recorded.address, query) ? recorded.address[query] : { status: 200, body: "[]" };
  if (answer === null) throw new TypeError("fetch failed");
  const bytes = encoder.encode(answer.body);
  return { status: answer.status, arrayBuffer: async () => bytes.buffer };
}
let ids = 0;
const routesOf = (tree) =>
  new ScenarioRoutes({
    wasm,
    tree,
    shelterIndex: async () => recorded.indexes.corpus,
    fetchJson: (url) => research.gsiFetchJson(url, fakeFetch),
    now: () => new Date(corpus.now),
    newId: () => `${(ids += 1).toString(16).padStart(32, "0")}`,
  });

async function answered(task) {
  try {
    return { status: 200, body: JSON.parse(await task()) };
  } catch (error) {
    const { status, body } = errorAnswer(error);
    return { status, body: JSON.parse(body), error };
  }
}

let failures = 0;
let compared = 0;
let runs = 0;
for (const [name, item] of Object.entries(corpus.cases)) {
  if (only.size > 0 && !only.has(name)) continue;
  const tree = store.memoryTree();
  await store.createProject(tree, "corpus");
  const routes = routesOf(tree);
  const problems = [];
  const preview = await answered(() => routes.preview("corpus", item.body));
  if (preview.status !== item.status) {
    problems.push(`status ${preview.status} (${JSON.stringify(preview.body).slice(0, 200)}), not ${item.status}`);
    if (preview.error && preview.status >= 500) problems.push(preview.error.stack);
  } else if (item.status !== 200) {
    for (const key of Object.keys(item.answer)) {
      if (JSON.stringify(preview.body[key]) !== JSON.stringify(item.answer[key])) problems.push(`${key}: ${JSON.stringify(preview.body[key])} != ${JSON.stringify(item.answer[key])}`);
    }
  } else {
    const body = preview.body;
    for (const key of Object.keys(item.answer)) {
      let mine = body[key];
      let theirs = item.answer[key];
      if (key === "request") {
        const sorted = (value) => ({ ...value, field_origins: Object.fromEntries(Object.entries(value.field_origins).sort()) });
        mine = sorted(mine);
        theirs = sorted(theirs);
      }
      try {
        assert.deepEqual(mine, theirs);
      } catch {
        problems.push(`${key}: ${JSON.stringify(mine).slice(0, 300)} != ${JSON.stringify(theirs).slice(0, 300)}`);
      }
    }
    const extra = Object.keys(body).filter((key) => !(key in item.answer) && !["scenario", "research", "facility_design", "draft_id", "preview_id"].includes(key));
    if (extra.length > 0) problems.push(`more keys: ${extra}`);
    if (item.scenario) {
      // The answer's own bytes of the Scenario and the FacilityDesign, as the routes wrote them.
      const draft = await routes.draft("corpus", body.preview_id);
      const content = draft.contentText;
      const scenarioText = content.slice(content.indexOf('"scenario":') + 11, content.lastIndexOf(',"usage_account_id":'));
      if (sha(scenarioText) !== item.scenario.sha256) problems.push(`the Scenario differs (${scenarioText.length} bytes, not ${item.scenario.length})`);
      const designText = content.slice(content.indexOf('"facility_design":') + 18, content.indexOf(',"hash":'));
      if (sha(designText) !== item.facility_design.sha256) problems.push("the FacilityDesign differs");
      try {
        assert.deepEqual(body.research, corpus.research[item.research]);
      } catch {
        problems.push("the research differs");
      }
    }
    if (item.run && problems.length === 0) {
      const run = await answered(() => routes.approve("corpus", { preview_id: body.preview_id, approved_hash: body.approved_hash, approved: true, run_id: "run-corpus" }));
      if (run.status !== 200) {
        problems.push(`approve: ${run.status} ${JSON.stringify(run.body)}`);
      } else {
        const versions = { ...corpus.common.model_versions, ...item.run.model_versions };
        try {
          assert.deepEqual(run.body.model_versions, versions);
        } catch {
          problems.push("model_versions differ");
        }
        const evidence = await tree.read(["corpus", "runs", "run-corpus", "evidence.json"]);
        if (sha(evidence) !== item.run.evidence.sha256) problems.push("the evidence differs");
        const scenarioBytes = await store.runScenarioBytes(tree, "corpus", "run-corpus");
        if (sha(wasm.initialEntry(scenarioBytes)) !== item.run.export.sha256) problems.push("the world at minute 0 differs");
        if (run.body.branches.length !== 1 || run.body.branches[0].computed_until !== 0) problems.push(`branches ${JSON.stringify(run.body.branches)}`);
        runs += 1;
      }
    }
  }
  compared += 1;
  if (problems.length > 0) {
    failures += 1;
    console.log(`${name}: FAILED`);
    for (const problem of problems) console.log(`  ${problem}`);
  }
}
if (failures > 0) {
  console.error(`${failures} of ${compared} cases differ`);
  process.exit(1);
}
console.log(`scenario routes: ok (${compared} cases, ${runs} Runs)`);
