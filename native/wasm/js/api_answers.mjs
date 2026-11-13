// The browser's answers to the API's KPI history, offline report and LLM status of a branch
// (Issue #266), in the JSON the Python server gives for the same Run:
//
//   GET /api/v1/projects/{p}/runs/{r}/branches/{b}/history?kind=kpis&start=&end=&step=&limit=&cursor=
//       historyResponse: from the branch's stored metric samples (js/run_store.mjs, #252), paged as
//       the history index pages them (src/sheltersim/web/project_queries.py,
//       src/sheltersim/storage/history_index.py)
//   GET …/branches/{b}/report       reportResponse: what the server writes when an offline Run of
//       the screen completes (web/workspace.py `finish_review`: the evidence review's rule
//       fallback), or {"status":"not_generated"} before
//   GET …/branches/{b}/llm-status   llmStatusResponse: the offline Run's answer (no router records)
//
// Each takes the store (`tree`, js/project_store.mjs) and the route's ids and query (the text after
// "?", read as Python's `parse_qs` reads it), and resolves to the answer's JSON text, its keys sorted
// as the server writes them (`json.dumps(sort_keys=True, separators=(",", ":"))`). A refusal is
// thrown as an `ApiError` with the server's HTTP status, `error` and `code`; `errorAnswer` turns it,
// or a `ProjectStoreError`, into the status and body the server gives (StorageError: 409, a query
// the contracts refuse: 400).
//
// The floats come from the core (`KpiHistory`, `Simulation.review_metrics`), which writes them as
// Python does: JS never reads a float of an answer and writes it again (1.0 would become 1).
//
// Where the browser differs from the server (docs/project-persistence.md lists these):
// - `anchor_hash` and the cursor's anchor are the branch's head (its record's `checkpoint_hash`,
//   the sha256 of its stored Run's last "end" line), not a Checkpoint's hash; a cursor is otherwise
//   the server's (the same scope, minute and key: see `cursorScope`).
// - `first_indexed_checkpoint_minute` is the branch's fork minute, where its stored Run starts.
// - Only `kind=kpis` is answered; any other kind is refused (`browser_unsupported`), and so is a
//   branch stored in a format whose samples lack the ledger's columns (`browser_run_outdated`).
// - The report is made the first time it is asked for, from the stored Run at its last minute,
//   and kept as the branch's `report.json`.
// - A number of the query takes ASCII digits only (Python's `int()` also takes other decimal
//   digits), and a cursor's minute written as 5.0 is taken as 5.

import { FORMAT, RunReader } from "./run_store.mjs";
import { ProjectStoreError, branchBackend, branchRecord, runScenarioBytes } from "./project_store.mjs";

const encoder = new TextEncoder();
const decoder = new TextDecoder();

/// A refusal with the server's HTTP status, message (`error`) and `code`.
export class ApiError extends Error {
  constructor(status, error, code = null) {
    super(error);
    this.name = "ApiError";
    this.status = status;
    this.code = code;
  }
}

/// StorageError's code: the given one, or the message's leading word where it is one, or
/// "storage_conflict" (src/sheltersim/storage/projects.py).
function storageCode(message, code = null) {
  if (code) return code;
  const candidate = message.split(":", 1)[0];
  return /^[a-z][a-z0-9_]{1,80}$/.test(candidate) ? candidate : "storage_conflict";
}

/// The server's StorageError: 409.
const storageError = (message, code = null) => new ApiError(409, message, storageCode(message, code));
/// The server's answer to a query its contracts refuse (a pydantic ValidationError or a bad int).
const invalidInput = () => new ApiError(400, "invalid query or saved contract", "invalid_input");

/// The status and body (JSON text) the server answers an error with: an `ApiError` as it is, a
/// `ProjectStoreError` as the StorageError it stands for, anything else as the server's 500.
export function errorAnswer(error) {
  if (error instanceof ApiError) return { status: error.status, body: sortedJson({ code: error.code, error: error.message }) };
  if (error instanceof ProjectStoreError) return { status: 409, body: sortedJson({ code: storageCode(error.message, error.code), error: error.message }) };
  return { status: 500, body: sortedJson({ error: "inspector request failed" }) };
}

