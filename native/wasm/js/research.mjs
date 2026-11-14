// The regional research of the Scenario screen's preview, in the browser (Issue #300, under #285).
//
// `research` answers what the Python server's `RegionResearchService.research` answers for the
// request `scenario-preview` makes (src/sheltersim/region/research.py), without an API key and with
// the VM's settings: no e-Stat app ID, no configured regional source, no JMA forecast area. Its
// result is `RegionResearchResult.model_dump(mode="json")`, written as canonical JSON (sorted keys,
// no spaces, floats as Python writes them) by `pyJson`.
//
// - The GSI address search is asked from the browser, as the VM asks it (the service sends
//   `Access-Control-Allow-Origin: *`), through `fetchJson` (`gsiFetchJson` by default), which
//   fails with the texts of providers.py's `_get_json`.
// - The GSI national shelter CSV (11.7 MB, no CORS) is not fetched: the site's build reads it once
//   and keeps its sha256, HTTP metadata, invalid-row count and the municipality names by code
//   (`gsi_shelter_index` of providers.py), and each code's rows the lookup can match, with the
//   columns the research reads (`gsi_shelter_rows`), both written by scripts/build_static_site.py.
//   The lookup answers as providers.py's `GsiShelterProvider.search_municipality`, from the index
//   and, when it needs the rows (a municipality code given, or a name that resolves to one code,
//   #332), from that code's rows (`shelterRows(code)`); the matched rows become research.py's
//   `ShelterCandidate`s.
// - e-Stat, the configured regional sources and the JMA forecast answer "not configured", as on the
//   VM; the MLIT P20 note and the hazard map layers are fixed texts.
//
// The clock is the caller's (`now`, a Date), read once for every timestamp: Python reads it more
// than once, but within one call the values are the same to the second they are compared at.

export const GSI_ADDRESS_SOURCE_ID = "SRC-GSI-ADDRESS-SEARCH";
export const GSI_HAZARD_SOURCE_ID = "SRC-GSI-HAZARD-OPEN-DATA";
export const GSI_SHELTER_SOURCE_ID = "SRC-GSI-DESIGNATED-SHELTERS";
export const GSI_SHELTER_SOURCE_PREFIX = "SRC-GSI-SHELTERS-";
export const MLIT_SHELTER_SOURCE_ID = "SRC-MLIT-P20-HISTORICAL";
const MLIT_P20_URL = "https://nlftp.mlit.go.jp/ksj/gml/datalist/KsjTmplt-P20.html";
const GSI_HAZARD_URL = "https://disaportal.gsi.go.jp/hazardmapportal/hazardmap/copyright/opendata.html";
export const GSI_ADDRESS_URL = "https://msearch.gsi.go.jp/address-search/AddressSearch";
export const GSI_SHELTER_CSV_URL = "https://hinanmap.gsi.go.jp/hinanjocp/defaultFtpData/csv/mergeFromCity_1.csv";
/// The shelter index the site's build writes (`gsi_shelter_index` of providers.py).
export const SHELTER_INDEX_FORMAT = "sheltersim-gsi-shelter-index";
export const SHELTER_INDEX_VERSION = 1;
/// A municipality code's rows the site's build writes (`gsi_shelter_rows` of providers.py).
export const SHELTER_ROWS_FORMAT = "sheltersim-gsi-shelter-rows";
export const SHELTER_ROWS_VERSION = 1;
const SHELTER_ROW_COLUMNS = ["named", "共通ID", "施設・場所名", "住所", "緯度", "経度", "受入対象者", "その他市町村長が必要と認める事項"];
const MAX_RESPONSE_BYTES = 2_000_000;
const MAX_MUNICIPALITY_CODE_SUGGESTIONS = 30;
const MAX_MUNICIPAL_SHELTERS = 500;
/// The longest municipality name the research's models take (pydantic's `max_length`, in code
/// points); Python answers a longer one "unavailable" (its ValidationError is a ValueError).
const MAX_MUNICIPALITY_NAME = 240;

/// What the browser does not answer: the caller answers 409 `browser_unsupported`.
export class BrowserUnsupported extends Error {}

/// A RegionProviderError of providers.py, with its text.
export class RegionProviderError extends Error {}

// ---- Python's values -------------------------------------------------------------------------------

/// A float, written as Python writes it (`1.0`, not `1`).
export class PyFloat {
  constructor(value) {
    this.value = value;
  }
}

