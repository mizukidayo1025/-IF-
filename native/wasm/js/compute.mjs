// Computing a Run in the browser (Issue #267, stage B of #263): the browser's answers to
// `POST runs/{run}/resume` and `POST runs/{run}/cancel` (js/browser_api.mjs routes them here), the
// Run the site offers to compute, and what a page closed in the middle leaves.
//
// The Run to compute (ensureComputeRun). Route B of #263 without #249: the browser does not build
// a world from a Scenario; it goes on from the minute-0 world Python built and the sample's stored
// Run holds (its "export" piece). Beside the sample's Run, the Project gets a Run of the same
// Scenario that is not computed (COMPUTE_RUN): its records are the sample's with its own id and
// Python's status of a Run just created ("queued"), and its stored Run holds the sample's "run"
// line and its "export" piece, the compressed bytes copied as they are (never parsed). Its branch
// `main` is computed to minute 0, its head the "export" line.
//
// A branch's head. A branch's record (`computed_until`, `checkpoint_hash`) names a line of its
// stored Run's journal the Run can be read back at and gone on from: the "export" line (minute 0),
// a "saved" line whose blocks are all stored before it, or the "end" line. Its `checkpoint_hash`
// is that line's sha256 (project_store.mjs: the "end" line's for a Run stored to its end). The
// record is written after the line is flushed, so the record is the commit: lines after the head
// are a computation that did not reach its next head, and going on cuts them off first.
//
// Computing (Computing.resume). Python's checks and answers (src/sheltersim/web/projects.py
// `submit`, `cancel`), one computation at a time (the thread worker's max_jobs). The branch is cut
// back to its head, restored there (`RunReader.seek`, #251; the exported world at minute 0) and
// computed in chunks of at most `chunkTicks` ticks. Each chunk is a task of the store's queue (the
// requests' queue of browser_api.mjs, inside its Web Lock), so the Worker answers timeline,
// history and the records between chunks. At every `save_every` minutes (360, #263), at the
// target and when cancelled, the blocks are sealed (`Simulation.seal_blocks`) and a saved point
// is stored: the record then moves its head there, the frame of that minute is kept for the
// screen's next `timeline` of it, and the Run's records stay `running` until the computation
// ends: `completed` at the end of the clock (with the "world" and "end" lines, as runAndStore
// stores them), `paused` at the target or when cancelled, `failed` on an error. The OPFS files
// are opened for each chunk only: a file has one synchronous access handle at a time, and a read
// between chunks opens its own.
//
// A page closed in the middle (recoverInterrupted): the Worker dies with the page; the record
// keeps the last head, the journal may hold lines after it (or a line cut short, which reading
// ignores). On the next start a branch left `running` whose lock no Worker holds becomes
// `interrupted`, as Python's server start does; resuming it cuts the lines after its head off.
// The lock of a branch (`branchLock`, a Web Lock in the page) is held while it computes, so
// another tab neither computes it at the same time nor calls it interrupted.

import { ApiError } from "./api_answers.mjs";
import * as store from "./project_store.mjs";
import { FORMAT, RunReader, RunWriter, SAVE_EVERY, writeEnd } from "./run_store.mjs";

/// The id of the Run the site offers to compute, beside the sample's.
export const COMPUTE_RUN = "run-browser-compute";
/// Ticks per chunk by default: about a tenth of a second for 100 people.
export const CHUNK_TICKS = 60;
const DATA = "data.pack";
const JOURNAL = "journal.jsonl";
const encoder = new TextEncoder();
const decoder = new TextDecoder();
const refused = (message) => new store.ProjectStoreError(message);
/// Python's status of a Run that is computing, and of one a computation left (`_advance`).
const RUNNING = "running";

/// Python's `fields`: the document has the required keys and no others but the optional ones.
export function checkFields(document, required, optional = []) {
  const keys = Object.keys(document);
  if (keys.some((key) => !required.includes(key) && !optional.includes(key)) || required.some((key) => !keys.includes(key))) {
    throw refused("request fields do not match this operation");
  }
}