/// JSON text already written (by the core, or a stored answer), put into an answer as it is.
class Raw {
  constructor(text) {
    this.text = text;
  }
}

/// JSON with sorted keys and no spaces, as the server writes it. Numbers are integers (a Number
/// or a BigInt); a float must come as `Raw` text from the core.
function sortedJson(value) {
  if (value instanceof Raw) return value.text;
  if (typeof value === "bigint") return value.toString();
  if (typeof value === "number") {
    if (!Number.isSafeInteger(value)) throw new Error(`not an integer of an answer: ${value}`);
    return String(value);
  }
  if (Array.isArray(value)) return `[${value.map(sortedJson).join(",")}]`;
  if (value !== null && typeof value === "object") {
    const keys = Object.keys(value).sort(byCodePoint);
    return `{${keys.map((key) => `${JSON.stringify(key)}:${sortedJson(value[key])}`).join(",")}}`;
  }
  return JSON.stringify(value);
}

/// Python's order of str: by code point (JS's `<` compares UTF-16 units).
function byCodePoint(a, b) {
  const [x, y] = [Array.from(a), Array.from(b)];
  for (let index = 0; index < Math.min(x.length, y.length); index++) {
    const difference = x[index].codePointAt(0) - y[index].codePointAt(0);
    if (difference !== 0) return difference;
  }
  return x.length - y.length;
}

const sha256 = (wasm, text) => wasm.sha256Hex(encoder.encode(text));
const length = (text) => Array.from(text).length;

// ---- The query --------------------------------------------------------------------------------

/// `urllib.parse.unquote(text.replace("+", " "))`: %XX escapes are bytes, read as UTF-8 with U+FFFD
/// for what is not; a % without two hex digits stays.
function unquote(text) {
  text = text.replaceAll("+", " ");
  if (!text.includes("%")) return text;
  const bytes = [];
  const raw = encoder.encode(text);
  for (let index = 0; index < raw.length; index++) {
    const hex = raw[index] === 0x25 && index + 2 < raw.length ? String.fromCharCode(raw[index + 1], raw[index + 2]) : "";
    if (/^[0-9A-Fa-f]{2}$/.test(hex)) {
      bytes.push(Number.parseInt(hex, 16));
      index += 2;
    } else {
      bytes.push(raw[index]);
    }
  }
  return decoder.decode(new Uint8Array(bytes));
}

/// The query as Python's `parse_qs` reads it: a Map of each name to its values, fields split at
/// "&", a field without "=" or with an empty value left out.
export function parseQuery(query) {
  const text = typeof query === "string" ? query.replace(/^\?/, "") : query instanceof URLSearchParams ? query.toString() : "";
  const fields = new Map();
  for (const field of text.split("&")) {
    if (field === "") continue;
    const at = field.indexOf("=");
    if (at < 0 || at === field.length - 1) continue;
    const name = unquote(field.slice(0, at));
    const value = unquote(field.slice(at + 1));
    if (!fields.has(name)) fields.set(name, []);
    fields.get(name).push(value);
  }
  return fields;
}

/// `int(text)` of Python: spaces around, a sign, decimal digits with single underscores between
/// them; a BigInt, or an invalid input (Python raises ValueError: 400). Only ASCII digits.
function pythonInt(text) {
  const match = /^\s*([+-]?)([0-9](?:_?[0-9])*)\s*$/u.exec(text);
  if (!match) throw invalidInput();
  const value = BigInt(match[2].replaceAll("_", ""));
  return match[1] === "-" ? -value : value;
}

/// pydantic's bool of a str (lax mode).
function pydanticBool(text) {
  const lower = text.toLowerCase();
  if (["1", "on", "t", "true", "y", "yes"].includes(lower)) return true;
  if (["0", "off", "f", "false", "n", "no"].includes(lower)) return false;
  throw invalidInput();
}

