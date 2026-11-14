// The Scenario screen's routes in the browser (Issue #301, under #285): `POST {project}/scenario-preview`,
// `POST {project}/scenario-approve` and `POST {project}/runs`, answered as the Python server answers
// them without an API key (src/sheltersim/web/workspace.py, web/projects.py).
//
// - preview: the text is read by the core (`parseScenarioText`, #294) or the form's request
//   validated by it (`scenarioRequest`, #295); the regional research is research.mjs's (#300);
//   the Scenario is built by the core (`buildScenario`, #299). The answer is Python's: the
//   Scenario, `approved_hash`, `can_start`, `approval_blockers`, `warnings`, `research`,
//   `request`, `llm_usage` (offline: no request, no cost), `draft_id`, `facility_design`,
//   `geometry_hash`, `preview_id`. The preview is kept as Python keeps it, a draft
//   (`<project>/drafts/<preview>/v1.json` and `current.json`), and the preparation account in
//   `<project>/preparation.json`. Python's usage ledger (`usage/<account>.json`) is not written:
//   offline nothing is spent and no screen route reads it.
// - approve: the draft read back and checked as Python checks it; the Scenario saved
//   (project_store.mjs's `saveScenario`) and the Run created through `runs`.
// - runs: the Run's record with its `model_versions` and evidence (the core's `modelVersions`,
//   `runEvidence`), and its branch `main` at minute 0: a stored Run (run_store.mjs) holding the
//   world at minute 0 the core builds (`initialEntry`, #249), and the branch's record (its
//   `checkpoint_hash` the head of that stored Run, as compute.mjs writes it; no status yet, as
//   Python's branch without a computation has none). The browser computes
//   the Run on `resume` (compute.mjs).
//
// What Python reads that the screen never sends (a plan, a geometry, a selected shelter, e-Stat
// selections, a target date, a weather area, an execution mode other than offline) is refused as
// `browser_unsupported`. Values are written as Python's JSON: the core's bytes are put in as they
// are, never parsed and written again by JS.

import { ApiError } from "./api_answers.mjs";
import * as store from "./project_store.mjs";
import { BrowserUnsupported, PyFloat, pyJson, pyStrip, research, researchRequest } from "./research.mjs";
import { FORMAT, RunWriter, SAVE_EVERY } from "./run_store.mjs";

const encoder = new TextEncoder();
const decoder = new TextDecoder();
/// `ArtifactStore.DRAFT_BYTES`.
export const DRAFT_BYTES = 8 * 1024 * 1024;
const UNSUPPORTED = "ブラウザー版では未対応です";
const PREVIEW_OPTIONAL = [
  "request",
  "estat_stats_data_id",
  "estat_area_code",
  "estat_period_code",
  "estat_category_codes",
  "selected_shelter_id",
  "plan_override",
  "geometry_id",
  "draft_id",
  "target_date",
  "weather_area_code",
];
const ID = /^[A-Za-z0-9][A-Za-z0-9._:-]{0,63}$/;

const refused = (message) => new store.ProjectStoreError(message);
const unsupported = (what) => new ApiError(409, `${UNSUPPORTED}（${what}）`, "browser_unsupported");
const invalid = () => new ApiError(400, "invalid project request");

/// JSON text put into Python's JSON as it is.
export class RawJson {
  constructor(text) {
    this.text = text;
  }
}

/// `pyJson` with `RawJson` pieces.
export function answerJson(value) {
  if (value instanceof RawJson) return value.text;
  if (value instanceof PyFloat || value === null || typeof value !== "object") return pyJson(value);
  if (Array.isArray(value)) return `[${value.map(answerJson).join(",")}]`;
  return `{${Object.keys(value)
    .sort()
    .map((key) => `${JSON.stringify(key)}:${answerJson(value[key])}`)
    .join(",")}}`;
}

