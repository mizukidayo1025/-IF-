//! The core of 避難所IF！ for the browser (Issue #244, PR 4-4; stage 4 of #132): a Run of a world
//! Python built and exported (`sheltersim.core.world_export.export_world`, route A), run on the
//! host without Python (`sheltersim_core::plain`), behind a JS boundary made with wasm-bindgen
//! (decision 8). It runs on one thread (decision 5: the core without its feature `parallel`).
//!
//! `Simulation` reads the export's JSON bytes, runs the ticks (`advance`) and gives the digests of
//! the world's parts as `tests/support/core_tick.py` writes them (`digests_json`) and of the
//! metric samples (`metrics_digest`): what the Node test (`tests/node_digests.mjs`) and the
//! measuring page of PR 4-5 compare with the native path's.
//!
//! It also hands the page what a Run stores (#252, PR 6-5; `sheltersim_core::archive`): the packed
//! log's and samples' table entries and sealed blocks, and the world without its log; `StoredRun`
//! reads them back to the Run's digests. The page's side is `js/run_store.mjs` and
//! `js/run_worker.mjs`. A Run is saved at a minute (`Simulation.saved_point`) and restored from
//! there with its stored log and samples (`Restore`, `Simulation.restore`; #251).
//!
//! A minute crosses the boundary as an `f64` (a JS number), not an `i64` (a `BigInt`): a world's
//! minutes are far below 2^53.

use sheltersim_core::events::packed::{Blocks, Packer};
use sheltersim_core::{archive, canonical, checkpoint, frame, history};
use sheltersim_core::engine::StepError;
use sheltersim_core::error::Error;
use sheltersim_core::json::Encoder;
use sheltersim_core::plain::PlainRun;
use sheltersim_core::world::WorldCell;
use wasm_bindgen::prelude::*;

fn js_error(error: Error) -> JsError {
    JsError::new(&error.to_string())
}

/// A Run of an exported world.
#[wasm_bindgen]
pub struct Simulation {
    run: PlainRun,
}

#[wasm_bindgen]
impl Simulation {
    /// The Run of an exported entry, from its JSON bytes. An error for bytes that are not an
    /// exported world.
    #[wasm_bindgen(constructor)]
    pub fn new(entry: &[u8]) -> Result<Simulation, JsError> {
        Ok(Simulation { run: PlainRun::of_json(entry).map_err(js_error)? })
    }

    /// The Run of the world `name` of a file of named worlds (`{"worlds": {name: entry}}`, as the
    /// fixtures of `native/core/tests/data/` hold them), from the file's JSON bytes.
    #[wasm_bindgen(js_name = fromWorlds)]
    pub fn from_worlds(file: &[u8], name: &str) -> Result<Simulation, JsError> {
        Ok(Simulation { run: PlainRun::of_worlds_json(file, name).map_err(js_error)? })
    }

    /// The world's minute: the next tick's.
    pub fn minute(&self) -> Result<f64, JsError> {
        Ok(self.run.minute().map_err(js_error)? as f64)
    }

    /// The minute the world's clock is complete at.
    pub fn duration(&self) -> Result<f64, JsError> {
        Ok(self.run.duration().map_err(js_error)? as f64)
    }

    /// Run up to `ticks` ticks, stopping where the clock is complete: how many ran. An error is a
    /// tick's or the end-of-tick check's failure, with its minute.
    pub fn advance(&mut self, ticks: u32) -> Result<u32, JsError> {
        let mut ran = 0;
        while ran < ticks {
            match self.run.step() {
                Ok(_) => ran += 1,
                Err(StepError::Complete) => break,
                Err(StepError::Tick { minute, label, error }) => {
                    return Err(JsError::new(&format!("minute {minute}: {}: {error}", label.unwrap_or("the tick"))));
                }
                Err(StepError::Check { minute, error }) => {
                    return Err(JsError::new(&format!("minute {minute}: the end-of-tick check: {error}")));
                }
            }
        }
        Ok(ran)
    }

    /// The digests of the world's parts, as a JSON object in the order of `core_tick.py`'s
    /// `world_digests` (`canonical::run_digests`).
    pub fn digests_json(&self) -> Result<String, JsError> {
        let digests = self.run.loaded.cell.with(|world| canonical::run_digests(world)).and_then(|digests| digests).map_err(js_error)?;
        let mut encoder = Encoder::with_capacity(1024);
        encoder.out.push(b'{');
        for (index, (part, digest)) in digests.iter().enumerate() {
            if index > 0 {
                encoder.out.push(b',');
            }
            encoder.string(part);
            encoder.out.push(b':');
            encoder.string(digest);
        }
        encoder.out.push(b'}');
        String::from_utf8(encoder.out).map_err(|error| JsError::new(&error.to_string()))
    }