// `HistoryQuery` (src/sheltersim/contracts/workspace.py).
const KINDS = ["people", "households", "events", "causal", "kpis", "conversations", "social", "staff", "actions", "transitions", "dialogue", "care", "operations"];
const SELECTORS = {
  agent_id: ["people", "events", "causal", "social", "actions", "transitions", "dialogue", "care"],
  household_id: ["households", "events", "actions", "transitions", "dialogue", "care"],
  staff_id: ["staff", "actions", "dialogue", "operations"],
  conversation_id: ["conversations"],
  event_id: ["causal", "events", "social", "staff", "actions", "transitions", "dialogue", "care", "operations"],
};
const IDENTIFIERS = ["agent_id", "household_id", "event_id", "staff_id", "conversation_id", "zone_id", "event_type", "source_id", "status"];
const FIELDS = new Set(["kind", "start", "end", "step", "limit", "cursor", ...IDENTIFIERS, "text", "include_related"]);
const IDENTIFIER = /^[A-Za-z0-9][A-Za-z0-9._:-]{0,63}$/;

/// The history query of a route's query, checked as the server checks it (`one_query`, the ints,
/// `HistoryQuery.model_validate`): the fields of `HistoryQuery` but `schema_version`, ints as
/// BigInt.
export function historyQuery(query) {
  const fields = parseQuery(query);
  for (const [name, values] of fields) {
    if (!FIELDS.has(name) || values.length !== 1) throw storageError("invalid_query_fields");
  }
  const given = Object.fromEntries([...fields].map(([name, [value]]) => [name, value]));
  const ints = {};
  for (const name of ["start", "end", "step", "limit"]) if (name in given) ints[name] = pythonInt(given[name]);
  const checked = {
    kind: given.kind,
    start: ints.start ?? 0n,
    end: ints.end,
    step: ints.step ?? 1n,
    limit: ints.limit ?? 25n,
    cursor: given.cursor ?? null,
    text: given.text ?? null,
    include_related: "include_related" in given ? pydanticBool(given.include_related) : false,
  };
  for (const name of IDENTIFIERS) checked[name] = given[name] ?? null;
  if (!KINDS.includes(checked.kind) || checked.end === undefined) throw invalidInput();
  if (checked.start < 0n || checked.end < 0n || checked.step < 1n || checked.limit < 1n || checked.limit > 100n) throw invalidInput();
  if (checked.cursor !== null && length(checked.cursor) > 2048) throw invalidInput();
  if (checked.text !== null && (length(checked.text) < 1 || length(checked.text) > 160)) throw invalidInput();
  for (const name of IDENTIFIERS) if (checked[name] !== null && !IDENTIFIER.test(checked[name])) throw invalidInput();
  for (const [selector, kinds] of Object.entries(SELECTORS)) if (checked[selector] !== null && !kinds.includes(checked.kind)) throw invalidInput();
  if (checked.include_related && checked.kind !== "causal") throw invalidInput();
  if (checked.start > checked.end) throw invalidInput();
  if ((checked.kind === "people" && checked.agent_id === null) || (checked.kind === "households" && checked.household_id === null) || (checked.kind === "causal" && checked.event_id === null)) {
    throw invalidInput();
  }
  return checked;
}

/// Whether a KPI row could pass the query's filters: the index has no identity, type, source,
/// status or text term of a KPI row, so any of them leaves no rows (`HistoryIndex.search`).
function filtered(query) {
  // `_terms`: `re.findall(r"[\w:-]{1,80}", text.casefold())`; Python's \w is letters, numbers and _.
  const terms = query.text !== null && /[\p{L}\p{N}_:-]/u.test(query.text);
  return terms || ["zone_id", "event_type", "source_id", "status"].some((name) => query[name] !== null);
}

// ---- The cursor -------------------------------------------------------------------------------

function base64Url(bytes) {
  let binary = "";
  for (const byte of bytes) binary += String.fromCharCode(byte);
  return btoa(binary).replaceAll("+", "-").replaceAll("/", "_");
}

