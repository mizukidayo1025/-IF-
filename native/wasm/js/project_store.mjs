// The browser's Project, Run and Branch records, and one file that carries a Project (Issue #265).
//
// Records. The browser keeps a Project in a folder of its store, beside its Runs' stored files
// (js/run_store.mjs). The folder holds what the screen's routes `GET /api/v1/projects`,
// `POST /api/v1/projects`, `GET {project}`, `GET {project}/runs`, `GET {project}/runs/{run}` and
// `GET {project}/runs/{run}/evidence` answer, in the JSON shapes of the Python server
// (src/sheltersim/storage/projects.py, src/sheltersim/web/projects.py):
//
//   <project>/project.json                         {schema_version, project_id, archived}
//   <project>/scenarios/<scenario>/v<n>.json       {schema_version, scenario_id, scenario_version,
//                                                   scenario_hash, approved}
//   <project>/scenarios/<scenario>/v<n>.scenario.json   the Scenario's canonical JSON, as Python
//                                                   wrote it; scenario_hash is its sha256
//   <project>/runs/<run>/run.json                  {schema_version, project_id, scenario_id,
//                                                   scenario_version, scenario_hash, run_id, seed,
//                                                   model_versions, status, archived}
//   <project>/runs/<run>/evidence.json             evidence_context(scenario), as Python wrote it
//   <project>/runs/<run>/branches/<branch>/branch.json   {schema_version, run_id, branch_id,
//                                                   parent_branch_id, fork_minute, computed_until,
//                                                   checkpoint_hash, status}
//   <project>/runs/<run>/branches/<branch>/journal.jsonl, data.pack   the branch's stored Run
//   <project>/drafts/<preview_id>.json            an intervention's preview (js/interventions.mjs,
//                                                   #306), not carried by a bundle
//
// Records are JSON with sorted keys and a newline. The Scenario and the evidence are kept as the
// bytes Python wrote (route A: Python builds the world and exports it), so their numbers are never
// read and written again by JS (1.0 would become 1). Where the browser has no Python meaning: a
// branch's `checkpoint_hash` is the sha256 of its head's journal line (the last "end" line of its
// stored Run, which holds the minute and the digests the Run reached); the Run's status is kept in
// run.json (Python keeps it in status.json); `storage` of the Project list counts the files of the
// store and takes its limit from the caller (the page: `navigator.storage.estimate().quota`); a
// Run answers no `last_fork`. docs/project-persistence.md lists these.
//
// A store is a tree: `memoryTree()` (Node, tests) or `opfsTree(directory)` (a folder of the origin
// private file system). A tree reads, writes, lists and removes files by path (a list of names),
// and gives a folder as run_store.mjs's backend (`folder`).
//
// Bundle. One file carries a Project: its records and each branch's stored files as they are,
// whatever pieces their journal holds. Its form (BUNDLE_FORMAT, BUNDLE_VERSION):
//   16 bytes   magic: 0x89 "SHELTERSIM-PRJ" 0x0a
//   4 bytes    the bundle's version, big-endian (1)
//   4 bytes    the manifest's length in bytes, big-endian
//   32 bytes   the manifest's sha256
//   manifest   JSON: {format, version, project_id, source, entries: [{path, length, sha256}]}
//   entries    each entry's bytes in the manifest's order, nothing after the last
// An entry's path is relative to the Project's folder. Reading checks the magic, the version, the
// manifest's and every entry's sha256, the records, and then each branch's stored Run read back
// to its digests (run_store.mjs's readAndVerify) and to its record's head.

import { opfsBackend, readAndVerify, runAndStore } from "./run_store.mjs";

export const BUNDLE_FORMAT = "sheltersim-project-bundle";
export const BUNDLE_VERSION = 1;
export const BUNDLE_EXTENSION = ".shelterproj";
const encoder = new TextEncoder();
const decoder = new TextDecoder("utf-8", { fatal: true });
const MAGIC = Uint8Array.of(0x89, ...encoder.encode("SHELTERSIM-PRJ"), 0x0a);
const HEADER = MAGIC.length + 4 + 4 + 32;
const MAX_MANIFEST = 16 * 2 ** 20;
const MAX_ENTRIES = 100_000;
// Python's identifiers (`ProjectStore._path`).
const ID = /^[A-Za-z0-9][A-Za-z0-9._:-]{0,63}$/;
// The names of a branch's stored files: run_store.mjs's and any it adds later.
const RUN_FILE = /^[A-Za-z0-9][A-Za-z0-9._-]{0,63}$/;
const STATUSES = new Set(["queued", "running", "paused", "completed", "cancelled", "failed", "interrupted"]);
const RESERVED = "runtime-accounts";

/// An error of the records or of a bundle: `message` is Python's StorageError text where the
/// Python server has one, `code` its code (or null).
export class ProjectStoreError extends Error {
  constructor(message, code = null) {
    super(message);
    this.name = "ProjectStoreError";
    this.code = code;
  }
}

