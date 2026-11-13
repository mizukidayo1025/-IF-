// The dedicated Worker that answers the screen's API in the browser (Issue #266): js/api_fetch.js
// starts it and hands it each `/api/v1/...` request; js/browser_api.mjs answers it from the origin
// private file system (OPFS) and WASM. A dedicated Worker, because OPFS's synchronous access
// handles, which a stored Run is read through, are a dedicated Worker's.
//
// Where things are (relative to this file): the WASM (`wasm-bindgen --target web`) in pkg/, the
// sample's manifest at `sampleUrl` (the page's `data-sample` on the script, see api_fetch.js;
// /sample/sample.json by default). The store is OPFS's folder projects/; the marker of the sample
// it holds (browser_api.mjs) is beside it. Every request, and the sample's install, holds the Web
// Lock "sheltersim-browser-store", so two tabs never open the same files at once.
//
// Messages in:  { type: "start", sampleUrl } once, then { type: "request", id, method, path,
//               search, body } or { type: "stats", id }
// Messages out: { type: "progress", phase, bytes, total } while the sample is installed;
//               { type: "ready", installed, ms } or { type: "failed", message } once;
//               { type: "answer", id, status, contentType, body } for each request;
//               { type: "stats", id, stats } for { type: "stats", id } (checks).

import { BrowserApi, installSample, SAMPLE_MARKER } from "./browser_api.mjs";
import { opfsTree } from "./project_store.mjs";

const LOCK = "sheltersim-browser-store";
const locked = (task) => (navigator.locks ? navigator.locks.request(LOCK, task) : task());

async function readFile(folder, name) {
  try {
    const handle = await folder.getFileHandle(name);
    return new Uint8Array(await (await handle.getFile()).arrayBuffer());
  } catch (error) {
    if (error?.name === "NotFoundError") return null;
    throw error;
  }
}

async function writeFile(folder, name, bytes) {
  const handle = await folder.getFileHandle(name, { create: true });
  const access = await handle.createSyncAccessHandle();
  try {
    access.truncate(0);
    let at = 0;
    while (at < bytes.length) at += access.write(bytes.subarray(at), { at });
    access.flush();
  } finally {
    access.close();
  }
}

async function start(sampleUrl) {
  const wasm = await import(new URL("./pkg/sheltersim_wasm.js", import.meta.url).href);
  await wasm.default();
  if (!navigator.storage?.getDirectory) throw new Error("このブラウザーはサイト専用の保存（OPFS）を使えません");
  const root = await navigator.storage.getDirectory();
  const tree = opfsTree(await root.getDirectoryHandle("projects", { create: true }));
  const manifestUrl = new URL(sampleUrl, self.location.href);
  const response = await fetch(manifestUrl, { cache: "no-cache" });
  if (!response.ok) throw new Error(`計算済みの例の目録を読めません（HTTP ${response.status}）`);
  const manifest = await response.json();
  const installed = await locked(() =>
    installSample({
      wasm,
      tree,
      manifest,
      async fetchBytes(path) {
        const part = await fetch(new URL(path, manifestUrl));
        if (!part.ok) throw new Error(`計算済みの例の ${path} を読めません（HTTP ${part.status}）`);
        return new Uint8Array(await part.arrayBuffer());
      },
      async readMarker() {
        const bytes = await readFile(root, SAMPLE_MARKER);
        try {
          return bytes === null ? null : JSON.parse(new TextDecoder().decode(bytes));
        } catch {
          return null;
        }
      },
      writeMarker: (bytes) => writeFile(root, SAMPLE_MARKER, bytes),
      progress: (state) => self.postMessage({ type: "progress", ...state }),
    }),
  );
  const api = new BrowserApi({
    wasm,
    tree,
    quota: async () => (await navigator.storage.estimate()).quota ?? null,
  });
  return { api, wasm, installed };
}

let ready = null;

self.onmessage = async ({ data }) => {
  if (data.type === "start") {
    ready = start(data.sampleUrl);
    ready.then(
      ({ installed }) => self.postMessage({ type: "ready", installed: installed.installed, ms: installed.ms }),
      (error) => self.postMessage({ type: "failed", message: String(error?.message ?? error) }),
    );
    return;
  }
  if (data.type === "stats") {
    // What the answers cost so far, for checks: seeks, ticks stepped, their times, WASM's memory.
    const { api, wasm } = await ready;
    self.postMessage({ type: "stats", id: data.id, stats: { ...api.stats, memoryBytes: wasm.memoryBytes() } });
    return;
  }
  if (data.type !== "request") return;
  let answer;
  try {
    const { api } = await ready;
    answer = await locked(() => api.answer(data));
  } catch (error) {
    answer = {
      status: 503,
      type: "application/json; charset=utf-8",
      body: JSON.stringify({ error: `ブラウザー版の準備に失敗しました：${error?.message ?? error}`, code: "browser_unavailable" }),
    };
  }
  self.postMessage({ type: "answer", id: data.id, status: answer.status, contentType: answer.type, body: answer.body });
};