/// A POST's body as Python's server reads it: an error for an empty body (409), for one that is
/// not JSON (400) and for JSON that is not an object (409).
export function postDocument(body) {
  if (body === null || body === undefined || body === "") throw refused("project request exceeds the body limit");
  let document;
  try {
    document = JSON.parse(body);
  } catch {
    throw new ApiError(400, "invalid project request");
  }
  if (document === null || typeof document !== "object" || Array.isArray(document)) throw refused("project request must be an object");
  return document;
}

// ---- Journals and heads ------------------------------------------------------------------------

/// A journal's lines, up to the first that is cut short (no "\n") or does not match its sha256:
/// `{ entry, sha, end }`, `end` the byte just after the line.
export function journalEntries(wasm, bytes) {
  const entries = [];
  let at = 0;
  while (at < bytes.length) {
    const newline = bytes.indexOf(10, at);
    if (newline < 0) break;
    const text = decoder.decode(bytes.subarray(at, newline));
    const tab = text.lastIndexOf("\t");
    if (tab < 0) break;
    const json = text.slice(0, tab);
    const sha = text.slice(tab + 1);
    if (wasm.sha256Hex(encoder.encode(json)) !== sha) break;
    let entry;
    try {
      entry = JSON.parse(json);
    } catch {
      break;
    }
    entries.push({ entry, sha, end: newline + 1 });
    at = newline + 1;
  }
  return entries;
}

/// The lines of a journal a branch's head may be (see the top of this file), in order, each with
/// its minute and its place among the entries (`index`).
export function journalHeads(entries) {
  const heads = [];
  let logBlocks = 0;
  let sampleBlocks = 0;
  entries.forEach(({ entry, sha }, index) => {
    if (entry.kind === "log_block") logBlocks += 1;
    else if (entry.kind === "sample_block") sampleBlocks += 1;
    else if (entry.kind === "export") heads.push({ minute: 0, sha, index, kind: "export" });
    else if (entry.kind === "saved" && entry.log_blocks <= logBlocks && entry.sample_blocks <= sampleBlocks) heads.push({ minute: entry.minute, sha, index, kind: "saved" });
    else if (entry.kind === "end") heads.push({ minute: entry.minute, sha, index, kind: "end" });
  });
  return heads;
}

async function readEntries(wasm, backend) {
  const journal = await backend.open(JOURNAL);
  try {
    return journalEntries(wasm, journal.read(0, journal.size()));
  } finally {
    journal.close();
  }
}

/// Cuts a branch's stored Run back to the head `hash`: the journal's lines after it, then the
/// bytes of data.pack no line up to it points at. Returns the lines kept and the head.
async function cutToHead(wasm, backend, hash) {
  const journal = await backend.open(JOURNAL);
  const data = await backend.open(DATA);
  try {
    const entries = journalEntries(wasm, journal.read(0, journal.size()));
    const head = journalHeads(entries).find((each) => each.sha === hash);
    if (!head) throw new Error("the branch's head is not a line its stored Run can go on from");
    const kept = entries.slice(0, head.index + 1);
    let dataEnd = 0;
    for (const { entry } of kept) {
      if (Number.isSafeInteger(entry.offset) && Number.isSafeInteger(entry.length)) dataEnd = Math.max(dataEnd, entry.offset + entry.length);
    }
    const end = kept.at(-1).end;
    if (journal.size() > end) {
      journal.truncate(end);
      journal.flush();
    }
    if (data.size() > dataEnd) {
      data.truncate(dataEnd);
      data.flush();
    }
    return { entries: kept, head };
  } finally {
    journal.close();
    data.close();
  }
}

/// Where a writer going on after `entries` stands: the tables' sizes the last tables lines give and
/// the blocks stored, of the log and of the samples (RunWriter's `log` and `samples`).
function writerPlace(entries) {
  const log = { strings: 0, shapes: 0, blocks: 0 };
  const samples = { strings: 0, shapes: 0, blocks: 0 };
  for (const { entry } of entries) {
    const at = entry.kind.startsWith("log_") ? log : entry.kind.startsWith("sample_") ? samples : null;
    if (!at) continue;
    if (entry.kind.endsWith("_tables")) {
      at.strings = entry.strings;
      at.shapes = entry.shapes;
    } else if (entry.kind.endsWith("_block")) {
      at.blocks += 1;
    }
  }
  return { log, samples };
}

