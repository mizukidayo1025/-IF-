// What a Run in the browser stores, and reading it back (Issue #252, PR 6-5), with saved points to
// seek to a minute (Issue #251).
//
// A Run's folder holds two files that are only ever appended to:
//   data.pack      the pieces, each compressed with CompressionStream("deflate-raw")
//   journal.jsonl  one line per piece or event: its JSON, a tab, the sha256 of that JSON, "\n"
// A piece is written to data.pack and flushed before its journal line is, so a line never points at
// bytes that are not there. Reading stops at the first line that is cut short or whose sha256 is
// not its JSON's (a write the page did not finish); bytes of data.pack no line points at are
// ignored. Each piece's line holds the sha256 of its bytes before compression, which reading
// checks: compressed bytes may differ between browsers, the bytes they hold may not.
//
// The pieces, in the order they are written (`runAndStore`):
//   export        the exported world at minute 0 the Run started from (its JSON bytes)
//   log_tables    the log's string and shape tables' new entries (Packer::write_tables)
//   log_block     a sealed block of the packed log (EventLog::packed_store), by its index
//   sample_tables, sample_block   the same of the metric samples (MetricSamples::store)
//   saved         a saved point (#251): the Run at a minute (Simulation.saved_point,
//                 archive::SavedPoint, a versioned binary: the world without its log and the counts
//                 of records and samples up to then), every `saveEvery` minutes and at the end
//   world         the world at the last minute without its log (archive::world_parts)
// and the lines without a piece: "run" first (the format and what the Run is of) and "end" last
// (the minute and the digests the Run gave). Tables come before the blocks that use them. Blocks
// are taken as they seal, after a call to `advance` (no transaction is open then), and every block
// at the end. A "saved" line has the minute, the counts of records and samples, the saved point's
// version, and how many log and sample blocks hold those records and samples (`log_blocks`,
// `sample_blocks`): the blocks there were when it was saved, the last of them maybe not stored yet.
//
// Formats: "sheltersim-browser-run-v2" (#251) is written. "sheltersim-browser-run-v1" (#252) is the
// same without saved points and is still read: seeking in it computes from minute 0. Any other is
// an error. The log and the samples are only appended to, so a saved point holds no copy of them:
// seeking (`RunReader.seek`) restores the nearest saved point at or before the minute from it and
// the stored log's and samples' first records and samples up to its counts, then computes the
// ticks up to the minute (at most `saveEvery` - 1).
//
// Storage is a backend: `opfsBackend` (the browser's origin private file system, through the
// synchronous access handles a dedicated Worker has) or `memoryBackend` (Node's tests). A backend
// opens a file by name; a file has size(), read(at, length), write(bytes, at), truncate(size),
// flush() and close(), all synchronous.
//
// `wasm` is the module wasm-bindgen made of sheltersim_wasm (any target): its Simulation,
// StoredRun, Restore, worldPartDigests and sha256Hex.

export const FORMAT = "sheltersim-browser-run-v2";
/// The format of #252, without saved points: read, never written.
export const FORMAT_V1 = "sheltersim-browser-run-v1";
/// The formats this code reads.
export const FORMATS = [FORMAT_V1, FORMAT];
/// Minutes between saved points by default: 6 hours (#263).
export const SAVE_EVERY = 360;
const DATA = "data.pack";
const JOURNAL = "journal.jsonl";
const encoder = new TextEncoder();
const decoder = new TextDecoder();

async function transform(bytes, stream) {
  const piped = new Blob([bytes]).stream().pipeThrough(stream);
  return new Uint8Array(await new Response(piped).arrayBuffer());
}

export const deflateRaw = (bytes) => transform(bytes, new CompressionStream("deflate-raw"));
export const inflateRaw = (bytes) => transform(bytes, new DecompressionStream("deflate-raw"));

/// Files held in memory, for tests: `files` maps a name to its bytes.
export function memoryBackend(files = new Map()) {
  return {
    files,
    async open(name, { truncate = false } = {}) {
      if (truncate || !files.has(name)) files.set(name, new Uint8Array(0));
      return {
        size: () => files.get(name).length,
        read(at, length) {
          return files.get(name).slice(at, at + length);
        },
        write(bytes, at) {
          const old = files.get(name);
          const grown = new Uint8Array(Math.max(old.length, at + bytes.length));
          grown.set(old);
          grown.set(bytes, at);
          files.set(name, grown);
          return bytes.length;
        },
        truncate(size) {
          files.set(name, files.get(name).slice(0, size));
        },
        flush() {},
        close() {},
      };
    },
  };
}