/// `repr(float)`: the shortest digits, in Python's notation.
export function pyFloatRepr(value) {
  if (!Number.isFinite(value)) throw new Error("NaN and infinity are not JSON");
  if (value === 0) return Object.is(value, -0) ? "-0.0" : "0.0";
  const magnitude = Math.abs(value);
  if (magnitude >= 1e-4 && magnitude < 1e16) {
    const text = String(value);
    return text.includes(".") || text.includes("e") ? text : `${text}.0`;
  }
  const [mantissa, exponent] = value.toExponential().split("e");
  const sign = exponent.startsWith("-") ? "-" : "+";
  const digits = exponent.replace(/^[+-]/, "").padStart(2, "0");
  return `${mantissa}e${sign}${digits}`;
}

/// Python's order of str: by code point (JS compares UTF-16 code units).
export function codePointCompare(a, b) {
  const first = Array.from(a);
  const second = Array.from(b);
  for (let index = 0; index < Math.min(first.length, second.length); index += 1) {
    const difference = first[index].codePointAt(0) - second[index].codePointAt(0);
    if (difference !== 0) return difference;
  }
  return first.length - second.length;
}

/// Canonical JSON as Python writes it: `json.dumps(value, sort_keys=True, separators=(",", ":"),
/// ensure_ascii=False)`, a `PyFloat` as a float, a JS number as an int.
export function pyJson(value) {
  if (value instanceof PyFloat) return pyFloatRepr(value.value);
  if (Array.isArray(value)) return `[${value.map(pyJson).join(",")}]`;
  if (value !== null && typeof value === "object") {
    return `{${Object.keys(value)
      .sort(codePointCompare)
      .map((key) => `${JSON.stringify(key)}:${pyJson(value[key])}`)
      .join(",")}}`;
  }
  if (typeof value === "number" && !Number.isInteger(value)) throw new Error("a JS number that is not an int");
  return JSON.stringify(value);
}

/// `str.isspace` (and `\s` of a str pattern), from CPython 3.12 (native/core/src/text/classes.rs).
const PY_SPACE = /[\t\n\v\f\r\x1c-\x20\x85\xa0\u1680\u2000-\u200a\u2028\u2029\u202f\u205f\u3000]/u;
const PY_SPACES = /[\t\n\v\f\r\x1c-\x20\x85\xa0\u1680\u2000-\u200a\u2028\u2029\u202f\u205f\u3000]+/gu;

/// `str.strip()`.
export function pyStrip(text) {
  const chars = Array.from(text);
  let start = 0;
  let end = chars.length;
  while (start < end && PY_SPACE.test(chars[start])) start += 1;
  while (end > start && PY_SPACE.test(chars[end - 1])) end -= 1;
  return chars.slice(start, end).join("");
}

/// `datetime.isoformat` of an aware UTC datetime as pydantic writes it: `Z`, and the
/// microseconds only when there are some.
export function pyUtc(date) {
  const text = date.toISOString();
  const millis = date.getUTCMilliseconds();
  return millis === 0 ? text.replace(/\.\d{3}Z$/, "Z") : text.replace(/\.(\d{3})Z$/, ".$1000Z");
}

/// `_normalize_municipality`: NFKC, without white space, casefolded.
export function normalizeMunicipality(name, casefold) {
  return casefold(name.normalize("NFKC").replace(PY_SPACES, ""));
}

// ---- The GSI address search ------------------------------------------------------------------------

/// `urlencode({"q": query})`: `quote_plus` of the UTF-8 bytes.
export function quotePlus(text) {
  const safe = /[A-Za-z0-9_.\-~]/;
  let out = "";
  for (const char of text) {
    if (char === " ") out += "+";
    else if (safe.test(char)) out += char;
    else for (const byte of new TextEncoder().encode(char)) out += `%${byte.toString(16).toUpperCase().padStart(2, "0")}`;
  }
  return out;
}

