// The EffectProgram of an intervention before it is scheduled, in WASM, is Python's (#304).
//
// Usage: node native/wasm/tests/node_programs.mjs PKG_DIR
//   PKG_DIR: the output of `wasm-bindgen --target nodejs` for sheltersim_wasm.wasm.
//
// For each case of native/core/tests/data/programs.json (tests/support/core_programs.py), it
// gives the program's JSON text (as bytes: JSON.parse would write 1.0 as 1) to `effectProgramDump`
// and checks the sha256 of the dump (the approval's hash) against Python's, or that the contract
// refuses it; then it runs the case's world in WASM to the minute and checks that
// `Simulation.validateEffectProgram` gives the issues `validate_effect_program` gave, or refuses
// a case the browser does not check, as native/core/tests/programs.rs does in native code.

import { readFileSync } from "node:fs";
import { createRequire } from "node:module";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const pkg = process.argv[2];
if (!pkg) {
  console.error("usage: node node_programs.mjs PKG_DIR");
  process.exit(2);
}
const require = createRequire(import.meta.url);
const wasm = require(join(resolve(pkg), "sheltersim_wasm.js"));

const here = dirname(fileURLToPath(import.meta.url));
const data = (name) => readFileSync(join(here, "..", "..", "core", "tests", "data", `${name}.json`));
const fixture = JSON.parse(data("programs").toString("utf8"));
const encoder = new TextEncoder();
const files = {};
const runs = {};

const problems = [];
let checked = 0;
for (const item of fixture.cases) {
  const raw = encoder.encode(item.program_text);
  let dump = null;
  try {
    dump = wasm.effectProgramDump(raw);
  } catch {
    if (!item.refused) problems.push(`${item.name}: the contract refused it`);
    continue;
  }
  if (item.refused) {
    problems.push(`${item.name}: the contract took what Python refused`);
    continue;
  }
  if (wasm.sha256Hex(dump) !== item.dump_sha256) problems.push(`${item.name}: the dump is not Python's`);
  const file = fixture.files[item.world];
  files[file] ??= data(file);
  runs[item.world] ??= wasm.Simulation.fromWorlds(files[file], item.world);
  const run = runs[item.world];
  run.advance(item.minute - run.minute());
  const before = run.digests_json();
  let issues = null;
  try {
    issues = JSON.parse(run.validateEffectProgram(raw));
  } catch (error) {
    if (!item.unsupported) problems.push(`${item.name}: ${error.message}`);
  }
  if (run.digests_json() !== before) problems.push(`${item.name}: the check changed the world`);
  if (issues !== null) {
    if (item.unsupported) problems.push(`${item.name}: checked what the browser does not check`);
    else if (JSON.stringify(issues) !== JSON.stringify(item.issues)) problems.push(`${item.name}: ${JSON.stringify(issues)} is not ${JSON.stringify(item.issues)}`);
  }
  checked += 1;
}
for (const run of Object.values(runs)) run.free();
console.log(`${fixture.cases.length} programs, ${checked} checked on their worlds: ${problems.length === 0 ? "ok" : "FAILED"}`);
for (const problem of problems) console.log(`  ${problem}`);
process.exit(problems.length === 0 ? 0 : 1);
