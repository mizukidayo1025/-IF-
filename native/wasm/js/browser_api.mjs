// The browser's answers to the API the screen `/demo-v2` calls (Issue #266, stage A of #263).
//
// The page wraps `fetch` (js/api_fetch.js) and hands each `/api/v1/...` request to a dedicated
// Worker (js/api_worker.mjs), which answers it here from the origin private file system: the
// Project, Run and Branch records and each branch's stored Run (js/project_store.mjs,
// js/run_store.mjs), and WASM (sheltersim_wasm). An answer is what the Python server
// (src/sheltersim/web/server.py, web/projects.py) answers: its status and its JSON, `{"error", "code"}`
// for an error.
//
// Routes (under /api/v1/projects):
//   GET  (list), POST (create), GET {project}, GET {project}/runs, GET runs/{run},
//   GET runs/{run}/evidence                  the records (project_store.mjs's response functions)
//   GET runs/{run}/branches/{branch}/timeline?minute=   the frame of the minute
//   GET runs/{run}/replay-frames?branch_ids=&start=&end=   the frames of 1 to 8 minutes of one branch,
//                                            or of two branches, each minute's comparison (#307)
//   GET runs/{run}/compare?branch_ids=&minute=   js/comparison.mjs (#307)
//   GET runs/{run}/branches/{branch}/history, report, llm-status   js/api_answers.mjs (or the
//                                            functions the caller gives, `extra`)
//   POST runs/{run}/resume, POST runs/{run}/cancel   computing in the Worker (js/compute.mjs, #267)
//   POST runs/{run}/branches/{branch}/intervention-preview, intervention-apply   js/interventions.mjs
//                                            (#306); GET runs/{run} then holds the fork's `last_fork`
//   POST runs/{run}/branches/{branch}/questions   js/questions.mjs (#308): the answer without an API key
//   POST runs/{run}/pdf-report               the server's checks, then its answer without an API key:
//                                            409 pdf_analysis_offline (#309)
//   POST {project}/scenario-preview, scenario-approve, runs   js/scenario_routes.mjs (#301)
//   anything else (the routes outside projects)   409 {"error": "ブラウザー版では未対応です…",
//                                            "code": "browser_unsupported"}
//
// A frame is the core's (`Simulation.frame`, #264) with the world's digest as its `state_hash`, as
// Python's is. The Run at a minute: a branch keeps one live Simulation; a later minute steps it
// forward when that is cheaper than restoring the nearest saved point, any other minute seeks
// (`RunReader.seek`, #251) and frees the Simulation it replaces. Replay therefore steps the kept
// Simulation and never restores. At most `keep` Simulations are kept (the least recently used one
// is freed).
//
// The sample (installSample): the site holds the precomputed sample Project as one bundle
// (project_store.mjs, #265) split into parts no larger than a static host takes, listed in a small
// manifest (SAMPLE_FORMAT): the parts are fetched, each checked against its sha256, joined, the
// whole checked, and imported (`importProject`). The store keeps the manifest's sha256 beside the
// Projects, so a site with another sample replaces it once.

import { ApiError, errorAnswer, historyCache, historyResponse, llmStatusResponse, reportResponse } from "./api_answers.mjs";
import { CHUNK_TICKS, Computing, checkFields, postDocument } from "./compute.mjs";
import * as store from "./project_store.mjs";
import { compareResponse, replayPair } from "./comparison.mjs";
import { interventionApply, interventionPreview } from "./interventions.mjs";
import { questionResponse } from "./questions.mjs";
import { RunReader, SAVE_EVERY } from "./run_store.mjs";
import { bodyDocument, ScenarioRoutes } from "./scenario_routes.mjs";