    /// The digest of each part of the world's canonical document (`canonical::digests`), as a JSON
    /// object in the order of the parts: what a stored Run's world piece and log read back to.
    pub fn part_digests_json(&self) -> Result<String, JsError> {
        let digests = self.run.digests().map_err(js_error)?;
        let mut encoder = Encoder::with_capacity(2048);
        encoder.out.push(b'{');
        for (index, (part, digest)) in digests.iter().enumerate() {
            if index > 0 {
                encoder.out.push(b',');
            }
            encoder.string(part);
            encoder.out.push(b':');
            encoder.string(digest);
        }
        encoder.out.push(b'}');
        String::from_utf8(encoder.out).map_err(|error| JsError::new(&error.to_string()))
    }

    /// The digest of the metric samples so far (`core_tick.py`'s `metrics_digest`).
    pub fn metrics_digest(&self) -> String {
        self.run.host.metrics_digest()
    }

    /// How many metric samples the Run took.
    pub fn metric_count(&self) -> u32 {
        self.run.host.samples.len() as u32
    }

    /// The metric sample `index` as its JSON list (`MetricSnapshot`'s fields, the minute first),
    /// written again from its packed form; None past the last sample.
    pub fn metric_line(&self, index: u32) -> Result<Option<String>, JsError> {
        let line = self.run.host.samples.line(index as usize).map_err(js_error)?;
        line.map(|line| String::from_utf8(line).map_err(|error| JsError::new(&error.to_string()))).transpose()
    }

    /// How many times each contract gave its input back (`PlainHost::contracts`), as a JSON
    /// object: where the Run may part from the native path's (decision 4).
    pub fn contracts_json(&self) -> String {
        let mut encoder = Encoder::with_capacity(256);
        encoder.out.push(b'{');
        for (index, (name, count)) in self.run.host.contracts.iter().enumerate() {
            if index > 0 {
                encoder.out.push(b',');
            }
            encoder.string(name);
            encoder.out.push(b':');
            encoder.int(*count as i64);
        }
        encoder.out.push(b'}');
        String::from_utf8(encoder.out).unwrap_or_default()
    }
}

/// The frame of a minute (#264): the map and the indicators the API's `timeline` gives, made by the
/// core (`sheltersim_core::frame`), byte for byte the native path's.
#[wasm_bindgen]
impl Simulation {
    /// The frame of the world now, as its canonical JSON bytes (a `TimelineFrame`): `computedUntil`
    /// is how far the branch is computed, and `stateHash` the world's digest where the caller holds
    /// it (a saved world's); without it the frame's is the world's (`stateDigest`).
    pub fn frame(&mut self, run_id: &str, branch_id: &str, computed_until: f64, state_hash: Option<String>) -> Result<Vec<u8>, JsError> {
        let request = frame::Request { run_id, branch_id, computed_until: minute_of(computed_until)?, state_hash: state_hash.as_deref() };
        self.run.frame(&request).map_err(js_error)
    }

    /// `world.state_digest()` of the world now: the `state_hash` of a frame without one. The
    /// Simulation keeps the node of its log between calls (#266), so a call after `advance` reads
    /// only the records added since; the first call (after `new` or a restore) reads them all.
    #[wasm_bindgen(js_name = stateDigest)]
    pub fn state_digest(&self) -> Result<String, JsError> {
        self.run.state_digest().map_err(js_error)
    }
}

/// The summary of the world that the reading of an intervention's text is given (#302, under #292).
#[wasm_bindgen]
impl Simulation {
    /// The summary of the world now (`ScenarioSummary`, `sheltersim_core::summary`), as its
    /// canonical JSON bytes: those of `scenario_summary_from_world(world).model_dump(mode="json")`
    /// that the server's `intervention-preview` reads. It changes nothing.
    #[wasm_bindgen(js_name = scenarioSummary)]
    pub fn scenario_summary(&self) -> Result<Vec<u8>, JsError> {
        self.run.scenario_summary().map_err(js_error)
    }
}

/// A minute given as a JS number: a whole number of minutes, not negative.
fn minute_of(minute: f64) -> Result<i64, JsError> {
    if minute.fract() != 0.0 || !(0.0..=9_007_199_254_740_991.0).contains(&minute) {
        return Err(JsError::new("a minute is a whole number of minutes, not negative"));
    }
    Ok(minute as i64)
}

