// The Worker of project.html (#265): a Project in the origin private file system, exported to one
// bundle and imported back (js/project_store.mjs).
//
// Message in: { mode: "roundtrip", pkg, exportUrl, world, ticks } or { mode: "bundle", pkg, bundleUrl }
//   roundtrip  computes the world (of a file of named worlds) into a Project of OPFS, answers the
//              Project's routes, exports the Project to a bundle, removes the whole folder (the
//              store is empty), imports the bundle and answers the routes again
//   bundle     imports a bundle file (the sample of scripts/build_browser_sample.py) into an
//              empty folder and answers the routes
// The folder is projects-check/ under OPFS's root; it is removed afterwards unless `keep`.
// Message out: { type: "done", ... } or { type: "error", message }.

import * as store from "./js/project_store.mjs";

const FOLDER = "projects-check";
const encoder = new TextEncoder();

async function emptyTree(root) {
  await root.removeEntry(FOLDER, { recursive: true }).catch(() => {});
  return store.opfsTree(await root.getDirectoryHandle(FOLDER, { create: true }));
}

async function answers(tree, projectId) {
  const run = JSON.parse(await store.runsResponse(tree, projectId)).runs[0];
  return {
    projects: await store.projectsResponse(tree),
    project: await store.projectResponse(tree, projectId),
    runs: await store.runsResponse(tree, projectId),
    run: await store.runResponse(tree, projectId, run.run_id),
    evidence: await store.evidenceResponse(tree, projectId, run.run_id),
  };
}

async function roundtrip(wasm, root, data) {
  let tree = await emptyTree(root);
  const exportBytes = new Uint8Array(await (await fetch(data.exportUrl)).arrayBuffer());
  const scenarioBytes = encoder.encode(`{"id":"trial","seed":2026,"world":${JSON.stringify(data.world)}}`);
  await store.createProject(tree, "trial");
  await store.saveScenario(tree, "trial", { scenarioBytes, scenarioHash: await store.sha256Hex(scenarioBytes) });
  await store.createRun(tree, "trial", { runId: "run", scenarioId: "trial", scenarioVersion: 1, seed: 2026, modelVersions: {}, evidenceBytes: encoder.encode('{"schema_version":1}') });
  let started = performance.now();
  const computed = await store.computeBranch({
    wasm,
    tree,
    projectId: "trial",
    runId: "run",
    branchId: "main",
    run: { exportBytes, world: data.world, ticks: data.ticks ?? 1440, progress: (minute, duration) => self.postMessage({ type: "progress", minute, duration }) },
  });
  const computeMs = performance.now() - started;
  await store.setRunStatus(tree, "trial", "run", "completed");
  const before = await answers(tree, "trial");
  started = performance.now();
  const bundle = await store.exportProject({ tree, projectId: "trial", source: { page: "project.html", world: data.world } });
  const exportMs = performance.now() - started;
  tree = await emptyTree(root);
  const emptied = (await tree.usage()).files;
  started = performance.now();
  const imported = await store.importProject({ wasm, tree, bundle });
  const importMs = performance.now() - started;
  const after = await answers(tree, "trial");
  const [branch] = imported.branches;
  return {
    computeMs,
    exportMs,
    importMs,
    bundleBytes: bundle.size,
    filesWhenEmptied: emptied,
    record: computed.record,
    partsComputed: computed.ran.end.parts,
    partsImported: branch.parts,
    metricsComputed: computed.ran.end.metrics,
    metricsImported: branch.metrics,
    verified: branch.verified,
    sameAnswers: Object.keys(before).filter((route) => before[route] !== after[route] && route !== "projects").length === 0,
    usage: await tree.usage(),
  };
}

async function importBundle(wasm, root, data) {
  const tree = await emptyTree(root);
  const bundle = await (await fetch(data.bundleUrl)).blob();
  const started = performance.now();
  const imported = await store.importProject({ wasm, tree, bundle });
  const importMs = performance.now() - started;
  const projectId = imported.projectId;
  const project = JSON.parse(await store.projectResponse(tree, projectId));
  const run = project.runs[0];
  const evidence = JSON.parse(await store.evidenceResponse(tree, projectId, run.run_id));
  return {
    importMs,
    bundleBytes: bundle.size,
    projectId,
    source: imported.source,
    branches: imported.branches.map(({ branch_id, verified, parts, metrics, records, samples }) => ({ branch_id, verified, parts, metrics, records, samples })),
    run: { ...run, model_versions: Object.keys(run.model_versions).length },
    scenarioPeople: project.scenarios[0].scenario.profiles?.length ?? null,
    evidenceKeys: Object.keys(evidence).length,
    projects: JSON.parse(await store.projectsResponse(tree)).projects,
    usage: await tree.usage(),
  };
}

self.onmessage = async ({ data }) => {
  try {
    const wasm = await import(data.pkg);
    await wasm.default();
    const root = await navigator.storage.getDirectory();
    const result = data.mode === "bundle" ? await importBundle(wasm, root, data) : await roundtrip(wasm, root, data);
    if (!data.keep) await root.removeEntry(FOLDER, { recursive: true });
    self.postMessage({ type: "done", ...result, memoryBytes: wasm.memoryBytes() });
  } catch (error) {
    self.postMessage({ type: "error", message: String(error?.stack ?? error) });
  }
};