/// The manifest of the site's sample: {format, version, project_id, bytes, sha256, parts: [{path,
/// bytes, sha256}]}, the paths relative to the manifest.
export const SAMPLE_FORMAT = "sheltersim-site-sample";
export const SAMPLE_VERSION = 1;
/// What the store keeps of the sample it imported, beside the Projects' folders.
export const SAMPLE_MARKER = "site-sample.json";
export const UNSUPPORTED = "ブラウザー版では未対応です";
const decoder = new TextDecoder();
const encoder = new TextEncoder();

// Errors are api_answers.mjs's: an `ApiError` with the server's status, or what the server's
// store refuses (a StorageError: 409, its code the message's leading word or "storage_conflict").
const refused = (message) => new store.ProjectStoreError(message);
const unsupported = (what) => new ApiError(409, `${UNSUPPORTED}（${what}）`, "browser_unsupported");

const json = (status, text) => ({ status, type: "application/json; charset=utf-8", body: text });

/// The query as Python's `parse_qs` reads it: blank values dropped, each name's values in order.
function queryOf(search) {
  const query = new Map();
  for (const [name, value] of new URLSearchParams(search)) {
    if (value === "") continue;
    if (!query.has(name)) query.set(name, []);
    query.get(name).push(value);
  }
  return query;
}

/// Python's `int()` of a query value: an optional sign and digits, spaces around allowed.
function integer(text, message) {
  const trimmed = text.trim().replaceAll("_", "");
  if (!/^[+-]?[0-9]+$/.test(trimmed)) throw refused(message);
  const value = Number(trimmed);
  if (!Number.isSafeInteger(value)) throw refused(message);
  return value;
}

/// The answers of one store. `wasm` is sheltersim_wasm, `tree` a project_store tree, `quota` what
/// the store may hold (the Project list's `storage.limit_bytes`), `extra` the routes another module
/// answers: `{ history, report, llmStatus }`, each `({ wasm, tree, projectId, runId, branchId,
/// query }) => JSON text`, `query` the request's query string as it came. By default they are
/// api_answers.mjs's (the history with a cache of the branches' samples). `locked(task)` runs each
/// task of the queue (a request or a chunk of a computation) holding the store's lock (the page:
/// a Web Lock), and `branchLock`, `chunkTicks` and `pause` are js/compute.mjs's `Computing`'s.
export class BrowserApi {
  constructor({ wasm, tree, quota = async () => null, extra = null, keep = 2, locked = (task) => task(), branchLock, chunkTicks = CHUNK_TICKS, pause, scenario = {} }) {
    this.wasm = wasm;
    this.tree = tree;
    this.quota = quota;
    this.history = historyCache(2);
    this.extra = extra ?? {
      history: (asked) => historyResponse({ ...asked, cache: this.history }),
      report: async (asked) => {
        // A report not yet made is made from the Run at its end, in a Simulation of its own:
        // free the kept ones first, as WASM's memory does not shrink.
        const kept = await tree.read([asked.projectId, "runs", asked.runId, "branches", asked.branchId, "report.json"]);
        if (kept === null) this.freeLive();
        return reportResponse(asked);
      },
      llmStatus: llmStatusResponse,
    };
    this.keep = keep;
    // "project/run/branch" -> { simulation, head, savedMinutes, used }
    this.live = new Map();
    this.used = 0;
    this.stats = { seeks: 0, steps: 0, restoreMs: 0, recomputeMs: 0, stepMs: 0, frameMs: 0, headFrames: 0 };
    this.queue = Promise.resolve();
    this.locked = locked;
    this.computing = new Computing({ api: this, branchLock, chunkTicks, pause });
    // The Scenario screen's routes (#301); `scenario` holds their `shelterIndex`, `fetchJson`,
    // `now` and `newId` (scenario_routes.mjs).
    this.scenarioRoutes = new ScenarioRoutes({ wasm, tree, ...scenario });
    // "project/run" -> the last fork's outcome (the server's `_fork_outcomes`, #306)
    this.forkOutcomes = new Map();
  }