/// providers.py's `_get_json` of a public JSON endpoint, with its errors' texts.
export async function gsiFetchJson(url, fetchImpl = globalThis.fetch) {
  let response;
  let body;
  try {
    response = await fetchImpl(url, { headers: { Accept: "application/json" } });
  } catch {
    throw new RegionProviderError("public-data endpoint could not be reached");
  }
  if (response.status !== 200) throw new RegionProviderError(`public-data endpoint returned HTTP ${response.status}`);
  try {
    body = new Uint8Array(await response.arrayBuffer());
  } catch {
    throw new RegionProviderError("public-data endpoint could not be reached");
  }
  if (body.length > MAX_RESPONSE_BYTES) throw new RegionProviderError("public-data response exceeded the 2 MB safety limit");
  try {
    return JSON.parse(new TextDecoder("utf-8", { fatal: true }).decode(body));
  } catch {
    throw new RegionProviderError("public-data endpoint returned invalid JSON");
  }
}

/// Python's `float(value)` of a JSON value, or null where it raises (`TypeError`, `ValueError`).
function pyFloatOf(value) {
  if (typeof value === "boolean") return value ? 1 : 0;
  if (typeof value === "number") return value;
  if (typeof value !== "string") return null;
  const text = pyStrip(value).toLowerCase();
  if (/^[+-]?(inf|infinity)$/.test(text)) return text.startsWith("-") ? -Infinity : Infinity;
  if (/^[+-]?nan$/.test(text)) return Number.NaN;
  const digits = /^[+-]?(\d(_?\d)*)?(\.(\d(_?\d)*)?)?(e[+-]?\d(_?\d)*)?$/;
  if (!digits.test(text) || !/\d/.test(text.split("e")[0])) return null;
  return Number(text.replaceAll("_", ""));
}

/// `str(value)` of a JSON value that is truthy (a title or an address).
function pyStr(value) {
  if (typeof value === "string") return value;
  if (typeof value === "number") return Number.isInteger(value) ? String(value) : pyFloatRepr(value);
  if (typeof value === "boolean") return value ? "True" : "False";
  return null;
}

const truthy = (value) =>
  value !== null && value !== undefined && value !== false && value !== 0 && value !== "" && !(Array.isArray(value) && value.length === 0) && !(typeof value === "object" && !Array.isArray(value) && Object.keys(value).length === 0);

/// `GsiAddressProvider.search(query)`: up to 8 candidates `{name, latitude, longitude}`.
async function addressSearch(query, fetchJson) {
  if (!pyStrip(query)) throw new RegionProviderError("query must be non-empty and limit must be between 1 and 20");
  const document = await fetchJson(`${GSI_ADDRESS_URL}?q=${quotePlus(query)}`);
  if (!Array.isArray(document)) throw new RegionProviderError("GSI address search response must be a list");
  const candidates = [];
  for (const item of document.slice(0, 8)) {
    if (item === null || typeof item !== "object" || Array.isArray(item)) continue;
    const { geometry, properties } = item;
    if (geometry === null || typeof geometry !== "object" || Array.isArray(geometry)) continue;
    if (properties === null || typeof properties !== "object" || Array.isArray(properties)) continue;
    const coordinates = geometry.coordinates;
    if (!Array.isArray(coordinates) || coordinates.length < 2) continue;
    const longitude = pyFloatOf(coordinates[0]);
    const latitude = pyFloatOf(coordinates[1]);
    if (longitude === null || latitude === null) continue;
    const named = truthy(properties.title) ? properties.title : truthy(properties.address) ? properties.address : query;
    const name = pyStr(named);
    if (name === null) throw new BrowserUnsupported("a GSI address title that is not text");
    candidates.push({ name, latitude, longitude });
  }
  return candidates;
}

// ---- The shelter register ----------------------------------------------------------------------------

/// Check a shelter index the site's build wrote.
export function checkShelterIndex(index) {
  if (index === null || typeof index !== "object" || index.format !== SHELTER_INDEX_FORMAT || index.version !== SHELTER_INDEX_VERSION) {
    throw new Error("not a GSI shelter index of this version");
  }
  if (!/^[0-9a-f]{64}$/.test(index.sha256) || typeof index.retrieved_at !== "string" || !Number.isInteger(index.invalid_record_count)) {
    throw new Error("a GSI shelter index without its snapshot's metadata");
  }
  if (index.codes === null || typeof index.codes !== "object" || Array.isArray(index.codes)) throw new Error("a GSI shelter index without its codes");
  return index;
}

/// `sorted` of (code, name) pairs; each keeps its normalized name third.
const sortPairs = (pairs) => pairs.sort((a, b) => codePointCompare(a[0], b[0]) || codePointCompare(a[1], b[1]));