/// The pieces a Run stores (#252, PR 6-5; `sheltersim_core::archive`): the packed log's and the
/// samples' tables and blocks, and the world at its minute without its log. The page takes, after
/// each `advance`, the tables' new entries and the newly sealed blocks, and every block at the end.
#[wasm_bindgen]
impl Simulation {
    /// The log's string and shape tables' sizes: `[strings, shapes]`.
    pub fn log_table_counts(&self) -> Result<Vec<u32>, JsError> {
        self.with_log_store(|packer, _| vec![packer.strings().len() as u32, packer.shapes().len() as u32])
    }

    /// The log's tables' entries from `strings` and `shapes` on (`Packer::write_tables`).
    pub fn log_tables(&self, strings: u32, shapes: u32) -> Result<Vec<u8>, JsError> {
        self.with_log_store(|packer, _| {
            let mut out = Vec::new();
            packer.write_tables(strings as usize, shapes as usize, &mut out);
            out
        })
    }

    /// How many of the log's blocks there are, and how many of them are sealed: `[blocks, sealed]`.
    pub fn log_block_counts(&self) -> Result<Vec<u32>, JsError> {
        self.with_log_store(|_, blocks| vec![blocks.blocks().len() as u32, blocks.sealed() as u32])
    }

    /// A copy of the log's block `index` (empty past the last).
    pub fn log_block(&self, index: u32) -> Result<Vec<u8>, JsError> {
        self.with_log_store(|_, blocks| blocks.blocks().get(index as usize).cloned().unwrap_or_default())
    }

    /// How many records the log holds.
    pub fn log_records(&self) -> Result<f64, JsError> {
        self.run.loaded.cell.with(|world| world.log().map(|log| log.len() as f64)).and_then(|count| count).map_err(js_error)
    }

    /// Whether every record of the log is in its blocks: none kept its bytes instead, so the stored
    /// blocks hold the whole log.
    pub fn log_all_packed(&self) -> Result<bool, JsError> {
        self.run.loaded.cell.with(|world| world.log().map(|log| log.all_packed())).and_then(|packed| packed).map_err(js_error)
    }

    /// The samples' string and shape tables' sizes: `[strings, shapes]`.
    pub fn sample_table_counts(&self) -> Vec<u32> {
        let (packer, _) = self.run.host.samples.store();
        vec![packer.strings().len() as u32, packer.shapes().len() as u32]
    }

    /// The samples' tables' entries from `strings` and `shapes` on.
    pub fn sample_tables(&self, strings: u32, shapes: u32) -> Vec<u8> {
        let (packer, _) = self.run.host.samples.store();
        let mut out = Vec::new();
        packer.write_tables(strings as usize, shapes as usize, &mut out);
        out
    }

    /// How many of the samples' blocks there are, and how many are sealed: `[blocks, sealed]`.
    pub fn sample_block_counts(&self) -> Vec<u32> {
        let (_, blocks) = self.run.host.samples.store();
        vec![blocks.blocks().len() as u32, blocks.sealed() as u32]
    }

    /// A copy of the samples' block `index` (empty past the last).
    pub fn sample_block(&self, index: u32) -> Vec<u8> {
        let (_, blocks) = self.run.host.samples.store();
        blocks.blocks().get(index as usize).cloned().unwrap_or_default()
    }

    /// The world now without its log (`archive::world_parts`).
    pub fn world_parts(&self) -> Result<Vec<u8>, JsError> {
        self.run.loaded.cell.with(|world| archive::world_parts(world)).and_then(|parts| parts).map_err(js_error)
    }
}

impl Simulation {
    fn with_log_store<R>(&self, f: impl FnOnce(&Packer, &Blocks) -> R) -> Result<R, JsError> {
        self.run
            .loaded
            .cell
            .with(|world| -> Result<R, Error> {
                let log = world.log()?;
                let (packer, blocks) = log.packed_store().ok_or_else(|| Error::unsupported("the log is not packed"))?;
                Ok(f(packer, blocks))
            })
            .and_then(|result| result)
            .map_err(js_error)
    }
}