/// The `scope` a cursor of the index holds. `HistoryIndex.search` names two things `scope`: the
/// digest of the query, and then the SQL term that selects the rows of the branch and of each
/// ancestor before its fork (`_scope`), which is the one its cursors hold and are checked against.
/// So a cursor is bound to the branch's lineage and head, not to the query: a cursor of another
/// query of the same branch is taken, as the server takes it.
async function cursorScope(tree, projectId, runId, record) {
  const terms = ["r.branch=?"];
  const seen = new Set([record.branch_id]);
  for (let parent = record.parent_branch_id; parent !== null && !seen.has(parent); ) {
    seen.add(parent);
    terms.push("(r.branch=? AND r.minute<?)");
    parent = (await branchRecord(tree, projectId, runId, parent)).parent_branch_id;
  }
  return `(${terms.join(" OR ")})`;
}

/// `cursor_decode` of the index: the cursor's (minute, key) for this scope and anchor, or the
/// server's refusal.
function cursorRow(cursor, scope, anchor) {
  const stale = () => storageError("stale_or_foreign_cursor: restart this query");
  let data;
  try {
    // `base64.b64decode(value, altchars=b"-_", validate=True)`: both alphabets, padded.
    if (!/^[A-Za-z0-9+/_-]*={0,2}$/.test(cursor) || cursor.length % 4 !== 0) throw stale();
    const binary = atob(cursor.replaceAll("-", "+").replaceAll("_", "/"));
    data = JSON.parse(new TextDecoder("utf-8", { fatal: true }).decode(Uint8Array.from(binary, (char) => char.charCodeAt(0))));
  } catch {
    throw stale();
  }
  const keys = data !== null && typeof data === "object" && !Array.isArray(data) ? Object.keys(data).sort() : [];
  if (
    keys.join(",") !== "anchor,key,minute,scope" ||
    data.scope !== scope ||
    data.anchor !== anchor ||
    !Number.isInteger(data.minute) ||
    typeof data.key !== "string" ||
    length(data.key) > 128
  ) {
    throw stale();
  }
  // `position`: the key names the kind of its row.
  const at = data.key.indexOf(":");
  if (at < 0 || data.key.slice(0, at) !== "kpis") throw storageError("stale_or_foreign_cursor");
  return { minute: data.minute, key: data.key.slice(at + 1) };
}

// ---- History ----------------------------------------------------------------------------------

/// A cache of the samples read back (`KpiHistory`), by branch head: pass the same one to each call
/// of `historyResponse` (a Worker keeps one) so that paging reads the stored samples once. It
/// keeps the last `size` branches and frees the others' WASM memory.
export function historyCache(size = 2) {
  const kept = new Map();
  return {
    get(key) {
      const table = kept.get(key);
      if (table !== undefined) {
        kept.delete(key);
        kept.set(key, table);
      }
      return table;
    },
    set(key, table) {
      kept.set(key, table);
      while (kept.size > size) {
        const [oldest, value] = kept.entries().next().value;
        kept.delete(oldest);
        value.free();
      }
    },
    clear() {
      for (const table of kept.values()) table.free();
      kept.clear();
    },
  };
}

/// The stored samples of a branch read back (`KpiHistory`), checked against its "end" line.
async function readSamples(wasm, backend) {
  const reader = await RunReader.open({ wasm, backend });
  try {
    if (reader.run.format !== FORMAT) {
      throw storageError(`browser_run_outdated: the branch was stored as ${reader.run.format}, whose samples lack the ledger's columns; compute it again`);
    }
    const table = new wasm.KpiHistory();
    try {
      for (const line of reader.lines) {
        if (line.kind === "sample_tables") table.read_sample_tables(await reader.raw(line));
        else if (line.kind === "sample_block") table.read_sample_block(await reader.raw(line));
      }
      const end = reader.end();
      if (end !== null && end.samples !== table.sample_count()) throw new Error("the stored samples are not the ones the Run took");
      return table;
    } catch (error) {
      table.free();
      throw error;
    }
  } finally {
    reader.close();
  }
}