/// Check a municipality code's rows the site's build wrote, for the index's snapshot.
export function checkShelterRows(rows, index, code) {
  if (rows === null || typeof rows !== "object" || rows.format !== SHELTER_ROWS_FORMAT || rows.version !== SHELTER_ROWS_VERSION || rows.code !== code) {
    throw new Error("not the GSI shelter rows of this code and version");
  }
  if (rows.sha256 !== index.sha256) throw new RegionProviderError("the site's GSI shelter rows are not of its index's snapshot; reload the page");
  if (JSON.stringify(rows.columns) !== JSON.stringify(SHELTER_ROW_COLUMNS) || !Array.isArray(rows.rows)) throw new Error("GSI shelter rows of other columns");
  return rows.rows.map((row) => {
    if (!Array.isArray(row) || row.length !== SHELTER_ROW_COLUMNS.length || typeof row[0] !== "boolean" || !row.slice(1).every((value) => value === null || typeof value === "string")) {
      throw new Error("a GSI shelter row of another shape");
    }
    const [named, commonId, name, address, latitude, longitude, accepted, notes] = row;
    return { named, commonId: pyStrip(commonId ?? ""), name, address, latitude, longitude, accepted, notes };
  });
}

/// `_parse_gsi_common_id`'s subtype of an overnight shelter's common ID ("1" or "2").
function shelterSubtype(commonId) {
  const match = /^E\d{5}\d{5}1([12])[0-9A-Za-z]$/.exec(commonId);
  if (match === null) throw new Error("a GSI shelter row without an overnight shelter's common ID");
  return match[1];
}

/// The rows of a pass of the lookup: the first of each common ID.
function firstOfEach(rows) {
  const seen = new Set();
  return rows.filter((row) => !seen.has(row.commonId) && seen.add(row.commonId));
}

/// `GsiShelterProvider.search_municipality(name, municipality_code)` on the site's index, and on
/// the rows of the code it matches (`shelterRows(code)`, read only then). Its ValueErrors are
/// RegionProviderErrors here (research.py answers both "unavailable").
async function shelterLookup(name, municipalityCode, index, casefold, shelterRows) {
  const requested = normalizeMunicipality(name, casefold);
  let code = municipalityCode === null ? null : pyStrip(municipalityCode);
  const codeWasSelected = code !== null;
  // Python's `\d` of a str pattern: any decimal digit (Nd).
  if (code !== null && !/^\p{Nd}{5}$/u.test(code)) throw new RegionProviderError("municipality code must be the five-digit code in the GSI common ID");
  if (code === null && Array.from(requested).length < 2) throw new RegionProviderError("municipality must be at least two characters");
  const namesOf = (of) => (Object.hasOwn(index.codes, of) ? index.codes[of] : []);
  const optionsOf = (codes) => sortPairs(codes.flatMap((of) => namesOf(of).map(([normalized, display]) => [of, display, normalized])));
  const exact = Object.keys(index.codes).filter((of) => index.codes[of].some(([normalized]) => normalized === requested));
  const lookup = { municipality: null, municipality_code: null, municipality_name_verified: false, complete: false, matched_count: 0, shelters: [], candidates: [], truncated: false };
  if (code === null) {
    if (exact.length > 1) return { ...lookup, status: "ambiguous_municipality", candidates: optionsOf(exact) };
    if (exact.length === 0) {
      const suggestions = sortPairs(
        Object.entries(index.codes).flatMap(([of, names]) => names.filter(([normalized]) => normalized.includes(requested)).map(([normalized, display]) => [of, display, normalized])),
      );
      return {
        ...lookup,
        status: "unresolved_municipality",
        candidates: suggestions.slice(0, MAX_MUNICIPALITY_CODE_SUGGESTIONS),
        truncated: suggestions.length > MAX_MUNICIPALITY_CODE_SUGGESTIONS,
      };
    }
    [code] = exact;
  }
  if (codeWasSelected && exact.length > 0 && !exact.includes(code)) {
    return { ...lookup, municipality_code: code, status: "ambiguous_municipality", candidates: optionsOf(exact) };
  }
  const names = namesOf(code);
  const complete = index.invalid_record_count === 0;
  if (names.length === 0 && codeWasSelected) return { ...lookup, municipality_code: code, status: "no_published_records", complete };
  const verified = names.some(([normalized]) => normalized === requested);
  if (names.length !== 1) return { ...lookup, municipality_code: code, status: "ambiguous_municipality", candidates: optionsOf([code]), municipality_name_verified: verified };
  const matched = { ...lookup, municipality: names[0][1], municipality_code: code, municipality_name_verified: verified };
  // A selected code's pass takes the rows with a municipality name; the pass after it (or after a
  // name resolved the code) takes every row of the code.
  const rows = checkShelterRows(await shelterRows(code), index, code);
  let picked = codeWasSelected ? firstOfEach(rows.filter((row) => row.named)) : [];
  if (picked.length === 0) picked = firstOfEach(rows);
  if (picked.length === 0) return { ...matched, status: "no_published_records", complete };
  if (picked.length > MAX_MUNICIPAL_SHELTERS) return { ...matched, status: "result_limit_exceeded", matched_count: picked.length };
  return { ...matched, status: "matched", complete, matched_count: picked.length, shelters: picked };
}