/// Saved points (#251; `sheltersim_core::archive::SavedPoint`): the Run now as a saved point's
/// bytes, and the Run at a saved point, restored with the stored log and samples (`Restore`). The
/// page's side is `RunWriter.save` and `RunReader.seek` of `js/run_store.mjs`.
#[wasm_bindgen]
impl Simulation {
    /// The Run now as a saved point's bytes (`archive::SavedPoint`): call it between calls to
    /// `advance`.
    pub fn saved_point(&self) -> Result<Vec<u8>, JsError> {
        Ok(self.run.saved_point().map_err(js_error)?.to_bytes())
    }

    /// Seals the log's and the samples' blocks (`PlainRun::seal_stores`): call it between calls to
    /// `advance`, before a saved point a stored Run goes on from (#267). The blocks there are then
    /// all sealed, so the page stores them before the point.
    pub fn seal_blocks(&mut self) -> Result<(), JsError> {
        self.run.seal_stores().map_err(js_error)
    }

    /// The Run at the saved point `restore` was made of, which has read the stored log and samples
    /// up to its counts, with the exported entry the Run started from (its JSON bytes). `restore`
    /// is used up: it cannot restore again.
    pub fn restore(entry: &[u8], restore: &mut Restore) -> Result<Simulation, JsError> {
        let entry = PlainRun::entry_of_json(entry).map_err(js_error)?;
        Ok(Simulation { run: PlainRun::restore(entry, restore.take()?).map_err(js_error)? })
    }

    /// `restore` with the world `name` of a file of named worlds as the exported entry.
    #[wasm_bindgen(js_name = restoreFromWorlds)]
    pub fn restore_from_worlds(file: &[u8], name: &str, restore: &mut Restore) -> Result<Simulation, JsError> {
        let entry = PlainRun::entry_of_worlds_json(file, name).map_err(js_error)?;
        Ok(Simulation { run: PlainRun::restore(entry, restore.take()?).map_err(js_error)? })
    }
}

/// A saved point being restored (`archive::Restore`): made of the saved point's bytes, then given
/// the stored log's and samples' tables and blocks in the order they were written until it needs
/// no more (`log_needed`, `samples_needed`), then used by `Simulation.restore`.
#[wasm_bindgen]
pub struct Restore {
    inner: Option<archive::Restore>,
}

#[wasm_bindgen]
impl Restore {
    /// An error for bytes that are not a saved point of the version this core reads.
    #[wasm_bindgen(constructor)]
    pub fn new(point: &[u8]) -> Result<Restore, JsError> {
        Ok(Restore { inner: Some(archive::Restore::new(point).map_err(js_error)?) })
    }

    /// The saved point's minute.
    pub fn minute(&self) -> Result<f64, JsError> {
        Ok(self.inner()?.point.minute as f64)
    }

    /// How many records the log held at the saved point.
    pub fn records(&self) -> Result<f64, JsError> {
        Ok(self.inner()?.point.records as f64)
    }

    /// How many metric samples the Run had taken at the saved point.
    pub fn samples(&self) -> Result<f64, JsError> {
        Ok(self.inner()?.point.samples as f64)
    }

    pub fn log_needed(&self) -> Result<bool, JsError> {
        Ok(self.inner()?.log_needed())
    }

    pub fn samples_needed(&self) -> Result<bool, JsError> {
        Ok(self.inner()?.samples_needed())
    }

    pub fn read_log_tables(&mut self, bytes: &[u8]) -> Result<(), JsError> {
        self.inner_mut()?.read_log_tables(bytes).map_err(js_error)
    }

    /// How many of the block's records were taken.
    pub fn read_log_block(&mut self, bytes: &[u8]) -> Result<u32, JsError> {
        self.inner_mut()?.read_log_block(bytes).map(|taken| taken as u32).map_err(js_error)
    }

    pub fn read_sample_tables(&mut self, bytes: &[u8]) -> Result<(), JsError> {
        self.inner_mut()?.read_sample_tables(bytes).map_err(js_error)
    }

    /// How many of the block's samples were taken.
    pub fn read_sample_block(&mut self, bytes: &[u8]) -> Result<u32, JsError> {
        self.inner_mut()?.read_sample_block(bytes).map(|taken| taken as u32).map_err(js_error)
    }
}

impl Restore {
    fn inner(&self) -> Result<&archive::Restore, JsError> {
        self.inner.as_ref().ok_or_else(|| JsError::new("the saved point was restored already"))
    }

    fn inner_mut(&mut self) -> Result<&mut archive::Restore, JsError> {
        self.inner.as_mut().ok_or_else(|| JsError::new("the saved point was restored already"))
    }