const fail = (message, code = null) => {
  throw new ProjectStoreError(message, code);
};

function checkId(value) {
  if (typeof value !== "string" || !ID.test(value)) fail("invalid artifact identifier");
  return value;
}

/// JSON with sorted keys (records hold strings, integers, booleans, null and objects of them).
function sortedJson(value) {
  if (Array.isArray(value)) return `[${value.map(sortedJson).join(",")}]`;
  if (value !== null && typeof value === "object") {
    return `{${Object.keys(value)
      .sort()
      .map((key) => `${JSON.stringify(key)}:${sortedJson(value[key])}`)
      .join(",")}}`;
  }
  return JSON.stringify(value);
}

const recordBytes = (value) => encoder.encode(`${sortedJson(value)}\n`);

export async function sha256Hex(bytes) {
  const digest = new Uint8Array(await crypto.subtle.digest("SHA-256", bytes));
  return Array.from(digest, (byte) => byte.toString(16).padStart(2, "0")).join("");
}

const equalBytes = (a, b) => a.length === b.length && a.every((byte, index) => byte === b[index]);

// ---- Trees -----------------------------------------------------------------------------------

/// A store in memory: `files` maps "a/b/c" to its bytes.
export function memoryTree(files = new Map()) {
  const key = (path) => path.join("/");
  const under = (path) => (path.length === 0 ? "" : `${key(path)}/`);
  return {
    files,
    async read(path) {
      const file = files.get(key(path));
      return file ? file.bytes.slice(0, file.length) : null;
    },
    async write(path, bytes) {
      files.set(key(path), { bytes: Uint8Array.from(bytes), length: bytes.length });
    },
    async list(path) {
      const prefix = under(path);
      const names = new Map();
      for (const name of files.keys()) {
        if (!name.startsWith(prefix)) continue;
        const rest = name.slice(prefix.length).split("/");
        names.set(rest[0], rest.length > 1 ? "directory" : "file");
      }
      return [...names].map(([name, kind]) => ({ name, kind })).sort((a, b) => (a.name < b.name ? -1 : a.name > b.name ? 1 : 0));
    },
    async remove(path) {
      const prefix = under(path);
      for (const name of [...files.keys()]) {
        if (name === key(path) || name.startsWith(prefix)) files.delete(name);
      }
    },
    async usage() {
      let bytes = 0;
      for (const file of files.values()) bytes += file.length;
      return { bytes, files: files.size };
    },
    /// The folder as run_store.mjs's backend. A file grows by doubling, so a Run's appends do not
    /// copy it each time.
    async folder(path) {
      return {
        async open(name, { truncate = false } = {}) {
          const at = key([...path, name]);
          if (truncate || !files.has(at)) files.set(at, { bytes: new Uint8Array(0), length: 0 });
          const file = () => files.get(at);
          return {
            size: () => file().length,
            read(offset, length) {
              const { bytes, length: size } = file();
              return bytes.slice(Math.min(offset, size), Math.min(offset + length, size));
            },
            write(bytes, offset) {
              const current = file();
              const end = offset + bytes.length;
              if (end > current.bytes.length) {
                const grown = new Uint8Array(Math.max(end, current.bytes.length * 2, 4096));
                grown.set(current.bytes.subarray(0, current.length));
                current.bytes = grown;
              }
              current.bytes.set(bytes, offset);
              current.length = Math.max(current.length, end);
              return bytes.length;
            },
            truncate(size) {
              const current = file();
              if (size < current.length) current.bytes.fill(0, size, current.length);
              current.length = Math.min(current.length, size);
            },
            flush() {},
            close() {},
          };
        },
      };
    },
  };
}

const missing = (error) => error?.name === "NotFoundError" || error?.name === "TypeMismatchError";