/// research.py's `_finite_coordinate` of a CSV value, or BrowserUnsupported where `float()` of
/// non-ASCII digits would read what JS does not.
function finiteCoordinate(value, lower, upper) {
  if (value === null || !pyStrip(value)) return null;
  if (/[^\x00-\x7f]/.test(pyStrip(value))) throw new BrowserUnsupported("GSIの避難所一覧の緯度・経度にASCIIでない文字");
  const number = pyFloatOf(value);
  return number !== null && Number.isFinite(number) && lower <= number && number <= upper ? number : null;
}

// ---- The research ------------------------------------------------------------------------------------

function dataSource(fields) {
  return {
    id: fields.id,
    title: fields.title,
    publisher: fields.publisher,
    url: fields.url,
    source_kind: fields.source_kind,
    accessed_at: fields.accessed_at,
    published_on: null,
    geographic_scope: fields.geographic_scope ?? null,
    license_note: fields.license_note ?? null,
    freshness: fields.freshness ?? "unknown",
    content_sha256: fields.content_sha256 ?? null,
    retrieved_at: fields.retrieved_at ?? null,
    etag: fields.etag ?? null,
    last_modified: fields.last_modified ?? null,
  };
}

function hazardLayers(disaster, casefold) {
  const text = casefold(disaster);
  const candidates = [
    [["flood", "flooding", "洪水", "浸水", "氾濫"], "洪水浸水想定区域", "https://disaportaldata.gsi.go.jp/raster/01_flood_l2_shinsuishin_data/{z}/{x}/{y}.png"],
    [["tsunami", "津波"], "津波浸水想定", "https://disaportaldata.gsi.go.jp/raster/04_tsunami_newlegend_data/{z}/{x}/{y}.png"],
  ];
  return candidates
    .filter(([terms]) => terms.some((term) => text.includes(term)))
    .map(([, title, tile]) => ({
      disaster,
      title,
      tile_url_template: tile,
      map_portal_url: GSI_HAZARD_URL,
      source_id: GSI_HAZARD_SOURCE_ID,
      status: "visual_reference_only",
      limitation: "Visual map reference only; coverage and legend must be checked by an operator.",
    }));
}

function unmappedHazardFamilies(disaster, casefold) {
  const text = casefold(disaster);
  const candidates = [
    ["地震", ["earthquake", "地震", "震災"]],
    ["土砂災害", ["landslide", "土砂", "崖崩れ", "がけ崩れ"]],
    ["高潮", ["storm_surge", "storm surge", "高潮"]],
    ["台風", ["typhoon", "台風"]],
    ["大雪", ["heavy_snow", "heavy snow", "大雪", "豪雪"]],
    ["猛暑", ["heatwave", "heat wave", "猛暑", "熱波"]],
  ];
  return candidates.filter(([, terms]) => terms.some((term) => text.includes(term))).map(([label]) => label);
}

/// The research request `scenario-preview` makes of a request (`RegionResearchRequest`'s dump).
export function researchRequest(request) {
  return {
    municipality_query: request.municipality,
    municipality_code: request.municipality_code ?? null,
    disaster: request.disaster,
    season: request.season ?? null,
    estat_stats_data_id: null,
    estat_area_code: null,
    estat_period_code: null,
    estat_category_codes: {},
    selected_municipality_name: request.municipality,
    target_date: null,
    weather_area_code: null,
  };
}