  /// Answers a request: `{ method, path, search, body }` (body the text of a POST) to `{ status,
  /// type, body }`. Requests are answered one at a time, in the order they came.
  answer(request) {
    return this.enqueue(() => this.route(request));
  }

  /// Runs `task` after the requests and tasks queued before it, holding the store's lock.
  enqueue(task) {
    const done = this.queue.then(() => this.locked(task));
    this.queue = done.catch(() => {});
    return done;
  }

  async route({ method, path, search = "", body = null }) {
    try {
      return json(200, await this.dispatch(method, path, search, body));
    } catch (error) {
      if (!(error instanceof ApiError) && !(error instanceof store.ProjectStoreError)) console.error(error);
      const { status, body } = errorAnswer(error);
      return json(status, body);
    }
  }

  async dispatch(method, path, search, body) {
    if (!path.startsWith("/api/v1/")) throw new ApiError(404, "page not found");
    const parts = path.split("/").filter((part) => part !== "").map((part) => decodeURIComponent(part));
    if (parts[2] !== "projects") throw unsupported(path);
    const rest = parts.slice(3);
    const query = queryOf(search);
    if (method === "POST") return this.post(rest, body);
    if (method !== "GET") throw new ApiError(405, "inspector API is read-only");
    return this.get(rest, query, search);
  }

  async post(parts, body) {
    if (parts.length === 0) {
      let document;
      try {
        document = JSON.parse(body ?? "");
      } catch {
        throw new ApiError(400, "invalid project request");
      }
      if (document === null || typeof document !== "object" || Array.isArray(document)) throw refused("project request must be an object");
      const keys = Object.keys(document);
      if (keys.length !== 1 || keys[0] !== "project_id") throw refused("request fields do not match this operation");
      if (typeof document.project_id !== "string") throw new ApiError(400, "invalid project request");
      return JSON.stringify(await store.createProject(this.tree, document.project_id));
    }
    if (parts.length === 2 && ["scenario-preview", "scenario-approve", "runs"].includes(parts[1])) {
      const document = bodyDocument(body);
      const routes = this.scenarioRoutes;
      if (parts[1] === "scenario-preview") return routes.preview(parts[0], document);
      if (parts[1] === "scenario-approve") return routes.approve(parts[0], document);
      return routes.runs(parts[0], document);
    }
    if (parts.length === 4 && parts[1] === "runs" && (parts[3] === "resume" || parts[3] === "cancel")) {
      const document = postDocument(body);
      return parts[3] === "resume" ? this.computing.resume(parts[0], parts[2], document) : this.computing.cancel(parts[0], parts[2], document);
    }
    if (parts.length === 6 && parts[1] === "runs" && parts[3] === "branches" && (parts[5] === "intervention-preview" || parts[5] === "intervention-apply")) {
      const answer = parts[5] === "intervention-preview" ? interventionPreview : interventionApply;
      return answer(this, parts[0], parts[2], parts[4], postDocument(body));
    }
    if (parts.length === 6 && parts[1] === "runs" && parts[3] === "branches" && parts[5] === "questions") return questionResponse(this, parts[0], parts[2], parts[4], postDocument(body));
    if (parts.length === 4 && parts[1] === "runs" && parts[3] === "pdf-report") return this.pdfReport(parts[0], parts[2], postDocument(body));
    const route = parts.length >= 6 && parts[1] === "runs" ? parts.slice(3).join("/") : parts.slice(1).join("/");
    throw unsupported(route || "POST");
  }