/// A store in a folder of the origin private file system. Writing a whole file uses a
/// synchronous access handle where there is one (a dedicated Worker) and a writable stream
/// otherwise; `folder` gives run_store.mjs's opfsBackend, which only a dedicated Worker can use.
export function opfsTree(directory) {
  async function folderOf(path, create) {
    let handle = directory;
    for (const name of path) {
      try {
        handle = await handle.getDirectoryHandle(name, { create });
      } catch (error) {
        if (!create && missing(error)) return null;
        throw error;
      }
    }
    return handle;
  }
  async function fileOf(path) {
    const parent = await folderOf(path.slice(0, -1), false);
    if (!parent) return null;
    try {
      return await parent.getFileHandle(path.at(-1));
    } catch (error) {
      if (missing(error)) return null;
      throw error;
    }
  }
  async function walk(handle, visit) {
    for await (const entry of handle.values()) {
      if (entry.kind === "directory") await walk(entry, visit);
      else await visit(entry);
    }
  }
  return {
    directory,
    async read(path) {
      const handle = await fileOf(path);
      return handle ? new Uint8Array(await (await handle.getFile()).arrayBuffer()) : null;
    },
    async write(path, bytes) {
      const parent = await folderOf(path.slice(0, -1), true);
      const handle = await parent.getFileHandle(path.at(-1), { create: true });
      if (typeof handle.createSyncAccessHandle === "function" && typeof WorkerGlobalScope !== "undefined") {
        const access = await handle.createSyncAccessHandle();
        try {
          access.truncate(0);
          let at = 0;
          while (at < bytes.length) at += access.write(bytes.subarray(at), { at });
          access.flush();
        } finally {
          access.close();
        }
      } else {
        const writable = await handle.createWritable();
        await writable.write(bytes);
        await writable.close();
      }
    },
    async list(path) {
      const handle = await folderOf(path, false);
      if (!handle) return [];
      const names = [];
      for await (const entry of handle.values()) names.push({ name: entry.name, kind: entry.kind === "directory" ? "directory" : "file" });
      return names.sort((a, b) => (a.name < b.name ? -1 : a.name > b.name ? 1 : 0));
    },
    async remove(path) {
      if (path.length === 0) {
        for await (const name of directory.keys()) await directory.removeEntry(name, { recursive: true });
        return;
      }
      const parent = await folderOf(path.slice(0, -1), false);
      if (!parent) return;
      try {
        await parent.removeEntry(path.at(-1), { recursive: true });
      } catch (error) {
        if (!missing(error)) throw error;
      }
    },
    async usage() {
      let bytes = 0;
      let files = 0;
      await walk(directory, async (handle) => {
        bytes += (await handle.getFile()).size;
        files += 1;
      });
      return { bytes, files };
    },
    async folder(path) {
      return opfsBackend(await folderOf(path, true));
    },
  };
}

// ---- Records ---------------------------------------------------------------------------------

const projectPath = (project) => [checkId(project)];
const scenarioPath = (project, scenario) => [checkId(project), "scenarios", checkId(scenario)];
const runPath = (project, run) => [checkId(project), "runs", checkId(run)];
const branchPath = (project, run, branch) => [...runPath(project, run), "branches", checkId(branch)];

function parseRecord(bytes) {
  let value;
  try {
    value = JSON.parse(decoder.decode(bytes));
  } catch {
    fail("saved artifact is missing or corrupt");
  }
  if (value === null || typeof value !== "object" || Array.isArray(value) || value.schema_version !== 1) {
    fail("unsupported saved artifact version; migration required");
  }
  return value;
}

async function readRecord(tree, path) {
  const bytes = await tree.read(path);
  if (bytes === null) fail("saved artifact is missing or corrupt");
  return parseRecord(bytes);
}

/// Writes a record that may be written again only with the same bytes (Python's immutable write).
async function writeOnce(tree, path, record) {
  const bytes = recordBytes(record);
  const old = await tree.read(path);
  if (old !== null) {
    if (!equalBytes(old, bytes)) fail("immutable saved artifact already exists");
    return record;
  }
  await tree.write(path, bytes);
  return record;
}

/// `POST /api/v1/projects`: the new Project's record (also when it exists with the same record).
export async function createProject(tree, projectId) {
  checkId(projectId);
  if (projectId === RESERVED) fail("project_id_reserved_for_usage_accounts");
  return writeOnce(tree, [projectId, "project.json"], { schema_version: 1, project_id: projectId, archived: false });
}

/// The Projects' records, by id.
export async function projects(tree) {
  const listed = [];
  for (const { name, kind } of await tree.list([])) {
    if (kind !== "directory" || name === RESERVED || !ID.test(name)) continue;
    const bytes = await tree.read([name, "project.json"]);
    if (bytes !== null) listed.push(parseRecord(bytes));
  }
  return listed;
}

/// Saves a Scenario Python approved: `scenarioBytes` its canonical JSON, whose sha256 must be
/// `scenarioHash`. The record of the new revision (without the Scenario).
export async function saveScenario(tree, projectId, { scenarioBytes, scenarioHash }) {
  await readRecord(tree, [checkId(projectId), "project.json"]);
  const hash = await sha256Hex(scenarioBytes);
  if (hash !== scenarioHash) fail("scenario approval hash does not match content");
  let scenarioId;
  try {
    scenarioId = JSON.parse(decoder.decode(scenarioBytes)).id;
  } catch {
    fail("scenario is not JSON");
  }
  const versions = (await scenarioVersions(tree, projectId, checkId(scenarioId))).map(({ version }) => version);
  const version = Math.max(0, ...versions) + 1;
  const folder = scenarioPath(projectId, scenarioId);
  await tree.write([...folder, `v${version}.scenario.json`], scenarioBytes);
  const record = { schema_version: 1, scenario_id: scenarioId, scenario_version: version, scenario_hash: hash, approved: true };
  await tree.write([...folder, `v${version}.json`], recordBytes(record));
  return record;
}