const sameCounts = (a, b) => a.length === b.length && a.every((value, index) => value === b[index]);

// ---- The Run to compute ------------------------------------------------------------------------

/// The Run of a Project whose main branch's stored Run the compute Run starts from: the first Run
/// (by id) other than `runId` whose branch `main` has a stored Run with an exported world.
async function sourceRun(wasm, tree, projectId, runId) {
  const { runs } = JSON.parse(await store.runsResponse(tree, projectId));
  for (const run of runs) {
    if (run.run_id === runId || !run.branches.some((branch) => branch.branch_id === "main" && branch.checkpoint_hash !== null)) continue;
    const backend = await store.branchBackend(tree, projectId, run.run_id, "main");
    if (journalHeads(await readEntries(wasm, backend)).some((head) => head.kind === "export")) return run;
  }
  return null;
}

async function createComputeRun(wasm, tree, projectId, runId) {
  const source = await sourceRun(wasm, tree, projectId, runId);
  if (!source) return null;
  const reader = await RunReader.open({ wasm, backend: await store.branchBackend(tree, projectId, source.run_id, "main") });
  let runLine;
  let exportLine;
  let compressed;
  try {
    runLine = reader.run;
    exportLine = reader.lines.find((line) => line.kind === "export");
    await reader.raw(exportLine);
    compressed = reader.data.read(exportLine.offset, exportLine.length);
  } finally {
    reader.close();
  }
  const backend = await store.branchBackend(tree, projectId, runId, "main");
  const writer = new RunWriter(wasm, await backend.open(DATA, { truncate: true }), await backend.open(JOURNAL, { truncate: true }));
  let head;
  try {
    writer.line({
      kind: "run",
      format: FORMAT,
      export_sha256: runLine.export_sha256,
      export_bytes: runLine.export_bytes,
      world: runLine.world ?? null,
      duration_minutes: runLine.duration_minutes,
      save_every: runLine.save_every || SAVE_EVERY,
    });
    writer.data.write(compressed, 0);
    writer.data.flush();
    head = writer.line({ kind: "export", offset: 0, length: compressed.length, raw_length: exportLine.raw_length, sha256: exportLine.sha256 });
  } finally {
    writer.close();
  }
  const evidence = await tree.read([projectId, "runs", source.run_id, "evidence.json"]);
  if (evidence === null) throw refused("saved artifact is missing or corrupt");
  await store.createRun(tree, projectId, {
    runId,
    scenarioId: source.scenario_id,
    scenarioVersion: source.scenario_version,
    seed: source.seed,
    modelVersions: source.model_versions,
    evidenceBytes: evidence,
  });
  await store.writeBranch(tree, projectId, runId, { branch_id: "main", parent_branch_id: null, fork_minute: 0, computed_until: 0, checkpoint_hash: head, status: "queued" });
  return { created: true, runId, source: source.run_id };
}

function parsedRecord(bytes) {
  try {
    const value = JSON.parse(decoder.decode(bytes));
    return value !== null && typeof value === "object" && value.schema_version === 1 ? value : null;
  } catch {
    return null;
  }
}