/// `GET …/branches/{branchId}/history?{query}`: the answer's JSON text. `cache` (`historyCache()`)
/// keeps the samples read between calls.
export async function historyResponse({ wasm, tree, projectId, runId, branchId, query = "", cache = null }) {
  const asked = historyQuery(query);
  const record = await branchRecord(tree, projectId, runId, branchId);
  if (!(BigInt(record.fork_minute) <= asked.start && asked.start <= asked.end && asked.end <= BigInt(record.computed_until))) {
    throw storageError("history_outside_computed_range");
  }
  if (asked.kind !== "kpis") throw storageError(`browser_unsupported: history of kind ${asked.kind} is not answered by the browser version`);
  const anchor = record.checkpoint_hash;
  const { cursor, limit } = asked;
  const scope = await cursorScope(tree, projectId, runId, record);
  const after = cursor === null ? null : cursorRow(cursor, scope, anchor);
  const start = Number(asked.start);
  const end = Number(asked.end);
  let table = null;
  let owned = false;
  if (anchor !== null) {
    const key = JSON.stringify([projectId, runId, branchId, anchor]);
    table = cache?.get(key) ?? null;
    if (table === null) {
      table = await readSamples(wasm, await branchBackend(tree, projectId, runId, branchId));
      if (cache) cache.set(key, table);
      else owned = true;
    }
  }
  try {
    let items = new Raw("[]");
    let next = null;
    let coverage = [0];
    if (table !== null) {
      const step = asked.step > BigInt(Number.MAX_SAFE_INTEGER) ? Number.MAX_SAFE_INTEGER : Number(asked.step);
      const page = table.page(start, end, step, Number(limit), after?.minute, after?.key, filtered(asked), runId, branchId);
      try {
        items = new Raw(page.items());
        if (page.more()) {
          const minute = page.last();
          next = base64Url(encoder.encode(sortedJson({ scope, anchor, minute, key: `kpis:${minute}` })));
        }
      } finally {
        page.free();
      }
      coverage = table.coverage(start, end);
    }
    const [count, oldest, newest] = coverage;
    return sortedJson({
      schema_version: 1,
      items,
      next_cursor: next,
      anchor_hash: anchor,
      indexed_through_minute: anchor === null ? null : record.computed_until,
      total_items: null,
      count_status: "not_calculated",
      query_strategy: "indexed_keyset",
      coverage: "persisted_index_records",
      coverage_scope: "branch_kind_time_window_before_identity_and_text_filters",
      completeness: "unknown",
      completeness_reason: "The index exposes persisted producer records but cannot certify that every domain event was produced or retained.",
      oldest_minute: count > 0 ? oldest : null,
      newest_minute: count > 0 ? newest : null,
      retention_limit: null,
      retention_limit_reason: "No per-kind source eviction boundary is persisted; absence of records is not evidence that no event occurred.",
      requested_range: [start, end],
      coverage_by_kind: count > 0 ? { kpis: { oldest_minute: oldest, newest_minute: newest, indexed_records: count } } : {},
      first_indexed_checkpoint_minute: anchor === null ? null : record.fork_minute,
      complete_lifetime_history: false,
      causal_interpretation: "Recorded model links, not empirical causal proof",
      run_id: runId,
      branch_id: branchId,
      kind: "kpis",
      computed_until: record.computed_until,
      time_semantics: "end_of_tick",
    });
  } finally {
    if (owned) table.free();
  }
}

// ---- Report -----------------------------------------------------------------------------------

/// The route's query must be empty: the server answers any other with this.
function noQuery(query) {
  if (parseQuery(query).size > 0) throw storageError("unknown project route or query");
}