  async get(parts, query, search = "") {
    const tree = this.tree;
    const noQuery = query.size === 0;
    if (parts.length === 0) {
      if (!noQuery) throw refused("projects list takes no query parameters");
      return store.projectsResponse(tree, { limitBytes: await this.quota() });
    }
    const [projectId, section, runId, action, branchId, branchAction] = parts;
    if (parts.length === 1 && noQuery) return store.projectResponse(tree, projectId);
    if (parts.length === 2 && section === "runs" && noQuery) return store.runsResponse(tree, projectId);
    if (parts.length === 3 && section === "runs" && noQuery) {
      const text = await store.runResponse(tree, projectId, runId);
      const outcome = this.forkOutcomes.get(`${projectId}/${runId}`);
      return outcome === undefined ? text : this.wasm.mergeJson(encoder.encode(text), encoder.encode(JSON.stringify({ last_fork: outcome })));
    }
    if (parts.length === 4 && section === "runs" && action === "evidence" && noQuery) return store.evidenceResponse(tree, projectId, runId);
    if (parts.length === 4 && section === "runs" && action === "replay-frames") return this.replay(projectId, runId, query);
    if (parts.length === 4 && section === "runs" && action === "compare" && query.size === 2 && query.has("branch_ids") && query.has("minute")) {
      return compareResponse(this, projectId, runId, query, integer);
    }
    if (parts.length === 4 && section === "runs" && action === "compare") throw refused("unknown project route or query");
    if (parts.length === 6 && section === "runs" && action === "branches") {
      if (branchAction === "timeline" && query.size === 1 && query.get("minute")?.length === 1) {
        const minute = integer(query.get("minute")[0], "minute must be an integer");
        const record = await this.branch(projectId, runId, branchId);
        if (minute < record.fork_minute || minute > record.computed_until) throw refused("minute is outside this branch's computed range");
        return this.frameText(projectId, runId, record, minute);
      }
      const named = { history: "history", report: "report", "llm-status": "llmStatus" }[branchAction];
      if (named) {
        if (typeof this.extra[named] !== "function") throw unsupported(branchAction);
        return this.extra[named]({ wasm: this.wasm, tree, projectId, runId, branchId, query: search });
      }
      throw unsupported(branchAction);
    }
    if (parts.length >= 2 && section !== "runs") throw unsupported(section);
    if (parts.length >= 4 && section === "runs") throw unsupported(action);
    throw refused("unknown project route or query");
  }

  /// `POST runs/{run}/pdf-report` (src/sheltersim/web/pdf_report.py `export`): the server's checks in
  /// its order (the fields, the scope, the Run, each branch, the computed range), then what the
  /// server answers without an API key: the analysis needs Opus, so no PDF is made. The server
  /// takes `minute` only as an int; JSON gives JS no way to tell 5.0 from 5, so 5.0 is taken as 5.
  async pdfReport(projectId, runId, document) {
    checkFields(document, ["branch_ids", "minute"]);
    const { branch_ids: ids, minute } = document;
    if (
      !Array.isArray(ids) ||
      ids.length < 1 ||
      ids.length > 2 ||
      ids.some((id) => typeof id !== "string") ||
      new Set(ids).size !== ids.length ||
      !Number.isInteger(minute) ||
      minute < 0
    ) {
      throw refused("pdf_invalid_scope: 単一BranchまたはA/Bの2分岐と整数分を指定してください。");
    }
    await store.runResponse(this.tree, projectId, runId);
    const records = [];
    for (const id of ids) records.push(await store.branchRecord(this.tree, projectId, runId, id));
    if (records.some((record) => !(record.fork_minute <= minute && minute <= record.computed_until))) {
      throw refused("pdf_uncomputed_range: 指定時刻は共通の計算済み範囲外です。");
    }
    throw refused("pdf_analysis_offline: Opusの記録応答またはAPI接続が必要です。");
  }

  /// The record of a branch; an error, as Python's, for one there is not.
  async branch(projectId, runId, branchId) {
    await store.runResponse(this.tree, projectId, runId);
    const record = (await store.branches(this.tree, projectId, runId)).find((each) => each.branch_id === branchId);
    if (!record) throw refused("saved artifact is missing or corrupt");
    if (record.checkpoint_hash === null) throw refused("branch has no computed minutes");
    return record;
  }