/// `fields(document, required, optional)` of web/projects.py.
function fields(document, required, optional = []) {
  const keys = Object.keys(document);
  if (keys.some((key) => !required.includes(key) && !optional.includes(key)) || required.some((key) => !keys.includes(key))) {
    throw refused("request fields do not match this operation");
  }
}

/// The JSON of a POST body: an object, or the server's 400.
export function bodyDocument(body) {
  let document;
  try {
    document = JSON.parse(body ?? "");
  } catch {
    throw invalid();
  }
  if (document === null || typeof document !== "object" || Array.isArray(document)) throw refused("project request must be an object");
  return document;
}

/// A core error (`invalid: …` / `unsupported: …` / `builder_refused: …`) as the server's answer.
function coreError(error, what) {
  const message = String(error?.message ?? error);
  if (message.startsWith("unsupported: ") || message.startsWith("browser_unsupported: ")) return unsupported(what);
  if (message.startsWith("invalid: ") || message.startsWith("builder_refused: ")) return invalid();
  return error;
}

const truthy = (value) =>
  value !== undefined && value !== null && value !== false && value !== 0 && value !== "" && !(Array.isArray(value) && value.length === 0) && !(typeof value === "object" && !Array.isArray(value) && Object.keys(value).length === 0);

/// The JSON text of the member `key` of canonical JSON `text` (keys sorted, no spaces), whose next
/// member is `next` (or none): its bytes as the core wrote them, which a `JSON.parse` would change
/// (`7.0` to `7`). A string never holds `,"next":` unescaped.
export function rawMember(text, key, next = null) {
  const start = text.indexOf(`"${key}":`);
  if (start < 0) throw new Error(`no member ${key}`);
  const end = next === null ? text.length - 1 : text.indexOf(`,"${next}":`, start);
  if (end < 0) throw new Error(`no member ${next}`);
  return text.slice(start + key.length + 3, end);
}

/// A uuid4's hex (Python's `uuid.uuid4().hex`).
export function uuidHex() {
  return crypto.randomUUID().replaceAll("-", "");
}

const bytesOf = (value) => (value instanceof Uint8Array ? value : encoder.encode(value));

/// `sha256(canonical(value))` of a Python document (`storage.projects.digest`) of its JSON text.
const digestOf = (wasm, text) => wasm.sha256Hex(encoder.encode(text));

export class ScenarioRoutes {
  /// `wasm`: the core's module; `tree`: the store; `shelterIndex()`: the site's GSI shelter index
  /// (research.mjs's), or null; `shelterRows(code)`: the site's rows of a code of it (research.mjs's);
  /// `fetchJson`: the address search (research.mjs's default when undefined); `now()` and
  /// `newId()`: the clock and uuid4's hex (fixed in tests).
  constructor({ wasm, tree, shelterIndex, shelterRows, fetchJson, now = () => new Date(), newId = uuidHex }) {
    Object.assign(this, { wasm, tree, shelterIndex, shelterRows, fetchJson, now, newId });
  }

  // ---- The preparation account (workspace.py's `_preparation_account`, `_bind_preparation`) ----

  async preparationAccount(projectId, requested) {
    if (requested !== undefined && requested !== null) {
      if (typeof requested !== "string" || !ID.test(requested)) throw refused("invalid artifact identifier");
      return requested;
    }
    const path = [projectId, "preparation.json"];
    const bytes = await this.tree.read(path);
    if (bytes !== null) {
      const pending = JSON.parse(decoder.decode(bytes));
      if (pending.status === "pending") return pending.account_id;
    }
    const accountId = this.newId();
    await this.tree.write(path, encoder.encode(`${answerJson({ schema_version: 1, account_id: accountId, status: "pending" })}\n`));
    return accountId;
  }

  async bindPreparation(projectId, accountId, runId) {
    const path = [projectId, "preparation.json"];
    const bytes = await this.tree.read(path);
    if (bytes === null) return;
    const pending = JSON.parse(decoder.decode(bytes));
    if (pending.account_id === accountId) {
      await this.tree.write(path, encoder.encode(`${answerJson({ ...pending, status: "bound", run_id: runId })}\n`));
    }
  }