/// The files of a folder of the origin private file system, through synchronous access handles:
/// only a dedicated Worker can make them.
export function opfsBackend(directory) {
  return {
    directory,
    async open(name, { truncate = false } = {}) {
      const handle = await directory.getFileHandle(name, { create: true });
      const access = await handle.createSyncAccessHandle();
      if (truncate) access.truncate(0);
      return {
        size: () => access.getSize(),
        read(at, length) {
          const bytes = new Uint8Array(length);
          const read = access.read(bytes, { at });
          return bytes.subarray(0, read);
        },
        write: (bytes, at) => access.write(bytes, { at }),
        truncate: (size) => access.truncate(size),
        flush: () => access.flush(),
        close: () => access.close(),
      };
    },
  };
}

/// Writes a Run's pieces and journal lines (see the top of this file).
export class RunWriter {
  constructor(wasm, data, journal) {
    this.wasm = wasm;
    this.data = data;
    this.journal = journal;
    this.log = { strings: 0, shapes: 0, blocks: 0 };
    this.samples = { strings: 0, shapes: 0, blocks: 0 };
    this.sizes = {};
    this.compressMs = 0;
    this.writeMs = 0;
    this.saveMs = 0;
    this.saved = [];
  }

  /// A writer of a new Run in the backend's files, which it empties.
  static async create(wasm, backend) {
    const data = await backend.open(DATA, { truncate: true });
    const journal = await backend.open(JOURNAL, { truncate: true });
    return new RunWriter(wasm, data, journal);
  }

  /// Appends a journal line and flushes it.
  line(entry) {
    const json = JSON.stringify(entry);
    const bytes = encoder.encode(`${json}\t${this.wasm.sha256Hex(encoder.encode(json))}\n`);
    const started = performance.now();
    this.journal.write(bytes, this.journal.size());
    this.journal.flush();
    this.writeMs += performance.now() - started;
  }

  /// Appends a piece to data.pack, compressed, flushes it, then writes its journal line.
  async piece(kind, raw, extra = {}) {
    let started = performance.now();
    const compressed = await deflateRaw(raw);
    this.compressMs += performance.now() - started;
    started = performance.now();
    const offset = this.data.size();
    this.data.write(compressed, offset);
    this.data.flush();
    this.writeMs += performance.now() - started;
    const size = (this.sizes[kind] ??= { pieces: 0, raw: 0, stored: 0 });
    size.pieces += 1;
    size.raw += raw.length;
    size.stored += compressed.length;
    this.line({ kind, offset, length: compressed.length, raw_length: raw.length, sha256: this.wasm.sha256Hex(raw), ...extra });
  }

  /// The tables' new entries and the newly sealed blocks (every block when `last`) of the log and
  /// the samples of `simulation`. Call it between calls to `advance`.
  async take(simulation, last) {
    const stores = [
      ["log", this.log, () => simulation.log_block_counts(), (s, h) => simulation.log_tables(s, h), () => simulation.log_table_counts(), (i) => simulation.log_block(i)],
      ["sample", this.samples, () => simulation.sample_block_counts(), (s, h) => simulation.sample_tables(s, h), () => simulation.sample_table_counts(), (i) => simulation.sample_block(i)],
    ];
    for (const [name, at, blockCounts, tables, tableCounts, block] of stores) {
      const [blocks, sealed] = blockCounts();
      const end = last ? blocks : sealed;
      if (end <= at.blocks) continue;
      const [strings, shapes] = tableCounts();
      await this.piece(`${name}_tables`, tables(at.strings, at.shapes), { strings_from: at.strings, shapes_from: at.shapes, strings, shapes });
      at.strings = strings;
      at.shapes = shapes;
      for (let index = at.blocks; index < end; index++) {
        await this.piece(`${name}_block`, block(index), { index });
      }
      at.blocks = end;
    }
  }

