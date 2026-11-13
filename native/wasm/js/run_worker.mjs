// A dedicated module Worker that runs an exported world in WASM and stores it in the origin private
// file system as it goes (Issue #252, PR 6-5; js/run_store.mjs), then reads it back and checks it.
// A Worker, because the synchronous access handles OPFS writes through are a dedicated Worker's,
// and the page stays responsive while the ticks run.
//
// Message in:  { pkg, exportUrl, world, run, ticks, keep }
//   pkg        the URL of sheltersim_wasm.js (wasm-bindgen --target web)
//   exportUrl  the URL of the exported world (or of a file of named worlds, with `world`)
//   run        the folder under OPFS's runs/ to store the Run in; emptied first
//   ticks      ticks per call to `advance` (blocks are stored after each call); 1440 by default
//   keep       keep the folder afterwards (false: it is removed once checked)
// Messages out: { type: "progress", minute, duration }, then { type: "done", ran, back, estimate,
//   memoryBytes } or { type: "error", message }.

import { opfsBackend, readAndVerify, runAndStore } from "./run_store.mjs";

self.onmessage = async ({ data }) => {
  try {
    const wasm = await import(data.pkg);
    await wasm.default();
    const exportBytes = new Uint8Array(await (await fetch(data.exportUrl)).arrayBuffer());
    const root = await navigator.storage.getDirectory();
    const runs = await root.getDirectoryHandle("runs", { create: true });
    const folder = await runs.getDirectoryHandle(data.run, { create: true });
    const backend = opfsBackend(folder);
    const ran = await runAndStore({
      wasm,
      exportBytes,
      world: data.world ?? null,
      backend,
      ticks: data.ticks ?? 1440,
      progress: (minute, duration) => self.postMessage({ type: "progress", minute, duration }),
    });
    const back = await readAndVerify({ wasm, backend });
    const { usage, quota } = await navigator.storage.estimate();
    if (!data.keep) await runs.removeEntry(data.run, { recursive: true });
    self.postMessage({ type: "done", ran, back, estimate: { usage, quota }, memoryBytes: wasm.memoryBytes() });
  } catch (error) {
    self.postMessage({ type: "error", message: String(error?.stack ?? error) });
  }
};