async function scenarioVersions(tree, projectId, scenarioId) {
  const names = [];
  for (const { name, kind } of await tree.list(scenarioPath(projectId, scenarioId))) {
    const match = kind === "file" && /^v([1-9][0-9]{0,8})\.json$/.exec(name);
    if (match) names.push({ name, version: Number(match[1]) });
  }
  return names;
}

/// The Scenario records of a Project, in Python's order (by scenario id, then file name), each
/// with the Scenario's JSON text in `scenarioText`.
export async function scenarios(tree, projectId) {
  const listed = [];
  for (const { name: scenarioId, kind } of await tree.list([checkId(projectId), "scenarios"])) {
    if (kind !== "directory" || !ID.test(scenarioId)) continue;
    for (const { name, version } of await scenarioVersions(tree, projectId, scenarioId)) {
      const record = await readRecord(tree, [...scenarioPath(projectId, scenarioId), name]);
      const text = await tree.read([...scenarioPath(projectId, scenarioId), `v${version}.scenario.json`]);
      if (text === null) fail("saved artifact is missing or corrupt");
      listed.push({ record, scenarioText: decoder.decode(text) });
    }
  }
  return listed;
}

/// Creates a Run of a saved Scenario revision, queued: `seed` and `modelVersions` as Python's
/// RunReference has them (the Scenario's seed, `model_signatures(scenario.model_configs)`), and
/// `evidenceBytes` the JSON of `evidence_context(scenario)`. An existing Run of the same Scenario
/// revision is returned as it is.
export async function createRun(tree, projectId, { runId, scenarioId, scenarioVersion, seed, modelVersions, evidenceBytes }) {
  await readRecord(tree, [checkId(projectId), "project.json"]);
  if (!Number.isSafeInteger(scenarioVersion) || scenarioVersion < 1) fail("invalid scenario revision");
  const scenario = await readRecord(tree, [...scenarioPath(projectId, scenarioId), `v${scenarioVersion}.json`]);
  if (!Number.isSafeInteger(seed) || seed < 0) fail("invalid seed");
  if (modelVersions === null || typeof modelVersions !== "object" || Object.values(modelVersions).some((value) => typeof value !== "string")) {
    fail("invalid model versions");
  }
  const path = [...runPath(projectId, runId), "run.json"];
  const old = await tree.read(path);
  if (old !== null) {
    const run = parseRecord(old);
    if (run.scenario_id !== scenarioId || run.scenario_version !== scenarioVersion) fail("run_id_conflict");
    return run;
  }
  parseRecord(evidenceBytes);
  await tree.write([...runPath(projectId, runId), "evidence.json"], evidenceBytes);
  const run = {
    schema_version: 1,
    project_id: projectId,
    scenario_id: scenarioId,
    scenario_version: scenarioVersion,
    scenario_hash: scenario.scenario_hash,
    run_id: runId,
    seed,
    model_versions: modelVersions,
    status: "queued",
    archived: false,
  };
  await tree.write(path, recordBytes(run));
  return run;
}

/// Sets a Run's status (Python's `set_status`).
export async function setRunStatus(tree, projectId, runId, status) {
  if (!STATUSES.has(status)) fail("unsupported run status");
  const path = [...runPath(projectId, runId), "run.json"];
  const run = await readRecord(tree, path);
  await tree.write(path, recordBytes({ ...run, status }));
  return { ...run, status };
}

function checkBranch(branch, runId) {
  const { branch_id, parent_branch_id, fork_minute, computed_until, checkpoint_hash, status } = branch;
  checkId(branch_id);
  if (parent_branch_id !== null) checkId(parent_branch_id);
  if (!Number.isSafeInteger(fork_minute) || fork_minute < 0) fail("invalid fork minute");
  if (!Number.isSafeInteger(computed_until) || computed_until < fork_minute) fail("invalid computed minute");
  if (checkpoint_hash !== null && !/^[a-f0-9]{64}$/.test(checkpoint_hash)) fail("invalid checkpoint hash");
  if (!STATUSES.has(status)) fail("unsupported run status");
  if (branch.run_id !== runId) fail("branch record of another run");
}

/// Writes a branch's record: `{branch_id, parent_branch_id, fork_minute, computed_until,
/// checkpoint_hash, status}`. A child's parent must have a record; a branch's lineage
/// (parent and fork minute) never changes.
export async function writeBranch(tree, projectId, runId, branch) {
  const record = {
    schema_version: 1,
    run_id: runId,
    branch_id: branch.branch_id,
    parent_branch_id: branch.parent_branch_id ?? null,
    fork_minute: branch.fork_minute ?? 0,
    computed_until: branch.computed_until ?? branch.fork_minute ?? 0,
    checkpoint_hash: branch.checkpoint_hash ?? null,
    status: branch.status,
  };
  checkBranch(record, runId);
  await readRecord(tree, [...runPath(projectId, runId), "run.json"]);
  if (record.parent_branch_id !== null) await readRecord(tree, [...branchPath(projectId, runId, record.parent_branch_id), "branch.json"]);
  const path = [...branchPath(projectId, runId, record.branch_id), "branch.json"];
  const old = await tree.read(path);
  if (old !== null) {
    const prior = parseRecord(old);
    if (prior.parent_branch_id !== record.parent_branch_id || prior.fork_minute !== record.fork_minute) fail("branch lineage is immutable");
  }
  await tree.write(path, recordBytes(record));
  return record;
}

