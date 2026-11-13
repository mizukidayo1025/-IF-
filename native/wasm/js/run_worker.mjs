// A dedicated module Worker that runs an exported world in WASM and stores it in the origin private
// file system as it goes (Issue #252, PR 6-5; js/run_store.mjs), then reads it back and checks it,
// and seeks in it (Issue #251).
// A Worker, because the synchronous access handles OPFS writes through are a dedicated Worker's,
// and the page stays responsive while the ticks run.
//
// Message in:  { pkg, exportUrl, world, run, ticks, keep, saveEvery, seek, chain }
//   pkg        the URL of sheltersim_wasm.js (wasm-bindgen --target web)
//   exportUrl  the URL of the exported world (or of a file of named worlds, with `world`)
//   run        the folder under OPFS's runs/ to store the Run in; emptied first
//   ticks      ticks per call to `advance` (blocks are stored after each call); 1440 by default
//   keep       keep the folder afterwards (false: it is removed once checked)
//   saveEvery  minutes between saved points (#251); 360 by default, 0 for none
//   seek       seek to the worst minute (the most ticks after a saved point, the latest); true by
//              default
//   chain      restore each saved point, compute to the next and check it is the one stored;
//              false by default
// Messages out: { type: "progress", minute, duration }, then { type: "done", ran, back, seek,
//   estimate, memoryBytes } or { type: "error", message }.

import { opfsBackend, readAndVerify, runAndStore, RunReader, SAVE_EVERY } from "./run_store.mjs";

/// Seeks in the stored Run: the worst minute, and with `chain` every saved point to the next.
async function seekIn(wasm, backend, chain) {
  const reader = await RunReader.open({ wasm, backend });
  try {
    const duration = reader.duration();
    const saved = reader.savedPoints();
    const before = saved.filter((line) => line.minute < duration).at(-1);
    const target = before ? Math.min(duration - 1, before.minute + reader.run.save_every - 1) : duration - 1;
    const worst = await reader.seek(target);
    worst.simulation.free();
    const result = { worst: { minute: target, from: worst.from, restoreMs: worst.restoreMs, recomputeMs: worst.recomputeMs } };
    if (chain) {
      const checked = { points: 0, mismatches: [], restoreMs: [], recomputeMs: 0 };
      for (const [index, line] of saved.entries()) {
        const sought = await reader.seek(line.minute);
        try {
          checked.restoreMs.push(sought.restoreMs);
          if (wasm.sha256Hex(sought.simulation.saved_point()) !== line.sha256) checked.mismatches.push(`restored at ${line.minute}`);
          const next = saved[index + 1];
          if (next) {
            const started = performance.now();
            sought.simulation.advance(next.minute - line.minute);
            checked.recomputeMs += performance.now() - started;
            if (wasm.sha256Hex(sought.simulation.saved_point()) !== next.sha256) checked.mismatches.push(`from ${line.minute} to ${next.minute}`);
          }
          checked.points += 1;
        } finally {
          sought.simulation.free();
        }
      }
      result.chain = checked;
    }
    return result;
  } finally {
    reader.close();
  }
}

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
      saveEvery: data.saveEvery ?? SAVE_EVERY,
      progress: (minute, duration) => self.postMessage({ type: "progress", minute, duration }),
    });
    const back = await readAndVerify({ wasm, backend });
    const seek = data.seek === false ? null : await seekIn(wasm, backend, data.chain === true);
    const { usage, quota } = await navigator.storage.estimate();
    if (!data.keep) await runs.removeEntry(data.run, { recursive: true });
    self.postMessage({ type: "done", ran, back, seek, estimate: { usage, quota }, memoryBytes: wasm.memoryBytes() });
  } catch (error) {
    self.postMessage({ type: "error", message: String(error?.stack ?? error) });
  }
};