/// `RegionResearchService.research(request)` of a research request (`researchRequest`), offline:
/// `{document, text}`, the result's dump (floats as `PyFloat`) and its canonical JSON.
/// `casefold` is Python's `str.casefold` (the core's, `wasm.casefold`); `index` the site's shelter
/// index; `shelterRows(code)` the site's rows of a code of the index (`gsi_shelter_rows`), which
/// fails with a RegionProviderError when they cannot be read, as Python's CSV download does;
/// `fetchJson(url)` the address search's JSON (`gsiFetchJson`).
export async function research({ request, now, index, shelterRows, casefold, fetchJson = (url) => gsiFetchJson(url) }) {
  checkShelterIndex(index);
  const at = pyUtc(now);
  const sources = [];
  const municipalities = [];
  const providerStatuses = [];
  const hazards = [];
  const unknowns = [];
  const warnings = [];
  const shelters = [];
  let shelterRegister;

  try {
    const geocoded = await addressSearch(request.municipality_query, fetchJson);
    if (geocoded.length > 0) {
      sources.push(
        dataSource({
          id: GSI_ADDRESS_SOURCE_ID,
          title: "GSI address search",
          publisher: "Geospatial Information Authority of Japan",
          url: GSI_ADDRESS_URL,
          source_kind: "official",
          accessed_at: at,
          geographic_scope: "Japan; address search result points",
          freshness: "unknown",
          license_note: "Confirm data terms and result accuracy at the source before reuse.",
        }),
      );
      const selected = request.selected_municipality_name;
      for (const item of geocoded) {
        const valid = [item.latitude, item.longitude].every(Number.isFinite) && Math.abs(item.latitude) <= 90 && Math.abs(item.longitude) <= 180 && item.name.length > 0 && Array.from(item.name).length <= 240;
        if (!valid) throw new BrowserUnsupported("GSIの住所検索の答えの範囲外の値");
        municipalities.push({
          name: item.name,
          latitude: new PyFloat(item.latitude),
          longitude: new PyFloat(item.longitude),
          source_id: GSI_ADDRESS_SOURCE_ID,
          municipality_code: null,
          match_kind: selected && casefold(selected) === casefold(item.name) ? "exact" : "candidate",
        });
      }
      if (!selected) unknowns.push("自治体候補は未選択です。検索結果は自治体境界を示しません。");
    } else {
      unknowns.push("住所検索から自治体候補を確認できませんでした。");
    }
  } catch (error) {
    if (!(error instanceof RegionProviderError)) throw error;
    unknowns.push("GSI住所検索を利用できませんでした。");
    warnings.push(error.message);
  }

  unknowns.push("統計APIのアプリケーションIDがありません。ESTAT_APP_ID設定後に取得できます。");

  const selectedHazards = hazardLayers(request.disaster, casefold);
  const unavailable = unmappedHazardFamilies(request.disaster, casefold);
  if (selectedHazards.length > 0) {
    sources.push(
      dataSource({
        id: GSI_HAZARD_SOURCE_ID,
        title: "GSI Hazard Map Portal open data tile catalog",
        publisher: "Geospatial Information Authority of Japan",
        url: GSI_HAZARD_URL,
        source_kind: "official",
        accessed_at: at,
        geographic_scope: "Japan; layer availability and coverage vary",
        freshness: "current",
        license_note: "A displayed hazard tile is a map layer, not a verified numeric exposure result.",
      }),
    );
    hazards.push(...selectedHazards);
    warnings.push("ハザードは地図表示レイヤーへの参照です。色やタイルから浸水深・施設安全性を自動判定していません。");
    if (unavailable.length > 0) unknowns.push(`次の災害種別に対応するGSI地図レイヤーは自動選択していません: ${unavailable.join("、")}`);
  } else {
    unknowns.push(`指定災害に対応するGSI公開タイルを自動選択できませんでした。${unavailable.length > 0 ? ` 未対応の災害種別: ${unavailable.join("、")}` : ""}`);
  }

  for (const kind of ["municipal_plan", "hazard"]) {
    providerStatuses.push({
      data_kind: kind,
      status: "not_configured",
      source_id: null,
      record_count: 0,
      applicability: "No deployment-configured endpoint for the requested data class.",
      message: "自治体計画・災害数値の取得Providerが構成されていません。",
    });
    unknowns.push(kind === "municipal_plan" ? "自治体の地域防災計画Providerが未構成です。" : "数値ハザードデータProviderが未構成です。");
  }
  const weatherMessage = "気象庁予報区域コードが選択されていないため、予報を地域条件へ結び付けていません。";
  providerStatuses.push({
    data_kind: "weather",
    status: "not_configured",
    source_id: null,
    record_count: 0,
    applicability: "A six-digit JMA forecast area was not selected for this municipality.",
    message: weatherMessage,
  });
  unknowns.push(weatherMessage);

  const lookupName = request.selected_municipality_name || request.municipality_query;
  try {
    const lookup = await shelterLookup(lookupName, request.municipality_code, index, casefold, shelterRows);
    const tooLong = (text) => text !== null && Array.from(text).length > MAX_MUNICIPALITY_NAME;
    if (tooLong(lookup.municipality) || lookup.candidates.some(([, display]) => tooLong(display))) {
      throw new BrowserUnsupported("GSIの避難所一覧の240文字を超える自治体名");
    }
    const sourceId = GSI_SHELTER_SOURCE_PREFIX + index.sha256.slice(0, 12);
    sources.push(
      dataSource({
        id: sourceId,
        title: "GSI designated shelters (national CSV snapshot)",
        publisher: "Geospatial Information Authority of Japan",
        url: GSI_SHELTER_CSV_URL,
        source_kind: "official",
        accessed_at: at,
        retrieved_at: index.retrieved_at,
        content_sha256: index.sha256,
        etag: index.etag ?? null,
        last_modified: index.last_modified ?? null,
        geographic_scope: lookup.municipality || request.selected_municipality_name || request.municipality_query,
        freshness: "unknown",
        license_note:
          "GSI warns that records can be missing or out of date and that the municipality must confirm current designation. HTTP file timestamps and download time do not establish the legal designation date or municipality-level freshness.",
      }),
    );
    // `_normalize_municipality_name(display)` is the index's normalized name of the display name.
    const requested = normalizeMunicipality(lookupName, casefold);
    const candidates = lookup.candidates.map(([code, display, normalized]) => ({
      municipality_code: code,
      municipality_name: display,
      match_kind: requested === normalized ? "exact_name" : "text_suggestion",
    }));
    shelterRegister = {
      status: lookup.status,
      source_id: sourceId,
      requested_municipality_code: request.municipality_code,
      resolved_municipality_code: lookup.municipality_code,
      resolved_municipality_name: lookup.municipality,
      municipality_name_verified: lookup.municipality_name_verified,
      municipality_candidates: candidates,
      municipality_candidates_truncated: lookup.truncated,
      matched_count: lookup.matched_count,
      returned_count: lookup.shelters.length,
      complete: lookup.complete,
      invalid_record_count: index.invalid_record_count,
      content_sha256: index.sha256,
      retrieved_at: index.retrieved_at,
      stale_cache_fallback: false,
      municipality_update_metadata_status: "unknown",
    };
    if (index.invalid_record_count) warnings.push(`GSI避難所CSVで共通ID等を解釈できない行が${index.invalid_record_count}件あり、取得範囲は完全ではありません。`);
    if (lookup.status === "matched" && !lookup.municipality_name_verified) {
      warnings.push("自治体コードから避難所を照合しましたが、入力自治体名との完全一致は確認できませんでした。候補自治体名を確認してください。");
    }
    const listed = candidates.map((item) => `${item.municipality_name} (${item.municipality_code})`).join("、");
    if (lookup.status === "ambiguous_municipality") {
      unknowns.push(`GSI避難所一覧の完全一致名から自治体コードを一意に決められません。候補の自治体コードを選択してください: ${listed}`);
    } else if (lookup.status === "unresolved_municipality") {
      unknowns.push(
        `GSI避難所一覧は自治体コードで照合します。表示候補は名称検索による選択用の候補で、施設レコードの絞り込みには使っていません。GSI共通IDの5桁コードを確定してください。${listed ? ` 候補: ${listed}` : ""}`,
      );
      if (lookup.truncated) warnings.push("自治体コード候補は先頭30件のみ表示しています。検索語を具体化してください。");
    } else if (lookup.status === "result_limit_exceeded") {
      unknowns.push(`該当自治体のGSI避難所一覧は${lookup.matched_count}件あり、表示上限500件を超えました。部分結果は返していません。`);
    } else if (lookup.status === "no_published_records") {
      unknowns.push("選択した自治体コードに一致するGSI公開避難所レコードはありません。未掲載・非公開・更新遅れの可能性があり、自治体に確認してください。");
    } else if (lookup.shelters.length > 0) {
      for (const row of lookup.shelters) {
        const name = pyStrip(row.name ?? "");
        if (!name) continue;
        const notes = ["GSIの登録データです。公開・登録の最新性および現在の指定状況は自治体に確認してください。", "定員、面積、災害別の適合性はこのレコードから確認できません。"];
        if (row.accepted) notes.push(`受入対象者: ${pyStrip(row.accepted)}`);
        if (row.notes) notes.push(`自治体記載事項: ${pyStrip(row.notes)}`);
        const latitude = finiteCoordinate(row.latitude, -90, 90);
        const longitude = finiteCoordinate(row.longitude, -180, 180);
        shelters.push({
          id: `GSI-${row.commonId}`,
          name,
          address: pyStrip(row.address ?? "") || null,
          latitude: latitude === null ? null : new PyFloat(latitude),
          longitude: longitude === null ? null : new PyFloat(longitude),
          capacity: null,
          area_m2: null,
          supported_hazards: null,
          source_id: sourceId,
          source_kind: "official",
          freshness: "unknown",
          designation_type: "designated_shelter",
          shelter_subtype: shelterSubtype(row.commonId) === "1" ? "general" : "welfare",
          municipality_code: lookup.municipality_code,
          municipality_name: lookup.municipality,
          native_record_id: row.commonId,
          municipality_update_date: null,
          metadata_source_id: null,
          verified_by_user: false,
          notes,
        });
      }
      if (shelters.length === 0) unknowns.push("GSI避難所データから表示可能な施設名を抽出できませんでした。");
      warnings.push("GSI避難所一覧の候補です。未登録・非公開施設があり得るため、指定状況と利用可否は自治体へ確認してください。");
    }
  } catch (error) {
    if (!(error instanceof RegionProviderError)) throw error;
    shelterRegister = {
      status: "unavailable",
      source_id: GSI_SHELTER_SOURCE_ID,
      requested_municipality_code: request.municipality_code,
      resolved_municipality_code: null,
      resolved_municipality_name: null,
      municipality_name_verified: false,
      municipality_candidates: [],
      municipality_candidates_truncated: false,
      matched_count: 0,
      returned_count: 0,
      complete: false,
      invalid_record_count: 0,
      content_sha256: null,
      retrieved_at: null,
      stale_cache_fallback: false,
      municipality_update_metadata_status: "unknown",
    };
    sources.push(
      dataSource({
        id: GSI_SHELTER_SOURCE_ID,
        title: "GSI designated shelters (national CSV)",
        publisher: "Geospatial Information Authority of Japan",
        url: GSI_SHELTER_CSV_URL,
        source_kind: "official",
        accessed_at: at,
        geographic_scope: request.selected_municipality_name || request.municipality_query,
        freshness: "unknown",
        license_note: "GSI entries may be missing or outdated; confirm current designation with the municipality.",
      }),
    );
    unknowns.push("GSI避難所一覧を取得または自治体へ照合できませんでした。");
    warnings.push(error.message);
  }

  sources.push(
    dataSource({
      id: MLIT_SHELTER_SOURCE_ID,
      title: "National Land Numerical Information P20: evacuation facilities",
      publisher: "Ministry of Land, Infrastructure, Transport and Tourism",
      url: MLIT_P20_URL,
      source_kind: "official",
      accessed_at: at,
      geographic_scope: "Japan; source dataset is 2012 vintage",
      freshness: "historical",
      license_note: "Historical open data; do not treat it as a current designated shelter register.",
    }),
  );
  unknowns.push("国土数値情報P20は歴史資料です。自治体の最新指定状況やGSI一覧に未掲載の施設は、自治体の公式一覧で確認してください。");
  warnings.push("国土数値情報P20は2012年データのため、現行の指定状況として使用していません。");

  const document = {
    schema_version: 1,
    request,
    researched_at: at,
    sources,
    municipalities,
    statistical_tables: [],
    statistical_table_metadata: null,
    population_observations: [],
    municipal_plan_observations: [],
    hazard_observations: [],
    weather_observations: [],
    provider_statuses: providerStatuses,
    hazard_layers: hazards,
    shelters,
    shelter_register: shelterRegister,
    unknowns,
    warnings,
    assumptions: [],
    llm_source_candidates: [],
  };
  return { document, text: pyJson(document) };
}