/// Makes sure Project `projectId` holds the Run to compute (`runId`), and that its records can be
/// read and name a head of its stored Run: creates it when it is missing (nothing when the Project
/// has no Run to start from), and mends what a page closed while writing may leave: a Run's record
/// cut short or a stored Run without a head (the Run is made again), a branch's record cut short or
/// naming no head of its stored Run (written again at the stored Run's last head, `interrupted`
/// past minute 0). Returns `{ created, mended, runId }`, or null for no Project or no Run to start
/// from.
export async function ensureComputeRun({ wasm, tree, projectId, runId = COMPUTE_RUN }) {
  if ((await tree.read([projectId, "project.json"])) === null) return null;
  const runFolder = [projectId, "runs", runId];
  const runBytes = await tree.read([...runFolder, "run.json"]);
  if (runBytes === null) return createComputeRun(wasm, tree, projectId, runId);
  const backend = await store.branchBackend(tree, projectId, runId, "main");
  const heads = journalHeads(await readEntries(wasm, backend));
  if (parsedRecord(runBytes)?.run_id !== runId || heads.length === 0) {
    await tree.remove(runFolder);
    const made = await createComputeRun(wasm, tree, projectId, runId);
    return made && { ...made, mended: true };
  }
  const branchPath = [...runFolder, "branches", "main", "branch.json"];
  const bytes = await tree.read(branchPath);
  const record = bytes === null ? null : parsedRecord(bytes);
  if (record && heads.some((head) => head.sha === record.checkpoint_hash && head.minute === record.computed_until)) return { created: false, mended: false, runId };
  const last = heads.at(-1);
  const status = last.kind === "end" ? "completed" : last.minute === 0 ? "queued" : "interrupted";
  await tree.remove(branchPath);
  await store.writeBranch(tree, projectId, runId, { branch_id: "main", parent_branch_id: null, fork_minute: 0, computed_until: last.minute, checkpoint_hash: last.sha, status });
  await store.setRunStatus(tree, projectId, runId, status);
  return { created: false, mended: true, runId };
}

/// A branch left `running` that no Worker computes (its lock is free: `branchLock(name)` gives a
/// release) was interrupted: its record and its Run's become `interrupted` (Python's
/// `recover_interrupted` at the server's start), its head kept. Every Project and Run is looked
/// at; one whose records cannot be read is left as it is. Returns the branches recovered, as
/// "project/run/branch".
export async function recoverInterrupted({ tree, branchLock }) {
  const recovered = [];
  for (const project of await store.projects(tree)) {
    let runs;
    try {
      ({ runs } = JSON.parse(await store.runsResponse(tree, project.project_id)));
    } catch {
      continue;
    }
    for (const run of runs) {
      for (const branch of run.branches) {
        if (branch.status !== RUNNING) continue;
        const release = await branchLock(lockName(project.project_id, run.run_id, branch.branch_id));
        if (!release) continue;
        try {
          const record = await store.branchRecord(tree, project.project_id, run.run_id, branch.branch_id);
          if (record.status !== RUNNING) continue;
          await store.writeBranch(tree, project.project_id, run.run_id, { ...record, status: "interrupted" });
          await store.setRunStatus(tree, project.project_id, run.run_id, "interrupted");
          recovered.push(`${project.project_id}/${run.run_id}/${branch.branch_id}`);
        } finally {
          release();
        }
      }
    }
  }
  return recovered;
}

/// The name of a branch's lock (a Web Lock in the page).
export const lockName = (projectId, runId, branchId) => `sheltersim-compute/${projectId}/${runId}/${branchId}`;

// ---- Computing ---------------------------------------------------------------------------------

/// The computations of one store (one at a time). `api` is the BrowserApi whose queue the chunks
/// go through (`api.enqueue(task)`), `branchLock(name)` gives a release function, or null when
/// another holder has the lock, and `pause()` lets the Worker take its messages between chunks.
export class Computing {
  constructor({ api, branchLock = async () => () => {}, chunkTicks = CHUNK_TICKS, pause = () => new Promise((resolve) => setTimeout(resolve, 0)) }) {
    this.api = api;
    this.branchLock = branchLock;
    this.chunkTicks = chunkTicks;
    this.pause = pause;
    // "project/run/branch" -> the job
    this.jobs = new Map();
    // The frame of the last head a computation reached: { key, head, minute, text }.
    this.headFrame = null;
    // Each computation's figures, for checks.
    this.done = [];
  }

