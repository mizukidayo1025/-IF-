// The browser's answer to a question about a branch's results (Issue #308, under #292):
// `POST runs/{run}/branches/{branch}/questions`, as the Python server answers it without an API key
// (src/sheltersim/web/results_chat.py `_ask` and `_project_evidence`): the request's checks, the
// evidence's checks (a completed branch, the minute, the selected person, the branches compared),
// the fixed answer that no question can be answered without the Claude API, the context of the
// question and its id, and the turn kept beside the branch.
//
// The context's id is the server's: "c" + the first 32 hex digits of the sha256 of the context's
// JSON, "-", and 24 random hex digits, or the id the request gave when it is of the same context.
// The turn is kept in the branch's folder as a line of `conversations.jsonl`, with the question as
// the server keeps it (contact details and entity names redacted, `questionText` of the core).
//
// Where the browser differs: a `history_query` (the screen never sends one) is refused, 409
// `browser_unsupported`, after the checks of the branch and the minute; the server would cite the
// saved history.

import { ApiError } from "./api_answers.mjs";
import { comparisonContext } from "./comparison.mjs";
import * as store from "./project_store.mjs";

const encoder = new TextEncoder();
const decoder = new TextDecoder();
const MAX_QUESTION = 2000;
const ALLOWED = ["run_id", "project_id", "comparison_branch_ids", "history_query", "demo_branch_id", "branch_id", "minute", "agent_id", "question", "context_id"];
const ANSWER = "Claude APIが利用できないため、結果に関する質問へ回答できません。レポートと記録済み根拠は引き続き確認できます。";
// The server's 400 for a request its checks refuse (a WebQueryError is a ValueError).
const invalid = () => new ApiError(400, "invalid project request");

/// `str.isspace` of a character (JS's `\s` also takes U+FEFF, which Python does not).
const isSpace = (char) => /^[\t\n\v\f\r \u001c-\u001f\u0085\u00a0\u1680\u2000-\u200a\u2028\u2029\u202f\u205f\u3000]$/.test(char);

/// Python's `bool()` of a JSON value.
const truthy = (value) =>
  !(value === undefined || value === null || value === false || value === 0 || value === "" || (Array.isArray(value) && value.length === 0) || (typeof value === "object" && value !== null && Object.keys(value).length === 0));

/// The number of code points of a string (Python's `len`).
const length = (text) => Array.from(text).length;

/// JSON with sorted keys and no spaces, as Python's `json.dumps(sort_keys=True)` of strings,
/// integers, booleans, null, lists and objects.
function sortedJson(value) {
  if (Array.isArray(value)) return `[${value.map(sortedJson).join(",")}]`;
  if (value !== null && typeof value === "object") {
    const keys = Object.keys(value).sort((a, b) => (a < b ? -1 : a > b ? 1 : 0));
    return `{${keys.map((key) => `${JSON.stringify(key)}:${sortedJson(value[key])}`).join(",")}}`;
  }
  return JSON.stringify(value);
}

function randomHex(bytes) {
  return Array.from(crypto.getRandomValues(new Uint8Array(bytes)), (byte) => byte.toString(16).padStart(2, "0")).join("");
}

/// `POST …/questions`: the answer's JSON text.
export async function questionResponse(api, projectId, runId, branchId, document) {
  const { wasm, tree } = api;
  const body = { ...document, project_id: projectId, run_id: runId, branch_id: branchId };
  if (Object.keys(body).some((key) => !ALLOWED.includes(key))) throw invalid();
  const question = body.question;
  if (typeof question !== "string" || Array.from(question).every(isSpace) || length(question) > MAX_QUESTION) throw invalid();
  const requested = body.context_id;
  if (requested !== undefined && requested !== null && (typeof requested !== "string" || length(requested) > 64)) throw invalid();
  const demo = body.demo_branch_id;
  if (truthy(runId) === truthy(demo)) throw invalid();
  let minute = body.minute;
  if (minute !== undefined && minute !== null && (typeof minute !== "number" || !Number.isInteger(minute) || minute < 0)) throw invalid();
  const agentId = body.agent_id ?? null;
  if (agentId !== null && (typeof agentId !== "string" || length(agentId) > 128)) throw invalid();
  let stored = wasm.questionText(question);
  for (const privateId of [runId, demo, branchId, agentId]) {
    if (typeof privateId === "string" && privateId) stored = stored.replaceAll(privateId, "[selected entity]");
  }
  stored = stored.replaceAll(projectId, "[selected project]");

  // _project_evidence
  const record = await store.branchRecord(tree, projectId, runId, branchId);
  const scenario = JSON.parse(decoder.decode(await store.runScenarioBytes(tree, projectId, runId)));
  if (record.computed_until !== scenario.duration_minutes) throw invalid();
  minute = minute ?? record.computed_until;
  if (minute < record.fork_minute || minute > record.computed_until) throw new store.ProjectStoreError("minute is outside this branch's computed range");
  const frame = JSON.parse(await api.frameText(projectId, runId, await api.branch(projectId, runId, branchId), minute));
  if (body.history_query !== undefined && body.history_query !== null) throw new ApiError(409, "ブラウザー版では未対応です（質問の履歴の根拠）", "browser_unsupported");
  if (agentId && !frame.people.some((person) => person.id === agentId)) throw invalid();
  const comparisons = body.comparison_branch_ids ?? [];
  if (!Array.isArray(comparisons) || comparisons.length > 7 || comparisons.some((id) => typeof id !== "string")) throw invalid();
  if (comparisons.length > 0) {
    const branches = [...new Set([branchId, ...comparisons])];
    await comparisonContext(api, projectId, runId, branches, minute);
  }
  const context = {
    project_id: projectId,
    run_id: runId,
    branch_id: branchId,
    requested_minute: minute,
    captured_minute: minute,
    agent_id: agentId,
    comparison_branch_ids: comparisons,
    history_scope: null,
  };
  const prefix = wasm.sha256Hex(encoder.encode(sortedJson(context))).slice(0, 32);
  const kept = typeof requested === "string" && new RegExp(`^c${prefix}-[0-9a-f]{24}$`).test(requested);
  const contextId = kept ? requested : `c${prefix}-${randomHex(12)}`;
  const result = {
    answer: ANSWER,
    sources: [],
    limitations: ["LLM is unavailable; no answer was generated."],
    mode: "offline",
    model: null,
    usage: { api_request_count: 0, estimated_cost_usd: 0 },
    context_id: contextId,
    context,
    availability: "unavailable",
  };
  const turn = {
    turn_id: `t-${Date.now()}000000-${randomHex(4)}`,
    question: stored,
    answer: result.answer,
    evidence: [],
    limitations: result.limitations,
    usage: result.usage,
    model_version: null,
    minute,
    conversation_id: contextId,
    claims: [],
    context,
    sources: [],
  };
  const path = [projectId, "runs", runId, "branches", branchId, "conversations.jsonl"];
  const old = (await tree.read(path)) ?? new Uint8Array(0);
  const line = encoder.encode(`${sortedJson(turn)}\n`);
  const grown = new Uint8Array(old.length + line.length);
  grown.set(old);
  grown.set(line, old.length);
  await tree.write(path, grown);
  return sortedJson(result);
}