/// The branches' records of a Run, by branch id.
export async function branches(tree, projectId, runId) {
  const listed = [];
  for (const { name, kind } of await tree.list([...runPath(projectId, runId), "branches"])) {
    if (kind !== "directory" || !ID.test(name)) continue;
    const bytes = await tree.read([...branchPath(projectId, runId, name), "branch.json"]);
    if (bytes !== null) listed.push(parseRecord(bytes));
  }
  return listed;
}

/// A branch's record (Python's `branch_reference`), with Python's errors for a bad identifier or a
/// missing record (#266).
export async function branchRecord(tree, projectId, runId, branchId) {
  return readRecord(tree, [...branchPath(projectId, runId, branchId), "branch.json"]);
}

/// The Scenario of a Run, as the bytes Python wrote (#266).
export async function runScenarioBytes(tree, projectId, runId) {
  const run = await readRecord(tree, [...runPath(projectId, runId), "run.json"]);
  const bytes = await tree.read([...scenarioPath(projectId, run.scenario_id), `v${run.scenario_version}.scenario.json`]);
  if (bytes === null) fail("saved artifact is missing or corrupt");
  return bytes;
}

/// The folder of a branch's stored Run, as run_store.mjs's backend.
export async function branchBackend(tree, projectId, runId, branchId) {
  return tree.folder(branchPath(projectId, runId, branchId));
}

/// The head of a stored Run's journal text: the last "end" line before the journal ends or a line
/// does not match its sha256, as `{minute, checkpointHash, duration}` (the line's sha256, and the
/// minute the clock is complete at, of the "run" line), or null.
export function journalHead(wasm, text) {
  let head = null;
  let duration = null;
  for (const line of text.split("\n")) {
    if (line === "") continue;
    const tab = line.lastIndexOf("\t");
    if (tab < 0) break;
    const json = line.slice(0, tab);
    const sha = line.slice(tab + 1);
    if (wasm.sha256Hex(encoder.encode(json)) !== sha) break;
    const entry = JSON.parse(json);
    if (entry.kind === "run") duration = entry.duration_minutes;
    if (entry.kind === "end") head = { minute: entry.minute, checkpointHash: sha, duration };
  }
  return head;
}

async function headOf(wasm, backend) {
  const journal = await backend.open("journal.jsonl");
  try {
    return journalHead(wasm, new TextDecoder().decode(journal.read(0, journal.size())));
  } finally {
    journal.close();
  }
}

/// Computes a branch in WASM and stores it as it goes (run_store.mjs's runAndStore with `run`'s
/// options: exportBytes, world, ticks, progress), then writes its record: computed to the minute
/// the Run reached, its head the journal's end line, completed when the clock is.
export async function computeBranch({ wasm, tree, projectId, runId, branchId, parentBranchId = null, forkMinute = 0, run }) {
  await writeBranch(tree, projectId, runId, { branch_id: branchId, parent_branch_id: parentBranchId, fork_minute: forkMinute, status: "running" });
  const backend = await branchBackend(tree, projectId, runId, branchId);
  let ran;
  try {
    ran = await runAndStore({ wasm, backend, ...run });
  } catch (error) {
    await writeBranch(tree, projectId, runId, { branch_id: branchId, parent_branch_id: parentBranchId, fork_minute: forkMinute, status: "failed" });
    throw error;
  }
  const head = await headOf(wasm, backend);
  const record = await writeBranch(tree, projectId, runId, {
    branch_id: branchId,
    parent_branch_id: parentBranchId,
    fork_minute: forkMinute,
    computed_until: head.minute,
    checkpoint_hash: head.checkpointHash,
    status: head.minute >= head.duration ? "completed" : "paused",
  });
  return { ran, record };
}

/// Reads a branch's stored Run back and checks it against its digests (readAndVerify) and its
/// record: a branch with a head must be complete, verified and end at its record's head.
export async function verifyBranch(wasm, tree, projectId, runId, record) {
  const journal = await tree.read([...branchPath(projectId, runId, record.branch_id), "journal.jsonl"]);
  if (journal === null || journal.length === 0) {
    if (record.checkpoint_hash !== null) fail(`branch ${record.branch_id}: its stored Run is missing`, "bundle_run_mismatch");
    return { branch_id: record.branch_id, stored: false };
  }
  const backend = await branchBackend(tree, projectId, runId, record.branch_id);
  let back;
  try {
    back = await readAndVerify({ wasm, backend });
  } catch (error) {
    fail(`branch ${record.branch_id}: its stored Run does not read back: ${error.message}`, "bundle_run_mismatch");
  }
  if (back.complete && !back.verified) fail(`branch ${record.branch_id}: its stored Run does not reach its digests (${back.mismatches.join(", ")})`, "bundle_run_mismatch");
  if (record.checkpoint_hash !== null) {
    const head = await headOf(wasm, backend);
    if (!back.verified || head === null || head.checkpointHash !== record.checkpoint_hash || head.minute !== record.computed_until) {
      fail(`branch ${record.branch_id}: its stored Run does not end at its record's head`, "bundle_run_mismatch");
    }
  }
  return { branch_id: record.branch_id, stored: true, ...back };
}

