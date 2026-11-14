// The browser's answers to the KPI history, report and LLM status of a branch are the Python
// server's (Issue #266; js/api_answers.mjs).
//
// Usage: node native/wasm/tests/node_api_answers.mjs PKG_DIR [FIXTURE]
//   PKG_DIR: the output of `wasm-bindgen --target nodejs` for sheltersim_wasm.wasm.
//   FIXTURE: another file of the same form (a larger Run built with the same builder), instead of
//   native/core/tests/data/api_answers.json.
//
// native/core/tests/data/api_answers.json (tests/support/core_api_answers.py) holds a Run the
// Project screen made of a generated Scenario on the Python server, the world it started from and
// the server's answers to the routes (each page's status and the sha256 of its body, the branch's
// head replaced by ANCHOR). The Run is computed here in WASM and stored in memory as the browser
// stores it (project_store.mjs), and each case is asked again: the browser's body, its head
// replaced the same way, has the same sha256, and a refusal the same body. Paged cases follow the
// browser's own `next_cursor`. Then `pdf-report`'s answers (#309), and what only the browser does: kinds it does not answer, a branch
// stored in an older format, a report kept once made, a branch not completed.

import { readFileSync, mkdirSync, writeFileSync } from "node:fs";
import { createRequire } from "node:module";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import assert from "node:assert/strict";

const pkg = process.argv[2];
if (!pkg) {
  console.error("usage: node node_api_answers.mjs PKG_DIR");
  process.exit(2);
}
const require = createRequire(import.meta.url);
const wasm = require(join(resolve(pkg), "sheltersim_wasm.js"));
const here = dirname(fileURLToPath(import.meta.url));
const store = await import(join(here, "..", "js", "project_store.mjs"));
const api = await import(join(here, "..", "js", "api_answers.mjs"));
const raw = readFileSync(process.argv[3] ?? join(here, "..", "..", "core", "tests", "data", "api_answers.json"));
const fixture = JSON.parse(raw.toString("utf8"));
const encoder = new TextEncoder();
const decoder = new TextDecoder();
// Where to write the browser's bodies of failed cases, to compare by hand (optional).
const dump = process.env.API_ANSWERS_DUMP ?? null;

let failures = 0;
function check(ok, message) {
  if (!ok) {
    failures += 1;
    console.error(`FAIL: ${message}`);
  }
}

/// tests/support/core_api_answers.py's `normalize`.
function normalize(body, anchor) {
  if (!anchor) return body;
  const document = JSON.parse(body);
  const cursor = document?.next_cursor;
  if (typeof cursor === "string") {
    const decoded = Buffer.from(cursor, "base64url").toString("utf8");
    const again = Buffer.from(decoded.replaceAll(anchor, "ANCHOR"), "utf8").toString("base64url");
    const padded = again + "=".repeat((4 - (again.length % 4)) % 4);
    body = body.replace(`"next_cursor":"${cursor}"`, `"next_cursor":"${padded}"`);
  }
  return body.replaceAll(anchor, "ANCHOR");
}

const { project_id: projectId, run_id: runId, branch_id: branchId } = fixture.run;
const tree = store.memoryTree();
await store.createProject(tree, projectId);
const scenarioBytes = encoder.encode(fixture.scenario_json);
const scenario = await store.saveScenario(tree, projectId, { scenarioBytes, scenarioHash: await store.sha256Hex(scenarioBytes) });
await store.createRun(tree, projectId, {
  runId,
  scenarioId: scenario.scenario_id,
  scenarioVersion: scenario.scenario_version,
  seed: fixture.run.seed,
  modelVersions: {},
  evidenceBytes: encoder.encode('{"schema_version":1}'),
});
let started = performance.now();
const { record } = await store.computeBranch({ wasm, tree, projectId, runId, branchId, run: { exportBytes: raw, world: "history", ticks: 60 } });
console.log(`computed ${record.computed_until} minutes in ${((performance.now() - started) / 1000).toFixed(2)} s`);
assert.equal(record.status, "completed");
const anchor = record.checkpoint_hash;

/// The browser's status and body of a route.
async function answer(route, branch, query, cache) {
  const ids = { wasm, tree, projectId, runId, branchId: branch, query };
  try {
    if (route === "history") return { status: 200, body: await api.historyResponse({ ...ids, cache }) };
    if (route === "report") return { status: 200, body: await api.reportResponse(ids) };
    if (route === "llm-status") return { status: 200, body: await api.llmStatusResponse(ids) };
  } catch (error) {
    const { status, body } = api.errorAnswer(error);
    if (status === 500) console.error(error);
    return { status, body };
  }
  throw new Error(`no route ${route}`);
}

function compare(name, got, expected) {
  if (got.status !== expected.status) {
    check(false, `${name}: status ${got.status} (${got.body.slice(0, 200)}), not ${expected.status}`);
    return;
  }
  if (expected.status !== 200) {
    check(got.body === expected.body, `${name}: ${got.body}, not ${expected.body}`);
    return;
  }
  const text = normalize(got.body, anchor);
  const bytes = encoder.encode(text);
  const ok = wasm.sha256Hex(bytes) === expected.sha256;
  check(ok, `${name}: the body (${bytes.length} bytes) is not the server's (${expected.length} bytes)`);
  if (!ok && dump) {
    mkdirSync(dump, { recursive: true });
    writeFileSync(join(dump, `${name.replace(/[^A-Za-z0-9=&_-]/g, "_")}.json`), text);
  }
}