  /// `replay-frames` of one branch (Python's checks, in its order); two branches are a comparison
  /// (stage C).
  async replay(projectId, runId, query) {
    const names = [...query.keys()].sort();
    if (names.join(",") !== "branch_ids,end,start" || [...query.values()].some((values) => values.length !== 1)) {
      throw refused("replay parameters must each occur once");
    }
    const start = integer(query.get("start")[0], "replay bounds must be integers");
    const end = integer(query.get("end")[0], "replay bounds must be integers");
    const branchIds = query.get("branch_ids")[0].split(",");
    if (branchIds.length < 1 || branchIds.length > 2 || new Set(branchIds).size !== branchIds.length) throw refused("replay requires one or two distinct branches");
    if (!(0 <= start && start <= end && end < start + 8)) throw refused("replay interval must contain 1–8 integer boundaries");
    const records = [];
    for (const branchId of branchIds) records.push(await this.branch(projectId, runId, branchId));
    if (start < Math.max(...records.map((record) => record.fork_minute)) || end > Math.min(...records.map((record) => record.computed_until))) {
      throw refused("replay interval is outside the common computed range");
    }
    if (records.length === 2) return replayPair(this, projectId, runId, records, branchIds, start, end);
    const samples = [];
    for (let minute = start; minute <= end; minute += 1) samples.push(await this.frameText(projectId, runId, records[0], minute));
    return `{"branch_ids":${JSON.stringify(branchIds)},"end":${end},"samples":[${samples.join(",")}],"schema_version":1,"start":${start}}`;
  }

  /// The frame of a branch at a minute, as JSON text.
  async frameText(projectId, runId, record, minute) {
    const kept = this.computing.frameAt(projectId, runId, record, minute);
    if (kept !== null) {
      this.stats.headFrames += 1;
      return kept;
    }
    const simulation = await this.simulationAt(projectId, runId, record, minute);
    const started = performance.now();
    const bytes = simulation.frame(runId, record.branch_id, record.computed_until, undefined);
    this.stats.frameMs += performance.now() - started;
    return decoder.decode(bytes);
  }

  /// The branch's live Simulation at `minute`: stepped forward from the kept one when that does
  /// not cost more ticks than restoring the nearest saved point, else sought.
  async simulationAt(projectId, runId, record, minute) {
    const key = `${projectId}/${runId}/${record.branch_id}`;
    let live = this.live.get(key);
    if (live && live.head !== record.checkpoint_hash) {
      live.simulation.free();
      this.live.delete(key);
      live = undefined;
    }
    this.used += 1;
    if (live) {
      live.used = this.used;
      const now = live.simulation.minute();
      const point = Math.max(0, ...live.savedMinutes.filter((saved) => saved <= minute));
      if (now === minute) return live.simulation;
      if (now < minute && (now >= point || minute - now <= SAVE_EVERY)) {
        const started = performance.now();
        if (live.simulation.advance(minute - now) !== minute - now) throw new Error(`the Run does not reach minute ${minute}`);
        this.stats.steps += minute - now;
        this.stats.stepMs += performance.now() - started;
        return live.simulation;
      }
      // Free the kept world before the sought one is made: WASM's memory does not shrink.
      live.simulation.free();
      this.live.delete(key);
    }
    while (this.live.size >= this.keep) {
      const [oldest] = [...this.live.entries()].sort((a, b) => a[1].used - b[1].used);
      oldest[1].simulation.free();
      this.live.delete(oldest[0]);
    }
    const backend = await store.branchBackend(this.tree, projectId, runId, record.branch_id);
    const reader = await RunReader.open({ wasm: this.wasm, backend });
    try {
      const sought = await reader.seek(minute);
      this.stats.seeks += 1;
      this.stats.restoreMs += sought.restoreMs;
      this.stats.recomputeMs += sought.recomputeMs;
      this.live.set(key, { simulation: sought.simulation, head: record.checkpoint_hash, savedMinutes: reader.savedPoints().map((line) => line.minute), used: this.used });
      return sought.simulation;
    } finally {
      reader.close();
    }
  }