// ---- Responses (#266) ------------------------------------------------------------------------

/// `GET /api/v1/projects`: `limitBytes` is what the store may hold (the page: the origin's quota).
export async function projectsResponse(tree, { limitBytes = null } = {}) {
  const usage = await tree.usage();
  return JSON.stringify({ projects: await projects(tree), storage: { bytes: usage.bytes, files: usage.files, limit_bytes: limitBytes } });
}

async function runDocument(tree, projectId, runId) {
  const run = await readRecord(tree, [...runPath(projectId, runId), "run.json"]);
  return { ...run, branches: await branches(tree, projectId, runId) };
}

async function runIds(tree, projectId) {
  const ids = [];
  for (const { name, kind } of await tree.list([checkId(projectId), "runs"])) {
    if (kind === "directory" && ID.test(name) && (await tree.read([...runPath(projectId, name), "run.json"])) !== null) ids.push(name);
  }
  return ids;
}

/// `GET /api/v1/projects/{project}`: the Project with its Scenarios (each with its Scenario's JSON
/// text as Python wrote it) and its Runs.
export async function projectResponse(tree, projectId) {
  const project = await readRecord(tree, [checkId(projectId), "project.json"]);
  const scenarioTexts = (await scenarios(tree, projectId)).map(({ record, scenarioText }) => `${JSON.stringify(record).slice(0, -1)},"scenario":${scenarioText.trimEnd()}}`);
  const runs = [];
  for (const runId of await runIds(tree, projectId)) runs.push(JSON.stringify(await runDocument(tree, projectId, runId)));
  return `${JSON.stringify(project).slice(0, -1)},"scenarios":[${scenarioTexts.join(",")}],"runs":[${runs.join(",")}]}`;
}

/// `GET /api/v1/projects/{project}/runs`.
export async function runsResponse(tree, projectId) {
  await readRecord(tree, [checkId(projectId), "project.json"]);
  const runs = [];
  for (const runId of await runIds(tree, projectId)) runs.push(await runDocument(tree, projectId, runId));
  return JSON.stringify({ runs });
}

/// `GET /api/v1/projects/{project}/runs/{run}`.
export async function runResponse(tree, projectId, runId) {
  return JSON.stringify(await runDocument(tree, projectId, runId));
}

/// `GET /api/v1/projects/{project}/runs/{run}/evidence`: the JSON Python wrote.
export async function evidenceResponse(tree, projectId, runId) {
  await readRecord(tree, [...runPath(projectId, runId), "run.json"]);
  const bytes = await tree.read([...runPath(projectId, runId), "evidence.json"]);
  if (bytes === null) fail("saved artifact is missing or corrupt");
  return decoder.decode(bytes).trimEnd();
}

/// Removes a Project and everything in its folder.
export async function removeProject(tree, projectId) {
  await tree.remove(projectPath(projectId));
}

// ---- Bundle ----------------------------------------------------------------------------------

/// The kind of a path in a Project's folder, or null for one a bundle may not hold.
function entryKind(path) {
  const parts = path.split("/");
  const [first, second, third, fourth, fifth] = parts;
  if (parts.length === 1 && first === "project.json") return { kind: "project" };
  if (parts.length === 3 && first === "scenarios" && ID.test(second)) {
    const match = /^v([1-9][0-9]{0,8})(\.scenario)?\.json$/.exec(third);
    if (match) return { kind: match[2] ? "scenario_text" : "scenario", scenarioId: second, version: Number(match[1]) };
  }
  if (parts.length === 3 && first === "runs" && ID.test(second) && (third === "run.json" || third === "evidence.json")) {
    return { kind: third === "run.json" ? "run" : "evidence", runId: second };
  }
  if (parts.length === 5 && first === "runs" && ID.test(second) && third === "branches" && ID.test(fourth) && RUN_FILE.test(fifth)) {
    return { kind: fifth === "branch.json" ? "branch" : "run_file", runId: second, branchId: fourth };
  }
  return null;
}

