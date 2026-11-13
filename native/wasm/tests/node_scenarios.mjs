// The Scenario the screen's request builds, in WASM, is the Python server's (#299).
//
// Usage: node native/wasm/tests/node_scenarios.mjs PKG_DIR
//   PKG_DIR: the output of `wasm-bindgen --target nodejs` for sheltersim_wasm.wasm.
//
// For each case of the scenario corpus (native/core/tests/data/scenario_corpus.json,
// tests/support/scenario_corpus.py) that made a Scenario, it calls `buildScenario` with the request
// the server validated (the form's as posted, or the text parser's), the research it made and the
// input's sha256 (the text's, or "" for the form's own), and compares the Scenario's sha256 (approved_hash), the design's and its
// geometry's, the request's dump, the warnings and the blockers with the server's answer, as
// native/core/tests/scenario_builder.rs does in native code. A refused request (a field the
// browser does not compute) gives `browser_unsupported: ...`.

import { readFileSync } from "node:fs";
import { createHash } from "node:crypto";
import { createRequire } from "node:module";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import assert from "node:assert/strict";

const pkg = process.argv[2];
if (!pkg) {
  console.error("usage: node node_scenarios.mjs PKG_DIR");
  process.exit(2);
}
const require = createRequire(import.meta.url);
const wasm = require(join(resolve(pkg), "sheltersim_wasm.js"));

const here = dirname(fileURLToPath(import.meta.url));
const corpus = JSON.parse(readFileSync(join(here, "..", "..", "core", "tests", "data", "scenario_corpus.json"), "utf8"));
const sha256 = (bytes) => createHash("sha256").update(bytes).digest("hex");
const encode = (value) => new TextEncoder().encode(JSON.stringify(value));
const decode = (bytes) => new TextDecoder().decode(bytes);

let failures = 0;
let checked = 0;
for (const [name, entry] of Object.entries(corpus.cases)) {
  if (!entry.scenario) continue;
  const answer = entry.answer;
  const text = typeof entry.body.text === "string" ? entry.body.text : "";
  // Ints and floats of these requests mean the same to the request's validation; the research's
  // floats have fractions, so JSON.stringify keeps them.
  const posted = entry.body.request ?? answer.request;
  const structured = text.trim() === "";
  // The form's input is the request's own sha256, which buildScenario computes when given "".
  const inputSha = structured ? "" : sha256(Buffer.from(text, "utf8"));
  const built = wasm.buildScenario(encode(posted), encode(corpus.research[entry.research]), inputSha, structured ? "structured_form" : "rules_fallback");
  const problems = [];
  const scenario = built.scenario();
  if (built.approvedHash !== answer.approved_hash || sha256(scenario) !== answer.approved_hash) problems.push(`approved_hash (${scenario.length} bytes, Python ${entry.scenario.length})`);
  if (sha256(built.facilityDesign()) !== entry.facility_design.sha256) problems.push("facility_design");
  if (built.geometryHash !== answer.geometry_hash) problems.push("geometry_hash");
  // JSON.parse drops a float's ".0", on both sides alike.
  if (JSON.stringify(sortKeys(JSON.parse(decode(built.request())))) !== JSON.stringify(sortKeys(answer.request))) problems.push("request");
  const warnings = JSON.parse(built.warnings());
  if (warnings.length < 3 || JSON.stringify(answer.warnings.slice(answer.warnings.length - warnings.length)) !== JSON.stringify(warnings)) problems.push("warnings");
  if (JSON.stringify(JSON.parse(built.approvalBlockers())) !== JSON.stringify(answer.approval_blockers) || built.canStart !== answer.can_start) problems.push("blockers");
  built.free();
  checked += 1;
  if (problems.length) {
    failures += 1;
    console.error(`${name}: ${problems.join(", ")}`);
  }
}

// What the browser does not compute is refused, not built differently.
const minimal = corpus.cases["form-minimal"];
assert.throws(
  () => wasm.buildScenario(encode({ ...minimal.body.request, behavior_policy_id: "priority-rule-v1" }), encode(corpus.research[minimal.research]), "0".repeat(64), "structured_form"),
  /^Error: browser_unsupported: /,
);
assert.throws(() => wasm.buildScenario(encode({ municipality: "x" }), encode(corpus.research[minimal.research]), "0".repeat(64), "structured_form"), /^Error: builder_refused: /);

function sortKeys(value) {
  if (Array.isArray(value)) return value.map(sortKeys);
  if (value && typeof value === "object") return Object.fromEntries(Object.keys(value).sort().map((key) => [key, sortKeys(value[key])]));
  return value;
}

if (failures || checked < 40) {
  console.error(`${failures} of ${checked} corpus Scenarios differ`);
  process.exit(1);
}
console.log(`${checked} corpus Scenarios built in WASM equal the server's`);