// src/sheltersim/llm/report.py.
const ASSUMPTION_IDS = new Set([
  "ASM-SYNTHETIC-PROFILES-V1",
  "ASM-SIMPLE-PSYCHOLOGY-RATES-V1",
  "ASM-H04-CAUSAL-STRESS-V1",
  "ASM-UTILITY-BEHAVIOR-V1",
  "ASM-CORE-MODEL-SUITE-V2",
  "ASM-SLEEP-AWARE-BEHAVIOR-V2",
  "ASM-PRIORITY-RULE-BEHAVIOR-V1",
  "ASM-TOILET-SERVICE-DURATION-V1",
  "ASM-DAY3-TOILET-OUTAGE-V1",
  "ASM-SPACE-AND-STAFF-CAPACITY-V1",
  "ASM-SOCIAL-PROPAGATION-V1",
  "ASM-INFORMATION-SPREAD-V1",
  "ASM-STAFF-SUPPORT-LIFECYCLE-V1",
  "ASM-ZONE-ROUTING-V1",
  "ASM-EVENT-TRIGGERED-SOCIAL-LIMITS-V1",
  "ASM-SUPPORT-PRIORITY-LIMITS-V1",
  "ASM-EVENT-TRIGGERED-SOCIAL-INTERACTIONS-V1",
  "ASM-SYNTHETIC-ZONE-DISPLAY-V1",
  "ASM-H02-RESOURCE-SERVICE-V1",
  "ASM-H02-TEST-INITIAL-STOCK-V1",
  "ASM-H02-QUEUE-STALLS-V1",
  "ASM-H03-OPERATIONAL-EVENTS-V1",
  "ASM-H05-OPERATIONAL-CAPACITY-V1",
  "ASM-H05-OPERATIONAL-STOCK-V1",
  "ASM-H05-OPERATIONAL-SCHEDULE-V1",
  "ASM-H05-INTERVENTION-V1",
]);
const EVENT_TYPE_ALIASES = {
  facility_outage: "facility_outage",
  facility_recovery: "facility_recovery",
  water_outage: "water_outage",
  water_restored: "water_restored",
  resource_delivery: "resource_delivery",
  delivery_delayed: "delivery_delayed",
  staff_unavailable: "staff_unavailable",
  staff_shift_change: "staff_shift_change",
  power_outage: "power_outage",
  power_restored: "power_restored",
  equipment_failure: "equipment_failure",
  equipment_repaired: "equipment_repaired",
  extreme_heat: "extreme_heat",
  extreme_cold: "extreme_cold",
  social_interaction: "social_interaction",
  toilet_service_complete: "completion",
  staff_support_complete: "completion",
  support_request_timeout: "completion",
  agent_travel_complete: "completion",
};
const COMPLETIONS = new Set(["toilet_service_complete", "staff_support_complete", "support_request_timeout", "agent_travel_complete"]);
const PROMPT_CONSTRAINTS = [
  "Do not make claims about real people or real-world outcomes.",
  "Only reference metric IDs, event references and assumption IDs supplied below.",
  "Observed metric direction must match the supplied values exactly.",
  "Event classifications must match their recorded type and outcome.",
  "Hypotheses must be labeled as hypotheses and include evidence references.",
  "Return at most 8 observed KPIs, 8 event observations, 3 hypotheses, and 3 recommendations.",
  "Prefer the most decision-relevant recorded evidence; do not repeat a metric or event.",
  "Return only the required JSON object. Do not include prose or numbers.",
];
// `render_review_markdown` of a rule fallback with the empty review.
const FALLBACK_MARKDOWN = [
  "# Evidence-bound Opus review",
  "",
  "- Status: `rule_fallback`",
  "",
  "Opus review was unavailable; the recorded run evidence below remains authoritative.",
  "",
  "## Observed evidence",
  "",
  "No structured observations were returned.",
  "",
  "## Assumptions",
  "",
  "No assumption references were selected.",
  "",
  "## Hypotheses",
  "",
  "No hypotheses were selected.",
  "",
  "## Recommendations",
  "",
  "No recommendations were selected.",
  "",
  "## Limitations",
  "",
  "This review summarizes recorded synthetic simulation outputs. It does not establish real-world safety, behavior, or causal effects.",
  "",
].join("\n");

const isObject = (value) => value !== null && typeof value === "object" && !Array.isArray(value);