/// Packs entries (`[path, bytes]`, paths relative to the Project's folder) into a bundle: a Blob.
export async function packBundle({ projectId, source = null, entries }) {
  const listed = [];
  for (const [path, bytes] of entries) listed.push({ path, length: bytes.length, sha256: await sha256Hex(bytes) });
  const manifest = encoder.encode(JSON.stringify({ format: BUNDLE_FORMAT, version: BUNDLE_VERSION, project_id: projectId, source, entries: listed }));
  const header = new Uint8Array(HEADER);
  header.set(MAGIC);
  const view = new DataView(header.buffer);
  view.setUint32(MAGIC.length, BUNDLE_VERSION);
  view.setUint32(MAGIC.length + 4, manifest.length);
  header.set(new Uint8Array(await crypto.subtle.digest("SHA-256", manifest)), MAGIC.length + 8);
  return new Blob([header, manifest, ...entries.map(([, bytes]) => bytes)], { type: "application/octet-stream" });
}

/// The bundle of a Project: every file of its folder, in path order. `source` is what made it
/// (any JSON), kept in the manifest.
export async function exportProject({ tree, projectId, source = null }) {
  await readRecord(tree, [checkId(projectId), "project.json"]);
  const entries = [];
  async function visit(path) {
    for (const { name, kind } of await tree.list([projectId, ...path])) {
      // An intervention's preview drafts (#306) are kept beside the records, not carried.
      if (path.length === 0 && name === "drafts" && kind === "directory") continue;
      if (kind === "directory") {
        await visit([...path, name]);
        continue;
      }
      const relative = [...path, name].join("/");
      if (entryKind(relative) === null) fail(`${relative}: a file a bundle does not hold`, "bundle_invalid");
      entries.push([relative, await tree.read([projectId, ...path, name])]);
    }
  }
  await visit([]);
  return packBundle({ projectId, source, entries });
}

async function sliceOf(bundle, start, end) {
  if (bundle instanceof Uint8Array) return bundle.subarray(start, end);
  return new Uint8Array(await bundle.slice(start, end).arrayBuffer());
}

/// Reads a bundle (a Blob or bytes) and checks its form and every entry's sha256: its manifest and
/// its entries' bytes by path. Nothing is written.
export async function readBundle(bundle) {
  const size = bundle instanceof Uint8Array ? bundle.length : bundle.size;
  const invalid = (message) => fail(`not a readable project bundle: ${message}`, "bundle_invalid");
  if (size < HEADER) invalid("the file is shorter than its header");
  const header = await sliceOf(bundle, 0, HEADER);
  if (!equalBytes(header.subarray(0, MAGIC.length), MAGIC)) invalid("the file does not start with the bundle's magic");
  const view = new DataView(header.buffer, header.byteOffset, header.byteLength);
  const version = view.getUint32(MAGIC.length);
  if (version !== BUNDLE_VERSION) fail(`a project bundle of version ${version}: this page reads version ${BUNDLE_VERSION}`, "bundle_version_unsupported");
  const length = view.getUint32(MAGIC.length + 4);
  if (length > MAX_MANIFEST || HEADER + length > size) invalid("the manifest's length is past the file's end");
  const manifestBytes = await sliceOf(bundle, HEADER, HEADER + length);
  const wanted = Array.from(header.subarray(MAGIC.length + 8), (byte) => byte.toString(16).padStart(2, "0")).join("");
  if ((await sha256Hex(manifestBytes)) !== wanted) invalid("the manifest is not the one written (sha256)");
  let manifest;
  try {
    manifest = JSON.parse(decoder.decode(manifestBytes));
  } catch {
    invalid("the manifest is not JSON");
  }
  if (manifest?.format !== BUNDLE_FORMAT || manifest.version !== version) invalid("the manifest is not of this format and version");
  if (typeof manifest.project_id !== "string" || !ID.test(manifest.project_id)) invalid("the Project's id is not an identifier");
  if (!Array.isArray(manifest.entries) || manifest.entries.length > MAX_ENTRIES) invalid("the manifest has no list of entries");
  let at = HEADER + length;
  const entries = new Map();
  for (const entry of manifest.entries) {
    const { path, length: entryLength, sha256 } = entry ?? {};
    if (typeof path !== "string" || entryKind(path) === null) invalid(`an entry's path is not one a bundle holds: ${JSON.stringify(path)}`);
    if (entries.has(path)) invalid(`the entry ${path} is listed twice`);
    if (!Number.isSafeInteger(entryLength) || entryLength < 0 || at + entryLength > size) invalid(`the entry ${path} is past the file's end`);
    if (typeof sha256 !== "string" || !/^[a-f0-9]{64}$/.test(sha256)) invalid(`the entry ${path} has no sha256`);
    const bytes = await sliceOf(bundle, at, at + entryLength);
    if ((await sha256Hex(bytes)) !== sha256) fail(`${path}: the bytes are not the ones the bundle lists (sha256)`, "bundle_digest_mismatch");
    entries.set(path, bytes);
    at += entryLength;
  }
  if (at !== size) invalid("bytes follow the last entry");
  return { manifest, entries };
}

