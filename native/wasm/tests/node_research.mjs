// The regional research of the browser against Python's (Issue #300).
//
// Usage: node native/wasm/tests/node_research.mjs PKG_DIR
//   PKG_DIR: the output of `wasm-bindgen --target nodejs` for sheltersim_wasm.wasm.
//
// For each case of native/core/tests/data/research_cases.json (tests/support/core_research.py),
// js/research.mjs researches the request with the fixture's clock, shelter index, rows by
// municipality code (#332) and address search answers (served through `gsiFetchJson` by a fake
// `fetch`) and must write Python's research byte for byte, or refuse it (`BrowserUnsupported`) where
// the case says the browser does. A lookup by name that the index answers reads no rows.

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
let rowsRead = 0;
for (const [index, item] of fixture.cases.entries()) {
  const label = `${index} ${item.request.municipality_query}/${item.request.disaster}/${item.request.municipality_code}`;
  const read = [];
  let made;
  try {
    made = await research.research({
      request: item.request,
      now: new Date(fixture.now),
      index: fixture.indexes[item.csv],
      shelterRows: async (code) => {
        read.push(code);
        if (!Object.hasOwn(fixture.rows[item.csv], code)) throw new Error(`no rows of ${code}`);
        return fixture.rows[item.csv][code];
      },
      casefold: wasm.casefold,
      fetchJson: (url) => research.gsiFetchJson(url, fakeFetch),
    });
    rowsRead += read.length;
    const status = JSON.parse(made.text).shelter_register.status;
    const needed = ["matched", "no_published_records", "result_limit_exceeded"].includes(status) && JSON.parse(made.text).shelter_register.resolved_municipality_name !== null;
    if (read.length !== (needed ? 1 : 0)) {
      console.log(`${label}: FAILED, read the rows of ${JSON.stringify(read)} for ${status}`);
      failures += 1;
    }
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

// Rows that cannot be read, or of another snapshot, make the register "unavailable", as Python's
// CSV download that fails.
const matched = fixture.cases.find((item) => item.csv === "rows" && JSON.parse(item.research).shelter_register.status === "matched");
for (const [shelterRows, warning] of [
  [async () => Promise.reject(new research.RegionProviderError("public-data endpoint could not be reached")), "public-data endpoint could not be reached"],
  [async (code) => ({ ...fixture.rows.rows[code], sha256: "0".repeat(64) }), "the site's GSI shelter rows are not of its index's snapshot; reload the page"],
]) {
  const { document } = await research.research({
    request: matched.request,
    now: new Date(fixture.now),
    index: fixture.indexes.rows,
    shelterRows,
    casefold: wasm.casefold,
    fetchJson: (url) => research.gsiFetchJson(url, fakeFetch),
  });
  assert.equal(document.shelter_register.status, "unavailable");
  assert.equal(document.shelters.length, 0);
  assert.equal(document.warnings.at(-2), warning);
}
await assert.rejects(
  research.research({
    request: matched.request,
    now: new Date(fixture.now),
    index: fixture.indexes.rows,
    shelterRows: async (code) => ({ ...fixture.rows.rows[code], columns: ["named"] }),
    casefold: wasm.casefold,
    fetchJson: (url) => research.gsiFetchJson(url, fakeFetch),
  }),
  /other columns/,
);

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
console.log(`research: ok (${fixture.cases.length} cases, ${asked.length} address searches, ${rowsRead} codes' rows read)`);