/// `scenario_assumption_ids(scenario)` of the Scenario's JSON (its canonical JSON holds every field).
export function scenarioAssumptionIds(scenario) {
  const ids = [];
  const configs = isObject(scenario.model_configs) ? scenario.model_configs : {};
  const list = (name) => (Array.isArray(scenario[name]) ? scenario[name] : []);
  if (list("profiles").some((profile) => (profile.provenance?.source_kind ?? "synthetic") === "synthetic")) ids.push("ASM-SYNTHETIC-PROFILES-V1");
  ids.push("ASM-SIMPLE-PSYCHOLOGY-RATES-V1", "ASM-H04-CAUSAL-STRESS-V1");
  const behavior = configs.behavior ?? {};
  ids.push(
    isObject(behavior) && behavior.policy === "priority-rule-v1"
      ? "ASM-PRIORITY-RULE-BEHAVIOR-V1"
      : isObject(behavior) && behavior.policy === "sleep-aware-utility-softmax-v2"
        ? "ASM-SLEEP-AWARE-BEHAVIOR-V2"
        : "ASM-UTILITY-BEHAVIOR-V1",
  );
  // `"toilet" in facility.kind.casefold()`.
  if (list("facilities").some((facility) => String(facility.kind).toLowerCase().includes("toilet"))) ids.push("ASM-TOILET-SERVICE-DURATION-V1");
  if (list("zones").length || list("staff").length || list("facilities").length) ids.push("ASM-SPACE-AND-STAFF-CAPACITY-V1");
  if (list("staff").length) ids.push("ASM-STAFF-SUPPORT-LIFECYCLE-V1");
  if (list("zones").length) ids.push("ASM-ZONE-ROUTING-V1");
  if ("social_propagation" in configs) ids.push("ASM-SOCIAL-PROPAGATION-V1");
  const information = configs.information_spread ?? {};
  if (isObject(information) && information.enabled === true) ids.push("ASM-INFORMATION-SPREAD-V1");
  ids.push("ASM-EVENT-TRIGGERED-SOCIAL-LIMITS-V1");
  if (list("staff").length) ids.push("ASM-SUPPORT-PRIORITY-LIMITS-V1");
  ids.push("ASM-EVENT-TRIGGERED-SOCIAL-INTERACTIONS-V1");
  const outage = "ASM-DAY3-TOILET-OUTAGE-V1";
  if (list("scheduled_events").some((event) => event.event_type === "facility_outage" && (event.source_id === outage || event.provenance?.source_id === outage))) ids.push(outage);
  const declared = isObject(scenario.provenance) ? scenario.provenance.model_assumption_ids : undefined;
  if (Array.isArray(declared)) ids.push(...declared.filter((value) => typeof value === "string" && ASSUMPTION_IDS.has(value)));
  return ids;
}