  /// A saved point of `simulation` now (#251), after the tables and blocks sealed so far. Call it
  /// between calls to `advance`. `saveMs` counts its time, its compression and writing included.
  async save(simulation) {
    await this.take(simulation, false);
    const started = performance.now();
    const point = simulation.saved_point();
    const line = {
      minute: simulation.minute(),
      records: simulation.log_records(),
      samples: simulation.metric_count(),
      point_version: this.wasm.savedPointVersion(),
      log_blocks: simulation.log_block_counts()[0],
      sample_blocks: simulation.sample_block_counts()[0],
    };
    await this.piece("saved", point, line);
    this.saveMs += performance.now() - started;
    this.saved.push(line);
  }

  close() {
    this.data.close();
    this.journal.close();
  }
}

/// Runs the exported world `exportBytes` (or the world `world` of a file of named worlds, as the
/// fixtures hold them) for its whole clock, `ticks` ticks per call to `advance`, and stores it in
/// `backend` as it goes, with a saved point every `saveEvery` minutes and at the end (none when
/// `saveEvery` is 0). `progress(minute, duration)` is called after each call. Returns what the Run
/// gave and what storing it took.
export async function runAndStore({ wasm, exportBytes, world = null, backend, ticks = 1440, saveEvery = SAVE_EVERY, progress = () => {} }) {
  if (!Number.isInteger(saveEvery) || saveEvery < 0) throw new Error(`saveEvery is not a count of minutes: ${saveEvery}`);
  const writer = await RunWriter.create(wasm, backend);
  const simulation = world === null ? new wasm.Simulation(exportBytes) : wasm.Simulation.fromWorlds(exportBytes, world);
  try {
    if (!simulation.log_all_packed()) throw new Error("the exported world's log is not all packed");
    const duration = simulation.duration();
    writer.line({ kind: "run", format: FORMAT, export_sha256: wasm.sha256Hex(exportBytes), export_bytes: exportBytes.length, world, duration_minutes: duration, save_every: saveEvery });
    await writer.piece("export", exportBytes);
    let tickMs = 0;
    for (;;) {
      const minute = simulation.minute();
      const step = saveEvery > 0 ? Math.min(ticks, (Math.floor(minute / saveEvery) + 1) * saveEvery - minute) : ticks;
      const started = performance.now();
      const ran = simulation.advance(step);
      tickMs += performance.now() - started;
      progress(simulation.minute(), duration);
      await writer.take(simulation, false);
      if (ran < step) break;
      if (saveEvery > 0 && simulation.minute() % saveEvery === 0 && simulation.minute() < duration) await writer.save(simulation);
    }
    await writer.take(simulation, true);
    if (saveEvery > 0) await writer.save(simulation);
    await writer.piece("world", simulation.world_parts());
    if (!simulation.log_all_packed()) throw new Error("a record of the log is not packed");
    const parts = JSON.parse(simulation.part_digests_json());
    const end = {
      kind: "end",
      minute: simulation.minute(),
      records: simulation.log_records(),
      samples: simulation.metric_count(),
      parts,
      metrics: simulation.metrics_digest(),
    };
    writer.line(end);
    return {
      end,
      digests: JSON.parse(simulation.digests_json()),
      tickMs,
      compressMs: writer.compressMs,
      writeMs: writer.writeMs,
      saveMs: writer.saveMs,
      saved: writer.saved,
      sizes: writer.sizes,
      memoryBytes: wasm.memoryBytes(),
    };
  } finally {
    simulation.free();
    writer.close();
  }
}

/// The journal's lines up to the first that is cut short or does not match its sha256.
export function journalLines(wasm, text) {
  const lines = [];
  let torn = false;
  for (const line of text.split("\n")) {
    if (line === "") continue;
    const tab = line.lastIndexOf("\t");
    const json = tab < 0 ? "" : line.slice(0, tab);
    if (tab < 0 || wasm.sha256Hex(encoder.encode(json)) !== line.slice(tab + 1)) {
      torn = true;
      break;
    }
    lines.push(JSON.parse(json));
  }
  return { lines, torn };
}

