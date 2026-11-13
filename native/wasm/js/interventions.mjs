// The browser's answers to an intervention (Issue #306, under #292): the routes the screen's
// 「ここから別の『もしも』を試す」 calls, as the Python server answers them without an API key.
//
//   POST runs/{run}/branches/{branch}/intervention-preview   {minute, text}
//       src/sheltersim/web/workspace.py `intervention`: the branch at the minute, the summary of its
//       world (Simulation.scenarioSummary, #302), the offline compiler's result
//       (compileInterventionOffline, #303), and for a compiled plan its EffectProgram
//       (effectProgramDump, #304), the program's checks on the world (validateEffectProgram),
//       the approval's hash and a draft (`preview_id`).
//   POST runs/{run}/branches/{branch}/intervention-apply     {preview_id, approved_program_hash,
//       approved, child_id}
//       the draft's checks, then ProjectAPI.fork as the server's process worker answers it:
//       {"status": "running", "branch_id": child, "minute": m}. The child is made at once (the
//       branch at the minute, the program applied, the child's stored Run written, run_store.mjs
//       format v4) and the outcome is the Run's `last_fork` (GET runs/{run}), as the server's is
//       once its worker ends: {"status": "created", "branch_id", "minute", "state_hash"} or
//       {"status": "failed", "error", "code"}.
//
// Where the browser differs from the server (docs/project-persistence.md lists these):
// - A typed `program` (the screen never sends one) is refused, 409 `browser_unsupported`, after the
//   checks of the branch and the minute: pydantic's texts for a program it refuses are not made here.
// - `preview_id` is random, as the server's (uuid4); a draft is kept in the Project's folder
//   (`drafts/<preview_id>.json`) and is not part of a Project's bundle.
// - The fork is not a job: it is done before the answer, so the Run never shows `running` for it,
//   and another branch computing does not count against the server's two workers.
// - `last_fork` is kept by the page's Worker, as the server keeps it in its process: it is gone
//   when the page closes.

import { ApiError } from "./api_answers.mjs";
import { checkFields } from "./compute.mjs";
import * as store from "./project_store.mjs";
import { FORMAT, RunReader, RunWriter, SAVE_EVERY } from "./run_store.mjs";

const encoder = new TextEncoder();
const decoder = new TextDecoder();
const refused = (message) => new store.ProjectStoreError(message);
const UNSUPPORTED = "ブラウザー版では未対応です";
const ID = /^[A-Za-z0-9][A-Za-z0-9._:-]{0,63}$/;
// The fields of a DraftPlan that are an EffectProgram's (`EffectProgram.model_fields`).
const PROGRAM_FIELDS = ["schema_version", "program_id", "narrative", "source_text", "simulation_start_minute", "effects", "assumptions", "support_mode", "compiler_provenance"];

/// JSON text already written, put into an answer as it is.
class Raw {
  constructor(text) {
    this.text = text;
  }
}

/// JSON with sorted keys and no spaces (Python's canonical JSON); a float must come as `Raw`.
function sortedJson(value) {
  if (value instanceof Raw) return value.text;
  if (typeof value === "number") {
    if (!Number.isSafeInteger(value)) throw new Error(`not an integer: ${value}`);
    return String(value);
  }
  if (Array.isArray(value)) return `[${value.map(sortedJson).join(",")}]`;
  if (value !== null && typeof value === "object") {
    const keys = Object.keys(value).sort((a, b) => (a < b ? -1 : a > b ? 1 : 0));
    return `{${keys.map((key) => `${JSON.stringify(key)}:${sortedJson(value[key])}`).join(",")}}`;
  }
  return JSON.stringify(value);
}

const draftPath = (projectId, previewId) => [projectId, "drafts", `${previewId}.json`];

/// A uuid4's hex, as the server's `preview_id`.
function previewId() {
  return crypto.randomUUID().replaceAll("-", "");
}

/// The branch's record, with the server's checks of the minute (`TimelineStore.seek`).
async function branchAt(api, projectId, runId, branchId, minute) {
  const record = await api.branch(projectId, runId, branchId);
  if (!Number.isInteger(minute) || typeof minute !== "number" || minute < record.fork_minute || minute > record.computed_until) {
    throw refused("minute is outside this branch's computed range");
  }
  return record;
}

