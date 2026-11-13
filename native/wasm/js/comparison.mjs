// The browser's answers to comparing branches (Issue #307, under #292), as the Python server
// answers them:
//
//   GET runs/{run}/compare?branch_ids=a,b&minute=m      src/sheltersim/web/workspace.py `comparison`
//   GET runs/{run}/replay-frames?branch_ids=a,b&start=&end=   web/replay.py `replay_frames` of two
//       branches: each minute's comparison
//
// A comparison holds each branch's frame at the minute (the core's, as text), the common computed
// range, each branch's fork receipt (`fork_origin`: the "fork" line of a child's stored Run, #306;
// null for a branch not made by a fork), whether they share their origin, and each branch's KPI
// differences from the first, which the core computes (`kpiDifferences`) so that an int stays an
// int and a float is written as Python writes it.
//
// Where the browser differs: a receipt's `checkpoint_hash` is the parent's head when the child was
// made (its journal line's sha256), not a Checkpoint's hash.

import { forkLine } from "./interventions.mjs";
import * as store from "./project_store.mjs";

const encoder = new TextEncoder();
const refused = (message) => new store.ProjectStoreError(message);

/// `ProjectWorkspace.comparison_context`: the common range, the receipts and whether the branches
/// share their origin, with the server's checks.
export async function comparisonContext(api, projectId, runId, branchIds, minute) {
  if (branchIds.length < 2 || branchIds.length > 8 || new Set(branchIds).size !== branchIds.length) {
    throw refused("comparison requires 2–8 distinct project branches");
  }
  const records = new Map((await store.branches(api.tree, projectId, runId)).map((record) => [record.branch_id, record]));
  if (branchIds.some((id) => !records.has(id))) throw refused("comparison branch is missing from this Run");
  const lower = Math.max(...branchIds.map((id) => records.get(id).fork_minute));
  const upper = Math.min(...branchIds.map((id) => records.get(id).computed_until));
  if (!Number.isInteger(minute) || minute < lower || minute > upper) throw refused("comparison minute is outside the common computed range");
  const receipts = [];
  for (const id of branchIds) receipts.push((await forkLine(api, projectId, runId, id))?.fork_origin ?? null);
  const origins = receipts.filter((receipt) => receipt !== null);
  const same = (a, b) => a !== null && b !== null && ["branch_id", "minute", "state_hash", "checkpoint_hash"].every((key) => a[key] === b[key]) && Object.keys(a).length === Object.keys(b).length;
  const paired = origins.length > 0 && branchIds.every((id, index) => same(receipts[index], origins[0]) || id === origins[0].branch_id);
  return { commonRange: [lower, upper], receipts, paired, records };
}

/// `comparison_projection`: the comparison's JSON text from the frames' texts.
function projection(api, runId, minute, context, frames) {
  const differences = frames.slice(1).map((frame) => {
    const branchId = JSON.parse(frame).branch_id;
    return `{"branch_id":${JSON.stringify(branchId)},"metrics":${api.wasm.kpiDifferences(encoder.encode(frames[0]), encoder.encode(frame))}}`;
  });
  const interpretation = "Same Run/seed/time descriptive model comparison. Shared lineage does not establish real-world causation.";
  return (
    `{"common_range":${JSON.stringify(context.commonRange)},"differences":[${differences.join(",")}],"frames":[${frames.join(",")}],` +
    `"interpretation":${JSON.stringify(interpretation)},"minute":${minute},"origin_receipts":${receiptsJson(context.receipts)},` +
    `"run_id":${JSON.stringify(runId)},"schema_version":1,"shared_origin":${context.paired}}`
  );
}

/// The receipts with sorted keys (their values are strings and integers).
function receiptsJson(receipts) {
  const one = (receipt) =>
    receipt === null ? "null" : `{${Object.keys(receipt).sort().map((key) => `${JSON.stringify(key)}:${JSON.stringify(receipt[key])}`).join(",")}}`;
  return `[${receipts.map(one).join(",")}]`;
}

/// `GET runs/{run}/compare?branch_ids=&minute=`: the query as `queryOf` read it, and `integer`, the
/// server's int() of a query value.
export async function compareResponse(api, projectId, runId, query, integer) {
  if ([...query.values()].some((values) => values.length !== 1)) throw refused("comparison parameters must each occur once");
  const minute = integer(query.get("minute")[0], "comparison minute must be an integer");
  const branchIds = query.get("branch_ids")[0].split(",");
  const context = await comparisonContext(api, projectId, runId, branchIds, minute);
  const frames = [];
  for (const id of branchIds) frames.push(await api.frameText(projectId, runId, await api.branch(projectId, runId, id), minute));
  return projection(api, runId, minute, context, frames);
}

/// `replay-frames` of two branches: the comparison of each minute from `start` to `end`, after
/// the checks `BrowserApi.replay` made.
export async function replayPair(api, projectId, runId, records, branchIds, start, end) {
  const context = await comparisonContext(api, projectId, runId, branchIds, start);
  const samples = [];
  for (let minute = start; minute <= end; minute += 1) {
    const frames = [];
    for (const record of records) frames.push(await api.frameText(projectId, runId, record, minute));
    samples.push(projection(api, runId, minute, context, frames));
  }
  return `{"branch_ids":${JSON.stringify(branchIds)},"end":${end},"samples":[${samples.join(",")}],"schema_version":1,"start":${start}}`;
}
