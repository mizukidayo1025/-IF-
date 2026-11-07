"""Exercise the real polling callback with stable/changed saved frame targets."""

import shutil
import subprocess
from pathlib import Path

import pytest


def test_polling_reuses_only_the_same_immutable_frame():
    node = shutil.which("node")
    if node is None:
        pytest.skip("Node is required for the UI callback regression")
    source = Path("src/sheltersim/web/static/demo-v2.js").resolve()
    script = r"""
import fs from "node:fs";
import assert from "node:assert/strict";
const source = fs.readFileSync(process.argv[1], "utf8");
const body = source.split("  async function refreshRun() {")[1].split("  async function seek(")[0];
const elements = new Map();
const $ = id => {
  if (!elements.has(id)) elements.set(id, {value:"",replaceChildren(){},options:[]});
  return elements.get(id);
};
const el = () => ({}), shortBranch = id => id;
let state, next, seeks, reads, controlled;
const api = async () => {reads++;return next;};
const runBase = () => "/run", base = () => "/project";
const reference = () => state.data.branches.find(b => b.branch_id === state.branch);
const upper = () => reference().computed_until, lower = () => reference().fork_minute;
const seek = async minute => {seeks.push(minute);};
const controls = () => {controlled++;}, remember = () => {}, note = () => {};
const clearTimeout = () => {}, setTimeout = () => 1;
const refresh = eval("(async function refreshRun() {" + body + ")");
const branch = {branch_id:"main",checkpoint_hash:"a",computed_until:30,fork_minute:0};
for (const test of [
  {name:"unchanged",frameMinute:30,minute:30,hash:"a",head:30,expected:[]},
  {name:"new checkpoint",frameMinute:30,minute:30,hash:"b",head:60,expected:[60]},
  {name:"same past frame",frameMinute:13,minute:13,hash:"a",head:30,expected:[]},
  {name:"different minute",frameMinute:0,minute:13,hash:"a",head:30,expected:[13]},
  {name:"different run",frameMinute:30,minute:30,hash:"a",head:30,frameRun:"other",expected:[30]},
  {name:"different branch",frameMinute:30,minute:30,hash:"a",head:30,
    frameBranch:"child",expected:[30]},
  {name:"completion only",frameMinute:30,minute:30,hash:"a",head:30,status:"completed",expected:[]},
]) {
  elements.clear();seeks=[];reads=0;controlled=0;
  state = {run:"r",loadedRun:"r",branch:"main",scope:0,minute:test.minute,
    data:{branches:[{...branch}]},
    frame:{run_id:test.frameRun||"r",branch_id:test.frameBranch||"main",minute:test.frameMinute}};
  next = {status:test.status||"running",
    branches:[{...branch,checkpoint_hash:test.hash,computed_until:test.head}]};
  await refresh();
  assert.deepEqual(seeks,test.expected,test.name);
  assert.equal(reads,1,test.name+" still polls status");
  assert.equal(controlled,1,test.name+" updates controls");
}
"""
    subprocess.run([node, "--input-type=module", "-e", script, str(source)], check=True)


def test_applying_an_intervention_waits_for_the_fork_process_to_list_the_child():
    """The server forks in its own process, so intervention-apply answers before the
    child Branch exists. The screen must wait for it, as /demo does, or the comparison it
    opens next finds no Branch (「介入後の分岐を選択してください。」)."""
    node = shutil.which("node")
    if node is None:
        pytest.skip("Node is required for the UI callback regression")
    source = Path("src/sheltersim/web/static/demo-v2.js").resolve()
    text = source.read_text(encoding="utf-8")
    apply = text.split("intervention-apply`, {")[1].split('show("comparison")')[0]
    assert apply.index("await waitForked(child);") < apply.index("state.branch = child;")
    script = r"""
import fs from "node:fs";
import assert from "node:assert/strict";
const source = fs.readFileSync(process.argv[1], "utf8");
const body = source
  .split("  async function waitForked(child) {")[1]
  .split("  async function previewIntervention(")[0];
let state, answers, reads;
const api = async () => {
  reads++;
  const next = answers.shift();
  if (next.scope) state.scope++;
  return next.data;
};
const runBase = () => "/run";
const setTimeout = (resolve) => {resolve();return 1;};
const waitForked = eval("(async function waitForked(child) {" + body + ")");
const run = (status, branches, last_fork) => ({data:{status, branches, last_fork}});
const main = {branch_id:"main"}, child = {branch_id:"c"};
// Not listed while the fork computes, then listed while computing, then done.
state = {scope:1}; reads = 0;
answers = [run("running",[main]), run("running",[main,child]), run("paused",[main,child])];
await waitForked("c");
assert.equal(reads, 3);
assert.equal(state.data.branches.length, 2);
// Another branch computes meanwhile: a child with its own idle status has ended.
state = {scope:1}; reads = 0;
answers = [run("running",[{...main,status:"running"},{...child,status:"paused"}])];
await waitForked("c");
assert.equal(reads, 1);
// A failed fork says why instead of waiting forever.
state = {scope:1}; reads = 0;
answers = [run("paused",[main],{status:"failed",error:"storage capacity exceeded"})];
await assert.rejects(waitForked("c"), /分岐を作成できませんでした: storage capacity exceeded/);
// The fork ended without its child and gave no reason.
state = {scope:1}; reads = 0;
answers = [run("paused",[main])];
await assert.rejects(waitForked("c"), /理由を取得できませんでした/);
// Another Run was opened meanwhile.
state = {scope:1}; reads = 0;
answers = [{...run("running",[main]),scope:true}];
await assert.rejects(waitForked("c"), /表示するRunが変更されました/);
"""
    subprocess.run(
        [node, "--input-type=module", "-e", script, str(source)],
        check=True,
        capture_output=True,
        text=True,
    )