  // ---- Drafts (storage/artifacts.py's `save_draft`, `draft`) ----

  async saveDraft(projectId, draftId, kind, contentText) {
    if (encoder.encode(contentText).length > DRAFT_BYTES) throw refused("draft_capacity_exceeded");
    const folder = [projectId, "drafts", draftId];
    if ((await this.tree.read([...folder, "current.json"])) !== null) throw refused("draft_conflict: reload the current revision");
    const record = { schema_version: 1, draft_id: draftId, kind, revision: 1, content_hash: digestOf(this.wasm, contentText) };
    const recordText = answerJson({ ...record, content: new RawJson(contentText) });
    await this.tree.write([...folder, "v1.json"], encoder.encode(`${recordText}\n`));
    await this.tree.write([...folder, "current.json"], encoder.encode(`${answerJson(record)}\n`));
  }

  async draft(projectId, draftId) {
    if (typeof draftId !== "string" || !ID.test(draftId)) throw refused("invalid artifact identifier");
    const pointer = await this.tree.read([projectId, "drafts", draftId, "current.json"]);
    if (pointer === null) throw refused("saved artifact is missing or corrupt");
    const { revision, content_hash: hash } = JSON.parse(decoder.decode(pointer));
    const bytes = await this.tree.read([projectId, "drafts", draftId, `v${revision}.json`]);
    if (bytes === null) throw refused("saved artifact is missing or corrupt");
    const text = decoder.decode(bytes).trimEnd();
    const record = JSON.parse(text);
    // The content's own bytes (its numbers as Python wrote them): what follows "content": in the
    // record's sorted JSON, up to the next key.
    const start = text.indexOf('"content":') + '"content":'.length;
    const end = text.lastIndexOf(',"content_hash":');
    const contentText = text.slice(start, end);
    if (digestOf(this.wasm, contentText) !== hash) throw refused("draft_integrity_mismatch");
    return { record, contentText };
  }

  // ---- POST {project}/scenario-preview ----