/// `POST …/intervention-preview`: the answer's JSON text.
export async function interventionPreview(api, projectId, runId, branchId, document) {
  const { wasm, tree } = api;
  checkFields(document, ["minute"], ["text", "program"]);
  if (("text" in document) === ("program" in document)) throw refused("provide exactly one of text or program");
  const minute = document.minute;
  const record = await branchAt(api, projectId, runId, branchId, minute);
  if ("program" in document) throw new ApiError(409, `${UNSUPPORTED}（型つきのEffectProgram）`, "browser_unsupported");
  const simulation = await api.simulationAt(projectId, runId, record, minute);
  const summary = decoder.decode(simulation.scenarioSummary());
  const request = `{"branch_id":${JSON.stringify(branchId)},"scenario_summary":${summary},"selected_minute":${minute},"text":${JSON.stringify(document.text) ?? "null"}}`;
  const compiled = wasm.compileInterventionOffline(encoder.encode(request));
  if (compiled === undefined || compiled === null) throw new ApiError(400, "invalid project request");
  const result = JSON.parse(compiled);
  if (result.status !== "compiled" || result.draft_plan === null) return compiled;
  // The offline plan holds no float (its parameters are counts and flags), so JSON.parse keeps it.
  const program = Object.fromEntries(PROGRAM_FIELDS.map((key) => [key, result.draft_plan[key]]));
  const dump = wasm.effectProgramDump(encoder.encode(JSON.stringify(program)));
  const issues = JSON.parse(simulation.validateEffectProgram(dump));
  const dumpText = decoder.decode(dump);
  const hash = wasm.sha256Hex(dump);
  const canApply = issues.length === 0;
  const content = {
    kind: "intervention",
    project_id: projectId,
    run_id: runId,
    branch_id: branchId,
    minute,
    state_hash: simulation.stateDigest(),
    program: new Raw(dumpText),
    hash,
    can_apply: canApply,
  };
  const contentText = sortedJson(content);
  if (wasm.documentRedacts(encoder.encode(contentText))) throw refused("remove sensitive text before saving or approving a draft");
  const id = previewId();
  // The program's text is kept as written as well: its sha256 is the approval's hash.
  await tree.write(draftPath(projectId, id), encoder.encode(`${sortedJson({ schema_version: 1, draft_id: id, kind: "intervention", content: new Raw(contentText), program_text: dumpText })}\n`));
  const extra = sortedJson({ approved_program_hash: hash, can_apply: canApply, preview_id: id, program: new Raw(dumpText), validation_issues: issues });
  return wasm.mergeJson(encoder.encode(compiled), encoder.encode(extra));
}

/// The draft a preview saved (`ProjectWorkspace._preview`): its content, the program as JSON text.
async function readDraft(tree, projectId, previewIdValue) {
  if (typeof previewIdValue !== "string" || !ID.test(previewIdValue)) throw refused("invalid artifact identifier");
  const bytes = await tree.read(draftPath(projectId, previewIdValue));
  if (bytes === null) throw refused("saved artifact is missing or corrupt");
  const text = decoder.decode(bytes);
  const draft = JSON.parse(text);
  if (draft?.schema_version !== 1 || typeof draft.content !== "object") throw refused("unsupported saved artifact version; migration required");
  const content = draft.content;
  if (content.project_id !== projectId) throw refused("draft belongs to another Project");
  return { ...content, programText: draft.program_text };
}