  /// `POST runs/{run}/resume` (Python's `submit`, its checks in its order): the answer's text.
  async resume(projectId, runId, document) {
    const { wasm, tree } = this.api;
    checkFields(document, ["branch_id", "target_minute"], ["measure_performance"]);
    if (typeof (document.measure_performance ?? false) !== "boolean") throw refused("measure_performance must be boolean");
    const branchId = document.branch_id;
    const key = `${projectId}/${runId}/${branchId}`;
    if (this.jobs.has(key)) throw refused("background worker is busy; wait or pause the active run");
    if (this.jobs.size >= 1) throw refused("background workers are busy; wait until a running computation ends");
    const record = await store.branchRecord(tree, projectId, runId, branchId);
    await store.runResponse(tree, projectId, runId);
    const scenario = JSON.parse(decoder.decode(await store.runScenarioBytes(tree, projectId, runId)));
    const target = document.target_minute;
    if (!Number.isSafeInteger(target) || !(record.computed_until < target && target <= scenario.duration_minutes)) {
      throw refused("target minute must be in the uncomputed remaining range");
    }
    if (record.checkpoint_hash === null) throw refused("branch has no computed minutes");
    const release = await this.branchLock(lockName(projectId, runId, branchId));
    if (!release) throw refused("run_worker_busy: another process writes this branch");
    try {
      // The kept worlds go first: WASM's memory does not shrink, and the computation holds one.
      this.api.freeLive();
      await store.writeBranch(tree, projectId, runId, { ...record, status: RUNNING });
      await store.setRunStatus(tree, projectId, runId, RUNNING);
    } catch (error) {
      release();
      throw error;
    }
    const job = { key, projectId, runId, branchId, target, cancel: false, release, from: record.computed_until };
    this.jobs.set(key, job);
    job.finished = this.run(job);
    return JSON.stringify({ branch_id: branchId, run_id: runId, status: RUNNING, target_minute: target });
  }

  /// `POST runs/{run}/cancel` (Python's): asks the Run's computations (of `branch_id` only, when
  /// given) to stop after their chunk; the Run's record as it is now.
  async cancel(projectId, runId, document) {
    checkFields(document, [], ["branch_id"]);
    const jobs = [...this.jobs.values()].filter((job) => job.projectId === projectId && job.runId === runId && (document.branch_id == null || document.branch_id === job.branchId));
    for (const job of jobs) job.cancel = true;
    const text = await store.runResponse(this.api.tree, projectId, runId);
    if (jobs.length === 0 && JSON.parse(text).status === RUNNING) throw refused("run_worker_busy: cancellation must reach the owning worker");
    return text;
  }

  /// The frame text of a branch's head a computation reached, when `record` is still at it.
  frameAt(projectId, runId, record, minute) {
    const kept = this.headFrame;
    if (!kept || kept.key !== `${projectId}/${runId}/${record.branch_id}`) return null;
    return kept.head === record.checkpoint_hash && kept.minute === minute && record.computed_until === minute ? kept.text : null;
  }

  /// A computation, a chunk at a time through the store's queue, to its status.
  async run(job) {
    const started = performance.now();
    let state = null;
    let status = "failed";
    try {
      await this.pause();
      state = await this.api.enqueue(() => this.start(job));
      for (;;) {
        await this.pause();
        const ended = await this.api.enqueue(() => this.chunk(job, state));
        if (ended) {
          status = ended;
          break;
        }
      }
    } catch (error) {
      console.error(error);
      job.error = String(error?.message ?? error);
    }
    await this.api
      .enqueue(async () => {
        state?.simulation.free();
        const { tree } = this.api;
        const record = await store.branchRecord(tree, job.projectId, job.runId, job.branchId);
        await store.writeBranch(tree, job.projectId, job.runId, { ...record, status });
        await store.setRunStatus(tree, job.projectId, job.runId, status);
        this.done.push({
          branch: job.key,
          from: job.from,
          target: job.target,
          reached: record.computed_until,
          status,
          error: job.error ?? null,
          ms: performance.now() - started,
          tickMs: state?.tickMs ?? 0,
          saveMs: state?.saveMs ?? 0,
          memoryBytes: this.api.wasm.memoryBytes(),
        });
      })
      .catch((error) => console.error(error))
      .finally(() => {
        this.jobs.delete(job.key);
        job.release();
      });
    return status;
  }