  /// Frees every kept Simulation.
  freeLive() {
    for (const { simulation } of this.live.values()) simulation.free();
    this.live.clear();
  }

  /// Frees every kept Simulation and sample table.
  close() {
    this.freeLive();
    this.history.clear();
  }
}

// ---- The site's sample -------------------------------------------------------------------------

/// Checks a sample manifest's form; an error otherwise.
export function checkSampleManifest(manifest) {
  const bad = (message) => {
    throw new Error(`not a sample manifest: ${message}`);
  };
  if (manifest?.format !== SAMPLE_FORMAT || manifest.version !== SAMPLE_VERSION) bad("another format or version");
  if (typeof manifest.project_id !== "string" || !/^[a-f0-9]{64}$/.test(manifest.sha256 ?? "") || !Number.isSafeInteger(manifest.bytes)) bad("no project, sha256 or size");
  if (!Array.isArray(manifest.parts) || manifest.parts.length === 0) bad("no parts");
  let total = 0;
  for (const part of manifest.parts) {
    if (typeof part?.path !== "string" || !/^[A-Za-z0-9._-]+$/.test(part.path) || !/^[a-f0-9]{64}$/.test(part.sha256 ?? "") || !Number.isSafeInteger(part.bytes)) bad("a part without its name, size or sha256");
    total += part.bytes;
  }
  if (total !== manifest.bytes) bad("the parts' sizes do not add up to the bundle's");
  return manifest;
}

/// Makes sure the store holds the site's sample: when the store's marker does not name the
/// manifest's bundle (or the Project is missing), fetches the parts (`fetchBytes(path)` →
/// Uint8Array, the path relative to the manifest), checks each and the whole against their sha256,
/// and imports the bundle, replacing a Project of the same id. `progress({ phase, bytes, total })`
/// is told as it goes. Returns `{ installed, projectId, ms }`.
export async function installSample({ wasm, tree, manifest, fetchBytes, readMarker, writeMarker, progress = () => {} }) {
  checkSampleManifest(manifest);
  const marker = await readMarker();
  if (marker?.sha256 === manifest.sha256 && (await tree.read([manifest.project_id, "project.json"])) !== null) {
    return { installed: false, projectId: manifest.project_id, ms: 0 };
  }
  const started = performance.now();
  const parts = [];
  let bytes = 0;
  progress({ phase: "fetch", bytes, total: manifest.bytes });
  for (const part of manifest.parts) {
    const got = await fetchBytes(part.path);
    if (got.length !== part.bytes || (await store.sha256Hex(got)) !== part.sha256) throw new Error(`the sample's part ${part.path} is not the one its manifest lists`);
    parts.push(got);
    bytes += got.length;
    progress({ phase: "fetch", bytes, total: manifest.bytes });
  }
  const whole = new Uint8Array(bytes);
  let at = 0;
  for (const part of parts.splice(0)) {
    whole.set(part, at);
    at += part.length;
  }
  if ((await store.sha256Hex(whole)) !== manifest.sha256) throw new Error("the sample's parts do not join into the bundle its manifest lists");
  progress({ phase: "import", bytes, total: manifest.bytes });
  const replace = (await tree.read([manifest.project_id, "project.json"])) !== null;
  const imported = await store.importProject({ wasm, tree, bundle: whole, replace });
  if (imported.projectId !== manifest.project_id) throw new Error("the sample's bundle is of another Project");
  await writeMarker(encoder.encode(`${JSON.stringify({ sha256: manifest.sha256, project_id: manifest.project_id })}\n`));
  progress({ phase: "done", bytes, total: manifest.bytes });
  return { installed: true, projectId: manifest.project_id, ms: performance.now() - started };
}
