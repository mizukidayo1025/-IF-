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
//! A minute crosses the boundary as an `f64` (a JS number), not an `i64` (a `BigInt`): a world's
//! minutes are far below 2^53.

use sheltersim_core::canonical;
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

    /// The digest of the metric samples so far (`core_tick.py`'s `metrics_digest`).
    pub fn metrics_digest(&self) -> String {
        self.run.host.metrics_digest()
    }

    /// How many metric samples the Run took.
    pub fn metric_count(&self) -> u32 {
        self.run.host.metric_lines.len() as u32
    }

    /// The metric sample `index` as its JSON list (`MetricSnapshot`'s fields, the minute first).
    pub fn metric_line(&self, index: u32) -> Option<String> {
        let line = self.run.host.metric_lines.get(index as usize)?;
        String::from_utf8(line.clone()).ok()
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