  /// Cuts the branch back to its head and restores the Run there.
  async start(job) {
    const { wasm, tree } = this.api;
    const record = await store.branchRecord(tree, job.projectId, job.runId, job.branchId);
    const backend = await store.branchBackend(tree, job.projectId, job.runId, job.branchId);
    const { entries, head } = await cutToHead(wasm, backend, record.checkpoint_hash);
    const reader = await RunReader.open({ wasm, backend });
    let sought;
    try {
      if (reader.run.format !== FORMAT) throw new Error(`browser_run_outdated: the branch was stored as ${reader.run.format}; compute it again`);
      sought = await reader.seek(head.minute);
    } finally {
      reader.close();
    }
    const simulation = sought.simulation;
    const place = writerPlace(entries);
    if (head.kind === "saved") {
      const fits =
        simulation.minute() === head.minute &&
        sought.from === head.minute &&
        sameCounts(simulation.log_table_counts(), [place.log.strings, place.log.shapes]) &&
        sameCounts(simulation.log_block_counts(), [place.log.blocks, place.log.blocks]) &&
        sameCounts(simulation.sample_table_counts(), [place.samples.strings, place.samples.shapes]) &&
        sameCounts(simulation.sample_block_counts(), [place.samples.blocks, place.samples.blocks]);
      if (!fits) {
        simulation.free();
        throw new Error("the Run restored at the branch's head does not hold what its stored Run holds there");
      }
    }
    return { simulation, backend, place, record, duration: reader.duration(), saveEvery: reader.run.save_every || SAVE_EVERY, head: head.minute, tickMs: 0, saveMs: 0 };
  }

  /// One chunk: at most `chunkTicks` ticks, not past the next saved point or the target, then
  /// the blocks sealed since stored, and at a saved point, the target or the clock's end the
  /// branch's new head. Returns the computation's status once it ends, else null.
  async chunk(job, state) {
    const { wasm } = this.api;
    const { simulation, duration, saveEvery } = state;
    const writer = new RunWriter(wasm, await state.backend.open(DATA), await state.backend.open(JOURNAL));
    writer.log = state.place.log;
    writer.samples = state.place.samples;
    try {
      let minute = simulation.minute();
      if (job.cancel) {
        if (minute > state.head) await this.commit(job, state, minute, await this.save(writer, state));
        return minute === duration ? "completed" : "paused";
      }
      const next = (Math.floor(minute / saveEvery) + 1) * saveEvery;
      const step = Math.min(this.chunkTicks, next - minute, job.target - minute);
      const started = performance.now();
      const ran = simulation.advance(step);
      state.tickMs += performance.now() - started;
      minute = simulation.minute();
      if (ran !== step) throw new Error(`the Run stopped at minute ${minute}`);
      await writer.take(simulation, false);
      if (minute === duration) {
        await this.save(writer, state);
        const { sha } = await writeEnd(writer, simulation);
        await this.commit(job, state, minute, sha);
        return "completed";
      }
      if (minute === job.target || minute % saveEvery === 0) await this.commit(job, state, minute, await this.save(writer, state));
      return minute === job.target ? "paused" : null;
    } finally {
      writer.close();
    }
  }

  async save(writer, state) {
    const started = performance.now();
    const sha = await writer.save(state.simulation, { seal: true });
    state.saveMs += performance.now() - started;
    return sha;
  }

  /// Moves the branch's head to the line `sha` at `minute` and keeps the frame there.
  async commit(job, state, minute, sha) {
    const { tree } = this.api;
    await store.writeBranch(tree, job.projectId, job.runId, { ...state.record, computed_until: minute, checkpoint_hash: sha, status: RUNNING });
    state.head = minute;
    const text = decoder.decode(state.simulation.frame(job.runId, job.branchId, minute, undefined));
    this.headFrame = { key: job.key, head: sha, minute, text };
  }
}