/// `POST …/intervention-apply`: the answer's JSON text. The child is made before it answers.
export async function interventionApply(api, projectId, runId, branchId, document) {
  const { tree } = api;
  checkFields(document, ["preview_id", "approved_program_hash", "approved", "child_id"]);
  const preview = await readDraft(tree, projectId, document.preview_id);
  if (preview.kind !== "intervention" || preview.project_id !== projectId || preview.run_id !== runId || preview.branch_id !== branchId) {
    throw refused("intervention preview expired or has another scope");
  }
  if (!preview.can_apply || document.approved_program_hash !== preview.hash) throw refused("intervention approval does not match validated preview");
  // ProjectAPI.fork, its checks in its order.
  if (document.approved !== true) throw refused("branch creation needs explicit human approval");
  if (api.computing.jobs.has(`${projectId}/${runId}/${branchId}`)) throw refused("pause background calculation before forking");
  const record = await store.branchRecord(tree, projectId, runId, branchId);
  const minute = preview.minute;
  if (!Number.isInteger(minute) || minute < record.fork_minute || minute > record.computed_until) throw refused("minute is outside this branch's computed range");
  const childId = document.child_id;
  if (typeof childId !== "string" || !ID.test(childId)) throw refused("invalid artifact identifier");
  if ((await tree.read([projectId, "runs", runId, "branches", childId, "branch.json"])) !== null) throw refused("child branch already exists");
  if (api.wasm.sha256Hex(encoder.encode(preview.programText)) !== preview.hash) throw refused("EffectProgram approval hash does not match preview");
  api.forkOutcomes.delete(`${projectId}/${runId}`);
  let outcome;
  try {
    outcome = { status: "created", ...(await fork(api, projectId, runId, record, minute, childId, preview)) };
  } catch (error) {
    if (error instanceof store.ProjectStoreError) {
      outcome = { status: "failed", error: error.message, code: error.code ?? storageCode(error.message) };
    } else if (String(error?.message ?? "").startsWith("EffectProgram could not be scheduled")) {
      // The server's worker answers a ValueError so (worker_process.py).
      outcome = { status: "failed", error: "invalid project request", code: null };
    } else {
      console.error(error);
      outcome = { status: "failed", error: "project operation failed", code: null };
    }
  }
  api.forkOutcomes.set(`${projectId}/${runId}`, outcome);
  return sortedJson({ branch_id: childId, minute, status: "running" });
}

/// StorageError's code: the message's leading word where it is one, else "storage_conflict".
function storageCode(message) {
  const candidate = message.split(":", 1)[0];
  return /^[a-z][a-z0-9_]{1,80}$/.test(candidate) ? candidate : "storage_conflict";
}

/// `TimelineStore.fork`: the parent at the minute, checked against the state the preview saw, the
/// program applied, and the child's stored Run and record. Returns `last_fork`'s fields.
async function fork(api, projectId, runId, parent, minute, childId, preview) {
  const { wasm, tree } = api;
  // The kept worlds go first: WASM's memory does not shrink.
  api.freeLive();
  const parentBackend = await store.branchBackend(tree, projectId, runId, parent.branch_id);
  const reader = await RunReader.open({ wasm, backend: parentBackend });
  let simulation;
  let runLine;
  let exportLine;
  let compressed;
  try {
    runLine = reader.run;
    exportLine = reader.lines.find((line) => line.kind === "export");
    if (!exportLine) throw new Error("the parent's stored Run has no exported world");
    await reader.raw(exportLine);
    compressed = reader.data.read(exportLine.offset, exportLine.length);
    simulation = (await reader.seek(minute)).simulation;
  } finally {
    reader.close();
  }
  try {
    const origin = simulation.stateDigest();
    if (origin !== preview.state_hash) throw refused("source state changed; preview the intervention again");
    const receipt = simulation.applyEffectProgram(encoder.encode(preview.programText));
    const backend = await store.branchBackend(tree, projectId, runId, childId);
    const writer = await RunWriter.create(wasm, backend);
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
        fork_minute: minute,
        parent_branch_id: parent.branch_id,
      });
      writer.line({
        kind: "fork",
        parent_branch_id: parent.branch_id,
        minute,
        fork_origin: { branch_id: parent.branch_id, minute, state_hash: origin, checkpoint_hash: parent.checkpoint_hash },
        program: preview.programText,
        approved_program_hash: preview.hash,
        receipt,
      });
      const offset = writer.data.size();
      writer.data.write(compressed, offset);
      writer.data.flush();
      writer.line({ kind: "export", offset, length: compressed.length, raw_length: exportLine.raw_length, sha256: exportLine.sha256 });
      head = await writer.save(simulation, { seal: true });
    } finally {
      writer.close();
    }
    await store.writeBranch(tree, projectId, runId, { branch_id: childId, parent_branch_id: parent.branch_id, fork_minute: minute, computed_until: minute, checkpoint_hash: head, status: "paused" });
    return { branch_id: childId, minute, state_hash: simulation.stateDigest() };
  } finally {
    simulation.free();
  }
}

/// The fork's receipt and origin a child's stored Run holds (its "fork" line), or null for a
/// branch not made by a fork.
export async function forkLine(api, projectId, runId, branchId) {
  const backend = await store.branchBackend(api.tree, projectId, runId, branchId);
  const reader = await RunReader.open({ wasm: api.wasm, backend });
  try {
    return reader.lines.find((line) => line.kind === "fork") ?? null;
  } finally {
    reader.close();
  }
}
