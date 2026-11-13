// What a Run in the browser stores, and reading it back (Issue #252, PR 6-5).
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
//   world         the world at the last minute without its log (archive::world_parts)
// and the lines without a piece: "run" first (the format and what the Run is of) and "end" last
// (the minute and the digests the Run gave). Tables come before the blocks that use them. Blocks
// are taken as they seal, after a call to `advance` (no transaction is open then), and every block
// at the end. The worlds between minute 0 and the last are not stored yet (#251).
//
// Storage is a backend: `opfsBackend` (the browser's origin private file system, through the
// synchronous access handles a dedicated Worker has) or `memoryBackend` (Node's tests). A backend
// opens a file by name; a file has size(), read(at, length), write(bytes, at), truncate(size),
// flush() and close(), all synchronous.
//
// `wasm` is the module wasm-bindgen made of sheltersim_wasm (any target): its Simulation,
// StoredRun, worldPartDigests and sha256Hex.

export const FORMAT = "sheltersim-browser-run-v1";
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

  close() {
    this.data.close();
    this.journal.close();
  }
}

/// Runs the exported world `exportBytes` (or the world `world` of a file of named worlds, as the
/// fixtures hold them) for its whole clock, `ticks` ticks per call to `advance`, and stores it in
/// `backend` as it goes. `progress(minute, duration)` is called after each call. Returns what the
/// Run gave and what storing it took.
export async function runAndStore({ wasm, exportBytes, world = null, backend, ticks = 1440, progress = () => {} }) {
  const writer = await RunWriter.create(wasm, backend);
  const simulation = world === null ? new wasm.Simulation(exportBytes) : wasm.Simulation.fromWorlds(exportBytes, world);
  try {
    if (!simulation.log_all_packed()) throw new Error("the exported world's log is not all packed");
    writer.line({ kind: "run", format: FORMAT, export_sha256: wasm.sha256Hex(exportBytes), export_bytes: exportBytes.length, world, duration_minutes: simulation.duration() });
    await writer.piece("export", exportBytes);
    let tickMs = 0;
    for (;;) {
      const started = performance.now();
      const ran = simulation.advance(ticks);
      tickMs += performance.now() - started;
      progress(simulation.minute(), simulation.duration());
      await writer.take(simulation, false);
      if (ran < ticks) break;
    }
    await writer.take(simulation, true);
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
    if (lines.length === 0 || lines[0].kind !== "run" || lines[0].format !== FORMAT) throw new Error("not a stored Run of this format");
    const reader = new wasm.StoredRun();
    let world = null;
    let exportSha = null;
    let end = null;
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