  async preview(projectId, document) {
    fields(document, ["text"], PREVIEW_OPTIONAL);
    await store.projectResponse(this.tree, projectId);
    const draftId = await this.preparationAccount(projectId, document.draft_id);
    const text = document.text;
    if (typeof text !== "string") throw refused("scenario text must be a string");
    let request = null;
    // The builder reads the request as Python's builder does: the form's as posted (a field left
    // out keeps the model's default, `duration_days` 7, an int), the text parser's dump otherwise.
    let posted = false;
    let inputSha256;
    let extractionSource;
    let parseWarnings = [];
    let missingFields = [];
    // A request as the core read it: `{request: RawJson, input_sha256}`.
    const formRequest = () => {
      let text;
      try {
        text = this.wasm.scenarioRequest(encoder.encode(JSON.stringify(document.request)));
      } catch (error) {
        throw coreError(error, "条件の入力の項目");
      }
      return { request: new RawJson(rawMember(text, "request")), input_sha256: JSON.parse(text).input_sha256 };
    };
    if (pyStrip(text)) {
      let parsedText;
      try {
        parsedText = this.wasm.parseScenarioText(text);
      } catch (error) {
        throw coreError(error, "文の読み取り");
      }
      const parsed = JSON.parse(parsedText);
      posted = truthy(document.request);
      request = posted ? formRequest().request : parsed.request === null ? null : new RawJson(rawMember(parsedText, "request", "source"));
      inputSha256 = parsed.input_sha256;
      extractionSource = parsed.source;
      parseWarnings = parsed.warnings;
      missingFields = parsed.missing_fields;
    } else if (truthy(document.request)) {
      const read = formRequest();
      posted = true;
      request = read.request;
      inputSha256 = read.input_sha256;
      extractionSource = "structured_form";
    } else {
      return answerJson({ can_start: false, missing_fields: ["municipality", "disaster", "evacuees"], warnings: [], draft_id: draftId });
    }
    if (request === null) return answerJson({ can_start: false, missing_fields: missingFields, warnings: parseWarnings, draft_id: draftId });
    for (const name of PREVIEW_OPTIONAL) {
      if (name !== "request" && name !== "draft_id" && document[name] !== undefined && document[name] !== null && !(name === "estat_category_codes" && !truthy(document[name]))) {
        throw unsupported(name);
      }
    }
    const index = this.shelterIndex ? await this.shelterIndex() : null;
    if (!index) throw unsupported("GSIの避難所一覧の索引がないサイト");
    let researched;
    try {
      researched = await research({
        request: researchRequest(JSON.parse(request.text)),
        now: this.now(),
        index,
        shelterRows: this.shelterRows,
        casefold: (value) => this.wasm.casefold(value),
        ...(this.fetchJson ? { fetchJson: this.fetchJson } : {}),
      });
    } catch (error) {
      if (error instanceof BrowserUnsupported) throw unsupported(error.message);
      throw error;
    }
    const requestText = request.text;
    let built;
    try {
      const asBuilt = posted ? JSON.stringify(document.request) : requestText;
      built = this.wasm.buildScenario(encoder.encode(asBuilt), encoder.encode(researched.text), inputSha256, extractionSource);
    } catch (error) {
      throw coreError(error, "シナリオの組み立て");
    }
    let scenarioText;
    let designText;
    let approvalBlockers;
    let builderWarnings;
    let approvedHash;
    let canStart;
    let geometryHash;
    try {
      ({ approvedHash, canStart, geometryHash } = built);
      scenarioText = decoder.decode(built.scenario()).trimEnd();
      designText = decoder.decode(built.facilityDesign()).trimEnd();
      approvalBlockers = JSON.parse(built.approvalBlockers());
      builderWarnings = JSON.parse(built.warnings());
    } finally {
      built.free();
    }
    // The draft keeps the research without the shelters' addresses (workspace.py's
    // `research_record`); the answer has them.
    const draftResearch = pyJson({ ...researched.document, shelters: researched.document.shelters.map(({ address, ...shelter }) => shelter) });
    const previewId = this.newId();
    const content = {
      usage_account_id: draftId,
      kind: "scenario",
      project_id: projectId,
      scenario: new RawJson(scenarioText),
      can_start: canStart,
      hash: approvedHash,
      approval_blockers: approvalBlockers,
      facility_design: new RawJson(designText),
      request: new RawJson(requestText),
      research: new RawJson(draftResearch),
    };
    await this.saveDraft(projectId, previewId, "scenario", answerJson(content));
    return answerJson({
      scenario: new RawJson(scenarioText),
      approved_hash: approvedHash,
      can_start: canStart,
      approval_blockers: approvalBlockers,
      warnings: [...parseWarnings, ...builderWarnings],
      research: new RawJson(researched.text),
      request: new RawJson(requestText),
      llm_usage: { mode: "offline", api_request_count: 0, spent_usd: new PyFloat(0) },
      draft_id: draftId,
      facility_design: new RawJson(designText),
      geometry_hash: geometryHash,
      preview_id: previewId,
    });
  }

  // ---- POST {project}/scenario-approve ----