const cache = api.historyCache();
started = performance.now();
let asked = 0;
for (const { route, branch, query, pages } of fixture.cases) {
  let current = query;
  for (const [index, expected] of pages.entries()) {
    const got = await answer(route, branch, current, cache);
    asked += 1;
    compare(`${route} ${branch} ${query} page ${index + 1}`, got, expected);
    if (index + 1 < pages.length) {
      const next = got.status === 200 ? JSON.parse(got.body).next_cursor : null;
      if (typeof next !== "string") {
        check(false, `${route} ${query}: no cursor after page ${index + 1}`);
        break;
      }
      current = `${query}&cursor=${next}`;
    }
  }
}
// A cursor of one query given to another (#278).
for (const { from, to, ...expected } of fixture.foreign_cursors) {
  const first = JSON.parse((await answer("history", branchId, from, cache)).body);
  compare(`the cursor of ${from} given to ${to}`, await answer("history", branchId, `${to}&cursor=${first.next_cursor}`, cache), expected);
}
check(fixture.foreign_cursors.length > 0, "the fixture has cursors of other queries");
console.log(`${asked} answers compared with the server's in ${((performance.now() - started) / 1000).toFixed(2)} s`);

// `POST runs/{run}/pdf-report` (#309): the server's checks and, past them, its 409 without an API key.
{
  const { BrowserApi } = await import(join(here, "..", "js", "browser_api.mjs"));
  const browser = new BrowserApi({ wasm, tree });
  for (const item of fixture.pdf_cases ?? []) {
    const got = await browser.answer({ method: "POST", path: `/api/v1/projects/${projectId}/runs/${item.run_id}/pdf-report`, body: item.body });
    check(got.status === item.status && got.body === item.answer, `pdf-report ${item.run_id} ${item.body}: ${got.status} ${got.body}, not ${item.status} ${item.answer}`);
  }
  check((fixture.pdf_cases ?? []).length > 0, "the fixture has pdf-report cases");
  browser.close();
}

// The report was made once and kept as the branch's report.json; the next answer is the same.
const reportPath = [projectId, "runs", runId, "branches", branchId, "report.json"];
const kept = await tree.read(reportPath);
check(kept !== null, "the report is kept");
check(kept !== null && decoder.decode(kept).trimEnd() === (await answer("report", branchId, "")).body, "the kept report is answered again");

// Without a cache, the same answer.
{
  const query = fixture.cases[1].query;
  const withCache = await answer("history", branchId, query, cache);
  const without = await answer("history", branchId, query, null);
  check(withCache.body === without.body, "an answer without the cache is the one with it");
}

// Kinds the browser does not answer, after the server's checks.
for (const kind of ["events", "people&agent_id=A001", "conversations"]) {
  const got = await answer("history", branchId, `kind=${kind}&end=10`, cache);
  check(got.status === 409 && JSON.parse(got.body).code === "browser_unsupported", `kind=${kind}: ${got.status} ${got.body}`);
}
check((await answer("history", branchId, "kind=events&end=999999", cache)).body.includes("history_outside_computed_range"), "the range is checked first");

// A branch not completed has no report yet; a branch with no stored Run answers no rows.
await store.writeBranch(tree, projectId, runId, { branch_id: "waiting", status: "queued" });
check((await answer("report", "waiting", "")).body === '{"status":"not_generated"}', "a branch not completed has no report");
{
  const got = await answer("history", "waiting", "kind=kpis&end=0", cache);
  const body = got.status === 200 ? JSON.parse(got.body) : {};
  check(got.status === 200 && body.items.length === 0 && body.anchor_hash === null && body.next_cursor === null, `a branch with no stored Run: ${got.body}`);
  const status = JSON.parse((await answer("llm-status", "waiting", "")).body);
  check(status.mode === "offline" && status.checkpoint_hash === null && status.computed_until === 0, "the LLM status of a branch with no stored Run");
}

// A branch stored in format v2 (samples without the ledger's columns) is refused, not answered
// without them.
{
  const folder = [projectId, "runs", runId, "branches"];
  const journal = decoder.decode(await tree.read([...folder, branchId, "journal.jsonl"])).split("\n");
  const first = JSON.parse(journal[0].slice(0, journal[0].lastIndexOf("\t")));
  first.format = "sheltersim-browser-run-v2";
  const json = JSON.stringify(first);
  const rewritten = [`${json}\t${wasm.sha256Hex(encoder.encode(json))}`, ...journal.slice(1)].join("\n");
  await store.writeBranch(tree, projectId, runId, { ...record, branch_id: "old" });
  await tree.write([...folder, "old", "journal.jsonl"], encoder.encode(rewritten));
  await tree.write([...folder, "old", "data.pack"], await tree.read([...folder, branchId, "data.pack"]));
  const got = await answer("history", "old", "kind=kpis&end=10", cache);
  check(got.status === 409 && JSON.parse(got.body).code === "browser_run_outdated", `format v2: ${got.status} ${got.body}`);
}

// The query is read as the server's parse_qs reads it.
{
  const fields = api.parseQuery("a=1&b=&c&a=%E3%81%82+x&d=%zz&=e");
  assert.deepEqual([...fields], [["a", ["1", "あ x"]], ["d", ["%zz"]], ["", ["e"]]]);
  assert.deepEqual(api.errorAnswer(new store.ProjectStoreError("invalid artifact identifier")), { status: 409, body: '{"code":"storage_conflict","error":"invalid artifact identifier"}' });
  assert.deepEqual(api.errorAnswer(new Error("x")), { status: 500, body: '{"error":"inspector request failed"}' });
}

cache.clear();
if (failures > 0) {
  console.error(`${failures} failure(s)`);
  process.exit(1);
}
console.log("the history, report and LLM status are the server's");