/// The report the server writes when an offline Run of the screen completes (`finish_review` with
/// the offline router: `generate_evidence_review` falls back to the empty review), of the Run
/// `simulation` at its last minute and the Scenario's JSON bytes: its JSON text.
export function buildReport({ wasm, simulation, scenarioBytes, branchId }) {
  const count = simulation.metric_count();
  const metrics = new Raw(count > 0 ? simulation.review_metrics(count - 1) : "{}");
  const generatedMinute = count > 0 ? JSON.parse(simulation.metric_line(count - 1))[0] : null;
  // `build_review_evidence` of the last 100 records: the critical ones, the last 20.
  const records = [];
  const total = simulation.log_records();
  for (let index = Math.max(0, total - 100); index < total; index++) {
    const text = simulation.log_record(index);
    if (text === undefined || text === null) throw new Error(`the log holds no record ${index}`);
    records.push(JSON.parse(text));
  }
  const critical = records
    .filter((record) => isObject(record.event) && (record.status === "failed" || record.status === "suppressed" || (Object.hasOwn(EVENT_TYPE_ALIASES, record.event.event_type) && !COMPLETIONS.has(record.event.event_type))))
    .slice(-20);
  const events = [];
  const eventIdByRef = {};
  critical.forEach((record, index) => {
    const event = record.event;
    if (typeof event.event_id !== "string" || event.event_id === "") return;
    const reference = `E${String(index + 1).padStart(3, "0")}`;
    eventIdByRef[reference] = event.event_id;
    const type = event.event_type;
    const status = record.status;
    events.push({
      event_ref: reference,
      minute: record.realized_minute ?? null,
      event_type: Object.hasOwn(EVENT_TYPE_ALIASES, type) ? type : "other",
      status: ["applied", "failed", "suppressed"].includes(status) ? status : "other",
      classification: status === "failed" ? "failure" : status === "suppressed" ? "suppressed" : (EVENT_TYPE_ALIASES[type] ?? "other"),
    });
  });
  const scenario = JSON.parse(decoder.decode(scenarioBytes));
  const assumptions = [...new Set(scenarioAssumptionIds(scenario))].filter((id) => ASSUMPTION_IDS.has(id)).sort();
  const prompt = sortedJson({
    task: "Select evidence-bound review codes for one shelter simulation run.",
    constraints: PROMPT_CONSTRAINTS,
    final_metrics: metrics,
    comparison_deltas: {},
    events,
    assumption_ids: assumptions,
    comparison_available: false,
  });
  return sortedJson({
    schema_version: 1,
    branch_id: branchId,
    minute: simulation.minute(),
    state_hash: simulation.stateDigest(),
    status: "rule_fallback",
    markdown: FALLBACK_MARKDOWN,
    review: {
      schema_version: 1,
      status: "rule_fallback",
      prompt_version: "evidence-review-v1",
      // `LLMTask.input_hash`: the sha256 of the input text's JSON.
      input_hash: sha256(wasm, JSON.stringify(prompt)),
      evidence: { metrics, events, assumptions, comparison_deltas: {}, event_id_by_ref: eventIdByRef, generated_minute: generatedMinute },
      review: { observed: [], event_observations: [], assumptions: [], hypotheses: [], recommendations: [] },
      validation_issue: null,
    },
  });
}

/// `GET …/branches/{branchId}/report`: the branch's `report.json`; for a completed branch without
/// one, the report of its stored Run at its last minute (`buildReport`), kept as `report.json`;
/// else `{"status":"not_generated"}`.
export async function reportResponse({ wasm, tree, projectId, runId, branchId, query = "" }) {
  noQuery(query);
  const record = await branchRecord(tree, projectId, runId, branchId);
  const path = [projectId, "runs", runId, "branches", branchId, "report.json"];
  const kept = await tree.read(path);
  if (kept !== null) return decoder.decode(kept).trimEnd();
  if (record.status !== "completed" || record.checkpoint_hash === null) return sortedJson({ status: "not_generated" });
  const scenarioBytes = await runScenarioBytes(tree, projectId, runId);
  const reader = await RunReader.open({ wasm, backend: await branchBackend(tree, projectId, runId, branchId) });
  let text;
  try {
    const { simulation } = await reader.seek(record.computed_until);
    try {
      text = buildReport({ wasm, simulation, scenarioBytes, branchId });
    } finally {
      simulation.free();
    }
  } finally {
    reader.close();
  }
  await tree.write(path, encoder.encode(`${text}\n`));
  return text;
}

// ---- LLM status -------------------------------------------------------------------------------

/// `summarize_call_outcomes([])`: an offline Run's router holds no records.
const NO_OUTCOMES = {
  accepted_count: 0,
  call_count: 0,
  fallback_by_error_kind: {},
  fallback_by_prompt_version: {},
  fallback_by_reason: {},
  fallback_count: 0,
  first_fallback_call_index: null,
  offline_rule_count: 0,
  status: "not_requested",
  summary_schema_version: 1,
};

/// `GET …/branches/{branchId}/llm-status`: an offline Run's answer.
export async function llmStatusResponse({ tree, projectId, runId, branchId, query = "" }) {
  noQuery(query);
  const record = await branchRecord(tree, projectId, runId, branchId);
  return sortedJson({
    schema_version: 1,
    branch_id: branchId,
    checkpoint_hash: record.checkpoint_hash,
    computed_until: record.computed_until,
    mode: "offline",
    outcomes: NO_OUTCOMES,
    record_scope: "router records in the branch head checkpoint; a child branch includes the parent's records before its fork",
  });
}