  async approve(projectId, document) {
    fields(document, ["preview_id", "approved_hash", "approved", "run_id"]);
    const { contentText } = await this.draft(projectId, document.preview_id);
    const preview = JSON.parse(contentText);
    if (preview.project_id !== projectId) throw refused("draft belongs to another Project");
    if (preview.kind !== "scenario") throw refused("preview expired; generate and review a new preview");
    if (document.approved !== true || document.approved_hash !== preview.hash || !preview.can_start) {
      throw refused("approval must match a startable reviewed scenario");
    }
    if (typeof document.run_id !== "string" || !ID.test(document.run_id)) throw refused("invalid artifact identifier");
    const existing = await this.tree.read([projectId, "runs", document.run_id, "run.json"]);
    let scenarioId;
    let scenarioVersion;
    if (existing !== null) {
      const run = JSON.parse(decoder.decode(existing));
      if (run.scenario_hash !== preview.hash) throw refused("run_id_conflict");
      scenarioId = run.scenario_id;
      scenarioVersion = run.scenario_version;
    } else {
      // The Scenario's own bytes, from the draft: between "scenario": and the next key.
      const start = contentText.indexOf('"scenario":') + '"scenario":'.length;
      const end = contentText.lastIndexOf(',"usage_account_id":');
      const saved = await store.saveScenario(this.tree, projectId, { scenarioBytes: encoder.encode(contentText.slice(start, end)), scenarioHash: preview.hash });
      scenarioId = saved.scenario_id;
      scenarioVersion = saved.scenario_version;
    }
    const answer = await this.runs(projectId, { run_id: document.run_id, scenario_id: scenarioId, scenario_version: scenarioVersion, usage_account_id: preview.usage_account_id });
    await this.bindPreparation(projectId, preview.usage_account_id, document.run_id);
    return answer;
  }

  // ---- POST {project}/runs ----

  async runs(projectId, document) {
    fields(document, ["run_id", "scenario_id", "scenario_version"], ["usage_account_id", "execution_mode"]);
    if ("execution_mode" in document && document.execution_mode !== "offline") throw refused("only_explicit_offline_experiment_mode_is_supported");
    const runId = document.run_id;
    if (typeof runId !== "string" || !ID.test(runId)) throw refused("invalid artifact identifier");
    const existing = await this.tree.read([projectId, "runs", runId, "run.json"]);
    if (existing !== null) {
      const run = JSON.parse(decoder.decode(existing));
      if (run.scenario_id !== document.scenario_id || run.scenario_version !== document.scenario_version) throw refused("run_id_conflict");
      if ((await store.branches(this.tree, projectId, runId)).length > 0) return store.runResponse(this.tree, projectId, runId);
    }
    const scenarioBytes = await this.tree.read([projectId, "scenarios", document.scenario_id, `v${document.scenario_version}.scenario.json`]);
    if (scenarioBytes === null) throw refused("saved artifact is missing or corrupt");
    let entry;
    let modelVersions;
    let evidence;
    try {
      entry = this.wasm.initialEntry(scenarioBytes);
      modelVersions = JSON.parse(this.wasm.modelVersions(scenarioBytes));
      evidence = this.wasm.runEvidence(scenarioBytes);
    } catch (error) {
      throw coreError(error, "このシナリオの0分の世界");
    }
    const seed = JSON.parse(decoder.decode(scenarioBytes)).seed;
    await store.createRun(this.tree, projectId, {
      runId,
      scenarioId: document.scenario_id,
      scenarioVersion: document.scenario_version,
      seed,
      modelVersions,
      evidenceBytes: evidence,
    });
    const simulation = new this.wasm.Simulation(entry);
    let duration;
    try {
      duration = simulation.duration();
    } finally {
      simulation.free();
    }
    const backend = await store.branchBackend(this.tree, projectId, runId, "main");
    const writer = await RunWriter.create(this.wasm, backend);
    let head;
    try {
      writer.line({ kind: "run", format: FORMAT, export_sha256: this.wasm.sha256Hex(entry), export_bytes: entry.length, world: null, duration_minutes: duration, save_every: SAVE_EVERY });
      head = await writer.piece("export", entry);
    } finally {
      writer.close();
    }
    await store.writeBranch(this.tree, projectId, runId, { branch_id: "main", parent_branch_id: null, fork_minute: 0, computed_until: 0, checkpoint_hash: head, status: null });
    return store.runResponse(this.tree, projectId, runId);
  }
}