/// Reads a stored Run back from `backend` and checks it: each piece's bytes against its sha256, and
/// the digests the log, the samples and the world read back to against the ones the "end" line
/// holds. Returns the digests read, whether the Run is complete (it has its "end" line) and
/// whether everything matched; an error for a piece whose bytes are not what its line says.
export async function readAndVerify({ wasm, backend }) {
  const data = await backend.open(DATA);
  const journal = await backend.open(JOURNAL);
  const started = performance.now();
  try {
    const { lines, torn } = journalLines(wasm, decoder.decode(journal.read(0, journal.size())));
    checkRunLine(lines);
    const reader = new wasm.StoredRun();
    let world = null;
    let exportSha = null;
    let end = null;
    let savedPoints = 0;
    let readMs = 0;
    let inflateMs = 0;
    try {
      for (const line of lines.slice(1)) {
        if (line.kind === "end") {
          end = line;
          break;
        }
        let at = performance.now();
        const stored = data.read(line.offset, line.length);
        readMs += performance.now() - at;
        if (stored.length !== line.length) throw new Error(`${line.kind}: data.pack ends before the piece`);
        at = performance.now();
        const raw = await inflateRaw(stored);
        inflateMs += performance.now() - at;
        if (raw.length !== line.raw_length || wasm.sha256Hex(raw) !== line.sha256) throw new Error(`${line.kind}: the bytes are not the ones written`);
        switch (line.kind) {
          case "export":
            exportSha = wasm.sha256Hex(raw);
            break;
          case "log_tables":
            reader.read_log_tables(raw);
            break;
          case "log_block":
            reader.read_log_block(raw);
            break;
          case "sample_tables":
            reader.read_sample_tables(raw);
            break;
          case "sample_block":
            reader.read_sample_block(raw);
            break;
          case "world":
            world = JSON.parse(wasm.worldPartDigests(raw));
            break;
          case "saved":
            if (lines[0].format === FORMAT_V1) throw new Error("a saved point in a Run of format v1");
            checkSavedPoint(wasm, line, raw);
            savedPoints += 1;
            break;
          default:
            throw new Error(`a piece of an unknown kind: ${line.kind}`);
        }
      }
      const parts = world ? { ...world, event_log: reader.log_digest() } : null;
      const mismatches = [];
      if (end) {
        if (exportSha !== lines[0].export_sha256) mismatches.push("export");
        for (const [part, digest] of Object.entries(end.parts)) {
          if (!parts || parts[part] !== digest) mismatches.push(part);
        }
        if (parts && Object.keys(parts).length !== Object.keys(end.parts).length) mismatches.push("parts");
        if (reader.log_records() !== end.records) mismatches.push("records");
        if (reader.metrics_digest() !== end.metrics || reader.sample_count() !== end.samples) mismatches.push("metrics");
      }
      return {
        complete: end !== null,
        torn,
        verified: end !== null && mismatches.length === 0,
        mismatches,
        parts,
        metrics: reader.metrics_digest(),
        records: reader.log_records(),
        samples: reader.sample_count(),
        savedPoints,
        storedBytes: data.size() + journal.size(),
        readMs,
        inflateMs,
        totalMs: performance.now() - started,
      };
    } finally {
      reader.free();
    }
  } finally {
    data.close();
    journal.close();
  }
}

/// The journal's "run" line is of a format this code reads; an error otherwise.
function checkRunLine(lines) {
  if (lines.length === 0 || lines[0].kind !== "run") throw new Error("not a stored Run");
  if (!FORMATS.includes(lines[0].format)) throw new Error(`a stored Run of format ${lines[0].format}: this code reads ${FORMATS.join(", ")}`);
}

/// A saved point's bytes are one (`Restore`) of the minute and counts its line says.
function checkSavedPoint(wasm, line, raw) {
  const restore = new wasm.Restore(raw);
  try {
    if (restore.minute() !== line.minute || restore.records() !== line.records || restore.samples() !== line.samples) {
      throw new Error(`saved: the point is not the one its line says (minute ${line.minute})`);
    }
  } finally {
    restore.free();
  }
}

/// A stored Run opened to seek to its minutes (#251): `RunReader.open`, then `seek(minute)` as often
/// as needed, then `close()`. It reads the journal once and keeps the exported world's bytes.
export class RunReader {
  constructor(wasm, data, journal, lines, torn) {
    this.wasm = wasm;
    this.data = data;
    this.journal = journal;
    this.lines = lines;
    this.torn = torn;
    this.exported = null;
  }