    fn take(&mut self) -> Result<archive::Restore, JsError> {
        self.inner.take().ok_or_else(|| JsError::new("the saved point was restored already"))
    }
}

/// The version of the saved points this core writes and reads (`archive::SAVED_POINT_VERSION`).
#[wasm_bindgen(js_name = savedPointVersion)]
pub fn saved_point_version() -> f64 {
    archive::SAVED_POINT_VERSION as f64
}

/// A stored Run read back (`archive::LogReader`, `SampleReader`): the pieces in the order they were
/// written, and the digests they give.
#[wasm_bindgen]
pub struct StoredRun {
    log: archive::LogReader,
    samples: archive::SampleReader,
}

#[wasm_bindgen]
impl StoredRun {
    #[wasm_bindgen(constructor)]
    pub fn new() -> StoredRun {
        StoredRun { log: archive::LogReader::new(), samples: archive::SampleReader::new() }
    }

    pub fn read_log_tables(&mut self, bytes: &[u8]) -> Result<(), JsError> {
        self.log.read_tables(bytes).map_err(js_error)
    }

    /// How many records the block held.
    pub fn read_log_block(&mut self, bytes: &[u8]) -> Result<u32, JsError> {
        self.log.read_block(bytes).map(|read| read as u32).map_err(js_error)
    }

    pub fn read_sample_tables(&mut self, bytes: &[u8]) -> Result<(), JsError> {
        self.samples.read_tables(bytes).map_err(js_error)
    }

    /// How many samples the block held.
    pub fn read_sample_block(&mut self, bytes: &[u8]) -> Result<u32, JsError> {
        self.samples.read_block(bytes).map(|read| read as u32).map_err(js_error)
    }

    pub fn log_records(&self) -> f64 {
        self.log.records() as f64
    }

    /// The digest of the `event_log` part of the records read.
    pub fn log_digest(&self) -> String {
        self.log.digest()
    }

    pub fn sample_count(&self) -> f64 {
        self.samples.samples() as f64
    }

    /// The digest of the metric lines of the samples read.
    pub fn metrics_digest(&self) -> String {
        self.samples.digest()
    }
}

impl Default for StoredRun {
    fn default() -> Self {
        Self::new()
    }
}

/// The digest of each part a world piece holds (`archive::world_part_digests`), as a JSON object.
#[wasm_bindgen(js_name = worldPartDigests)]
pub fn world_part_digests(bytes: &[u8]) -> Result<String, JsError> {
    let digests = archive::world_part_digests(bytes).map_err(js_error)?;
    let mut encoder = Encoder::with_capacity(2048);
    encoder.out.push(b'{');
    for (index, (part, digest)) in digests.iter().enumerate() {
        if index > 0 {
            encoder.out.push(b',');
        }
        encoder.string(part);
        encoder.out.push(b':');
        encoder.string(digest);
    }
    encoder.out.push(b'}');
    String::from_utf8(encoder.out).map_err(|error| JsError::new(&error.to_string()))
}

/// The entry of a Scenario's world at minute 0, from the Scenario's canonical JSON
/// (`sheltersim_core::initial::entry`, #249): the bytes Python's `export_world` writes of it, which
/// `new Simulation(entry)` runs. What the core does not build (a Scenario whose configs its table
/// lacks, and the like) is an error whose message starts with `unsupported: `; any other error is
/// input that is not a Scenario.
#[wasm_bindgen(js_name = initialEntry)]
pub fn initial_entry(scenario: &[u8]) -> Result<Vec<u8>, JsError> {
    sheltersim_core::initial::entry(scenario).map_err(|error| match error {
        Error::Unsupported(reason) => JsError::new(&format!("unsupported: {reason}")),
        other => js_error(other),
    })
}

/// `hashlib.sha256(bytes).hexdigest()`: what the page's journal checks its lines and pieces by.
#[wasm_bindgen(js_name = sha256Hex)]
pub fn sha256_hex(bytes: &[u8]) -> String {
    checkpoint::sha256_hex(bytes)
}

/// The size of the WASM memory now, in bytes (`memory.buffer.byteLength`): it only grows, so read
/// after a Run it is the Run's peak. 0 off wasm32.
#[wasm_bindgen(js_name = memoryBytes)]
pub fn memory_bytes() -> f64 {
    #[cfg(target_arch = "wasm32")]
    {
        (core::arch::wasm32::memory_size(0) * 65536) as f64
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        0.0
    }
}