/// Checks the records of a bundle against each other: what Python's store would refuse to read.
async function checkRecords(projectId, entries) {
  const invalid = (message) => fail(`the bundle's records do not hold together: ${message}`, "bundle_invalid");
  const record = (path) => {
    try {
      return parseRecord(entries.get(path));
    } catch (error) {
      invalid(`${path}: ${error.message}`);
    }
  };
  if (!entries.has("project.json")) invalid("there is no project.json");
  const project = record("project.json");
  if (project.project_id !== projectId || typeof project.archived !== "boolean") invalid("project.json is not this Project's");
  const scenarioHashes = new Map();
  const runs = new Map();
  for (const path of entries.keys()) {
    const kind = entryKind(path);
    if (kind.kind === "scenario") {
      const scenario = record(path);
      const text = entries.get(`scenarios/${kind.scenarioId}/v${kind.version}.scenario.json`);
      if (!text) invalid(`${path} has no Scenario`);
      if (scenario.scenario_id !== kind.scenarioId || scenario.scenario_version !== kind.version || scenario.approved !== true) invalid(`${path} is not its folder's`);
      if ((await sha256Hex(text)) !== scenario.scenario_hash) invalid(`${path}: the Scenario's sha256 is not its hash`);
      scenarioHashes.set(`${kind.scenarioId}/${kind.version}`, scenario.scenario_hash);
    } else if (kind.kind === "scenario_text") {
      if (!entries.has(`scenarios/${kind.scenarioId}/v${kind.version}.json`)) invalid(`${path} has no record`);
    } else if (kind.kind === "run") {
      runs.set(kind.runId, { run: record(path), branches: [] });
    }
  }
  for (const [runId, { run }] of runs) {
    if (run.project_id !== projectId || run.run_id !== runId) invalid(`run ${runId} is not its folder's`);
    if (scenarioHashes.get(`${run.scenario_id}/${run.scenario_version}`) !== run.scenario_hash) invalid(`run ${runId}'s Scenario is not in the bundle`);
    if (!STATUSES.has(run.status) || typeof run.archived !== "boolean" || !Number.isSafeInteger(run.seed)) invalid(`run ${runId}'s record is not a Run's`);
    if (!entries.has(`runs/${runId}/evidence.json`)) invalid(`run ${runId} has no evidence`);
    record(`runs/${runId}/evidence.json`);
  }
  for (const path of entries.keys()) {
    const kind = entryKind(path);
    if (kind.kind !== "branch" && kind.kind !== "run_file") continue;
    const owner = runs.get(kind.runId);
    if (!owner) invalid(`${path} belongs to no Run`);
    if (kind.kind === "run_file") {
      if (!entries.has(`runs/${kind.runId}/branches/${kind.branchId}/branch.json`)) invalid(`${path} belongs to no branch`);
      continue;
    }
    const branch = record(path);
    try {
      checkBranch(branch, kind.runId);
    } catch (error) {
      invalid(`${path}: ${error.message}`);
    }
    if (branch.branch_id !== kind.branchId) invalid(`${path} is not its folder's`);
    owner.branches.push(branch);
  }
  for (const [runId, { branches: listed }] of runs) {
    const ids = new Set(listed.map((branch) => branch.branch_id));
    for (const branch of listed) {
      if (branch.parent_branch_id !== null && !ids.has(branch.parent_branch_id)) invalid(`branch ${branch.branch_id} of run ${runId} has no parent`);
    }
  }
  return runs;
}

/// Reads a bundle into `tree`: checks its form, every entry's sha256 and its records, reads each
/// branch's stored Run back to its digests and its record's head, and only then writes the
/// Project (its project.json last, so a Project read half is never listed). The stored Runs are
/// read back from the bundle's entries in memory before `tree` is touched, so a bundle that does
/// not check out leaves a Project already there as it was (#273). A Project with the same id is
/// refused unless `replace`, which removes it once the bundle has checked out; if writing then
/// fails (the store is full), the old Project is gone and nothing of the new one is left.
/// Returns what each branch read back to.
export async function importProject({ wasm, tree, bundle, replace = false }) {
  const { manifest, entries } = await readBundle(bundle);
  const projectId = manifest.project_id;
  const runs = await checkRecords(projectId, entries);
  if ((await tree.read([projectId, "project.json"])) !== null && !replace) fail(`project ${projectId} exists`, "project_exists");
  const staged = memoryTree();
  for (const [path, bytes] of entries) {
    if (entryKind(path).kind === "run_file") await staged.write([projectId, ...path.split("/")], bytes);
  }
  const verified = [];
  for (const [runId, { branches: listed }] of runs) {
    for (const branch of listed) verified.push({ run_id: runId, ...(await verifyBranch(wasm, staged, projectId, runId, branch)) });
  }
  staged.files.clear();
  await tree.remove([projectId]);
  try {
    for (const [path, bytes] of entries) {
      if (path !== "project.json") await tree.write([projectId, ...path.split("/")], bytes);
    }
    await tree.write([projectId, "project.json"], entries.get("project.json"));
    return { projectId, source: manifest.source ?? null, branches: verified };
  } catch (error) {
    await tree.remove([projectId]);
    throw error;
  }
}