  /// The stored Run in `backend`; an error for one of a format this code does not read.
  static async open({ wasm, backend }) {
    const data = await backend.open(DATA);
    const journal = await backend.open(JOURNAL);
    try {
      const { lines, torn } = journalLines(wasm, decoder.decode(journal.read(0, journal.size())));
      checkRunLine(lines);
      return new RunReader(wasm, data, journal, lines, torn);
    } catch (error) {
      data.close();
      journal.close();
      throw error;
    }
  }

  /// The "run" line.
  get run() {
    return this.lines[0];
  }

  /// The minute the Run's clock is complete at.
  duration() {
    return this.run.duration_minutes;
  }

  /// The "end" line, or null for a Run not stored to its end.
  end() {
    return this.lines.find((line) => line.kind === "end") ?? null;
  }

  /// The "saved" lines a seek can restore, by minute: those whose log and sample blocks are all
  /// stored (in a Run not stored to its end, the last ones may not be).
  savedPoints() {
    const stored = (kind) => this.lines.filter((line) => line.kind === kind).length;
    const logBlocks = stored("log_block");
    const sampleBlocks = stored("sample_block");
    return this.lines.filter((line) => line.kind === "saved" && line.log_blocks <= logBlocks && line.sample_blocks <= sampleBlocks);
  }

  /// A piece's bytes, checked against its line: an error for bytes that are not the ones written.
  async raw(line) {
    const stored = this.data.read(line.offset, line.length);
    if (stored.length !== line.length) throw new Error(`${line.kind}: data.pack ends before the piece`);
    const raw = await inflateRaw(stored);
    if (raw.length !== line.raw_length || this.wasm.sha256Hex(raw) !== line.sha256) throw new Error(`${line.kind}: the bytes are not the ones written`);
    return raw;
  }

  /// The exported world the Run started from, read once.
  async exportBytes() {
    if (this.exported === null) {
      const line = this.lines.find((each) => each.kind === "export");
      if (!line) throw new Error("the stored Run has no exported world");
      this.exported = await this.raw(line);
    }
    return this.exported;
  }

  /// The Run at `minute` (an integer from 0 to the duration): restored from the nearest saved
  /// point at or before it, or from the exported world when there is none, then computed to it.
  /// Returns { simulation, from, restoreMs, recomputeMs }: a `Simulation` the caller frees, the
  /// minute it was computed from, and the time restoring and computing took.
  async seek(minute) {
    const { wasm } = this;
    if (!Number.isInteger(minute) || minute < 0 || minute > this.duration()) {
      throw new Error(`minute ${minute} is not one of the Run (0 to ${this.duration()})`);
    }
    const started = performance.now();
    const point = this.savedPoints().filter((line) => line.minute <= minute).at(-1) ?? null;
    const exportBytes = await this.exportBytes();
    const world = this.run.world ?? null;
    let simulation;
    if (point === null) {
      simulation = world === null ? new wasm.Simulation(exportBytes) : wasm.Simulation.fromWorlds(exportBytes, world);
    } else {
      const restore = new wasm.Restore(await this.raw(point));
      try {
        for (const line of this.lines) {
          const log = line.kind === "log_tables" || line.kind === "log_block";
          const sample = line.kind === "sample_tables" || line.kind === "sample_block";
          if (!(log && restore.log_needed()) && !(sample && restore.samples_needed())) continue;
          const raw = await this.raw(line);
          if (line.kind === "log_tables") restore.read_log_tables(raw);
          else if (line.kind === "log_block") restore.read_log_block(raw);
          else if (line.kind === "sample_tables") restore.read_sample_tables(raw);
          else restore.read_sample_block(raw);
          if (!restore.log_needed() && !restore.samples_needed()) break;
        }
        simulation = world === null ? wasm.Simulation.restore(exportBytes, restore) : wasm.Simulation.restoreFromWorlds(exportBytes, world, restore);
      } finally {
        restore.free();
      }
    }
    const restored = performance.now();
    try {
      const ticks = minute - simulation.minute();
      if (ticks < 0 || simulation.advance(ticks) !== ticks) throw new Error(`the Run does not reach minute ${minute}`);
    } catch (error) {
      simulation.free();
      throw error;
    }
    return { simulation, from: point?.minute ?? 0, restoreMs: restored - started, recomputeMs: performance.now() - restored };
  }

  close() {
    this.data.close();
    this.journal.close();
  }
}