/// What the API's KPI history and offline report read (#266; `sheltersim_core::history`): a
/// stored Run's metric samples read back and paged as the history index pages them, and, of a
/// Run, its records and its samples' final metrics. The page's side is `js/api_answers.mjs`.
#[wasm_bindgen]
pub struct KpiHistory {
    table: history::KpiTable,
}

#[wasm_bindgen]
impl KpiHistory {
    #[wasm_bindgen(constructor)]
    pub fn new() -> KpiHistory {
        KpiHistory { table: history::KpiTable::new() }
    }

    pub fn read_sample_tables(&mut self, bytes: &[u8]) -> Result<(), JsError> {
        self.table.read_tables(bytes).map_err(js_error)
    }

    /// How many samples the block held.
    pub fn read_sample_block(&mut self, bytes: &[u8]) -> Result<u32, JsError> {
        self.table.read_block(bytes).map(|read| read as u32).map_err(js_error)
    }

    /// How many samples were read.
    pub fn sample_count(&self) -> f64 {
        self.table.len() as f64
    }

    /// The samples from `start` to `end`: `[count, first minute, last minute]`, or `[0]`.
    pub fn coverage(&self, start: f64, end: f64) -> Result<Vec<f64>, JsError> {
        Ok(match self.table.coverage(minute_of(start)?, minute_of(end)?) {
            Some((count, first, last)) => vec![count as f64, first as f64, last as f64],
            None => vec![0.0],
        })
    }

    /// A page of rows (`history::KpiQuery`): `afterMinute` and `afterKey` are the cursor's row
    /// (both or neither).
    #[allow(clippy::too_many_arguments)]
    pub fn page(
        &self,
        start: f64,
        end: f64,
        step: f64,
        limit: u32,
        after_minute: Option<f64>,
        after_key: Option<String>,
        filtered: bool,
        run_id: &str,
        branch_id: &str,
    ) -> Result<KpiPage, JsError> {
        let after = match (after_minute, after_key.as_deref()) {
            (Some(minute), Some(key)) => Some((minute as i64, key)),
            (None, None) => None,
            _ => return Err(JsError::new("a cursor's row has a minute and a key")),
        };
        let query = history::KpiQuery { start: minute_of(start)?, end: minute_of(end)?, step: minute_of(step)?, limit: limit as usize, after, filtered, run_id, branch_id };
        let page = self.table.page(&query).map_err(js_error)?;
        Ok(KpiPage { items: String::from_utf8(page.items).map_err(|error| JsError::new(&error.to_string()))?, last: page.last, more: page.more })
    }
}

impl Default for KpiHistory {
    fn default() -> Self {
        Self::new()
    }
}

/// A page of KPI rows (`KpiHistory.page`).
#[wasm_bindgen]
pub struct KpiPage {
    items: String,
    last: Option<i64>,
    more: bool,
}

#[wasm_bindgen]
impl KpiPage {
    /// The rows, as a JSON list (canonical JSON, the floats as Python writes them).
    pub fn items(&self) -> String {
        self.items.clone()
    }

    /// The minute of the last row, or None for no rows.
    pub fn last(&self) -> Option<f64> {
        self.last.map(|minute| minute as f64)
    }

    /// Whether rows follow the page.
    pub fn more(&self) -> bool {
        self.more
    }
}

#[wasm_bindgen]
impl Simulation {
    /// The record `index` of the log, as its canonical JSON (`EventRecord.to_dict()`), or None past
    /// the last.
    pub fn log_record(&self, index: f64) -> Result<Option<String>, JsError> {
        let mut out = Vec::new();
        let written = self
            .run
            .loaded
            .cell
            .with(|world| world.log().and_then(|log| if index >= 0.0 && index < log.len() as f64 { log.write_record(index as usize, &mut out) } else { Ok(false) }))
            .and_then(|written| written)
            .map_err(js_error)?;
        if !written {
            return Ok(None);
        }
        String::from_utf8(out).map(Some).map_err(|error| JsError::new(&error.to_string()))
    }

    /// The final metrics of the offline report's evidence (`history::review_metrics`) of the
    /// sample `index`, as canonical JSON, or None past the last sample.
    pub fn review_metrics(&self, index: u32) -> Result<Option<String>, JsError> {
        let Some((sample, _, _)) = self.run.host.samples.sample(index as usize).map_err(js_error)? else {
            return Ok(None);
        };
        let metrics = history::review_metrics(&sample).map_err(js_error)?;
        String::from_utf8(metrics).map(Some).map_err(|error| JsError::new(&error.to_string()))
    }
}
