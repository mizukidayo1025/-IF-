// The regional research of the browser against Python's (Issue #300).
//
// Usage: node native/wasm/tests/node_research.mjs PKG_DIR
//   PKG_DIR: the output of `wasm-bindgen --target nodejs` for sheltersim_wasm.wasm.
//
// For each case of native/core/tests/data/research_cases.json (tests/support/core_research.py),
// js/research.mjs researches the request with the fixture's clock, shelter index and address
// search answers (served through `gsiFetchJson` by a fake `fetch`) and must write Python's research
// byte for byte, or refuse it (`BrowserUnsupported`) where the case says the browser does.

import { readFileSync } from "node:fs";
import { createRequire } from "node:module";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import assert from "node:assert/strict";

const pkg = process.argv[2];
if (!pkg) {
  console.error("usage: node node_research.mjs PKG_DIR");
  process.exit(2);
}
const require = createRequire(import.meta.url);
const wasm = require(join(resolve(pkg), "sheltersim_wasm.js"));
const here = dirname(fileURLToPath(import.meta.url));
const research = await import(join(here, "..", "js", "research.mjs"));
const fixture = JSON.parse(readFileSync(join(here, "..", "..", "core", "tests", "data", "research_cases.json"), "utf8"));

const asked = [];
async function fakeFetch(url) {
  asked.push(url);
  const query = new URL(url).searchParams.get("q");
  const answer = Object.hasOwn(fixture.address, query) ? fixture.address[query] : { status: 200, body: "[]" };
  if (answer === null) throw new TypeError("fetch failed");
  const bytes = new TextEncoder().encode(answer.body);
  return { status: answer.status, arrayBuffer: async () => bytes.buffer };
}

let failures = 0;
for (const [index, item] of fixture.cases.entries()) {
  const label = `${index} ${item.request.municipality_query}/${item.request.disaster}`;
  let made;
  try {
    made = await research.research({
      request: item.request,
      now: new Date(fixture.now),
      index: fixture.indexes[item.csv],
      casefold: wasm.casefold,
      fetchJson: (url) => research.gsiFetchJson(url, fakeFetch),
    });
  } catch (error) {
    if (error instanceof research.BrowserUnsupported && item.browser === "unsupported") continue;
    console.log(`${label}: FAILED ${error.stack}`);
    failures += 1;
    continue;
  }
  if (item.browser === "unsupported") {
    console.log(`${label}: FAILED, answered what the browser refuses`);
    failures += 1;
  } else if (made.text !== item.research) {
    const python = JSON.parse(item.research);
    const browser = JSON.parse(made.text);
    const keys = Object.keys(python).filter((key) => JSON.stringify(python[key]) !== JSON.stringify(browser[key]));
    console.log(`${label}: FAILED, differs in ${keys.join(", ")}`);
    for (const key of keys.slice(0, 2)) console.log(`  python  ${JSON.stringify(python[key]).slice(0, 400)}\n  browser ${JSON.stringify(browser[key]).slice(0, 400)}`);
    failures += 1;
  }
}

// Python's floats, order and times.
assert.equal(research.pyFloatRepr(139), "139.0");
assert.equal(research.pyFloatRepr(1e-5), "1e-05");
assert.equal(research.pyFloatRepr(1e16), "1e+16");
assert.equal(research.pyFloatRepr(123456.789), "123456.789");
assert.equal(research.pyUtc(new Date("2026-11-01T03:00:00.120Z")), "2026-11-01T03:00:00.120000Z");
assert.equal(research.quotePlus("墨田 区~*"), "%E5%A2%A8%E7%94%B0+%E5%8C%BA~%2A");
assert.ok(research.codePointCompare("\u{ffff}", "\u{10000}") < 0);
assert.throws(() => research.checkShelterIndex({ format: "x" }), /not a GSI shelter index/);

if (failures > 0) {
  console.error(`${failures} cases differ`);
  process.exit(1);
}
console.log(`research: ok (${fixture.cases.length} cases, ${asked.length} address searches)`);
