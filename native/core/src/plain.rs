//! A host without Python (`PlainHost`) and a Run of an exported world on it (`PlainRun`): Issue
//! #244, PR 4-3, for the browser prototype of #132 (stage 4), which runs the ticks of a world Python
//! built and exported (route A).
//!
//! What it does where the native host calls Python (decision 4 of #244, the rest left to stage 6):
//! - The Python-parsed sections of the configs are the exported world's (`load::Sections`), and
//!   the floor plan is built from them with the world's overlay now, again when the overlay's
//!   facility moves or partitions change (`load::FloorPlans`).
//! - A contract (policy E15) gives its input back as given. What it was asked is kept by the
//!   model's name (`contracts`): where the pydantic model would have refused or changed the input,
//!   the Run may part from the native path's.
//! - The whole world's validation (policy E6) passes, and the end-of-tick check asks no Python
//!   validator (`engine::NoPython`).
//! - There is no LLM: the Run runs as one without (`Llm::default()`); no decision or trace goes out
//!   (`DecisionSink::Silent`, `TraceLevel::Off`), and no foreign handler runs.
//! - The records of the log are the world's (`load::find_record`). The log holds them in the binary
//!   form (`EventLog::pack_records`, `events::packed`, #252); their JSON is written when asked
//!   for (the digests, `find_record`), the same bytes as the native path's.
//! - The metric samples are kept packed (`MetricSamples`, #252), and their lines, which
//!   `engine::metric_line` writes, are written again when asked for; `metrics_digest` is the digest
//!   `tests/support/core_tick.py` writes of a Run's samples. Where the world has a resource ledger,
//!   each sample also holds its KPI columns then (`Balances`, #266), which the API's KPI history
//!   answers with (`history`).
//! - The core writes `world.resources` and the namespaces it starts itself (decision 2): the host
//!   is only told.
//! - A Run is saved at a minute and restored from there (`saved_point`, `restore`; #251, for the
//!   browser only): see `archive::SavedPoint`.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::rc::Rc;
use std::sync::Arc;

use serde_json::Value;

use sha2::{Digest, Sha256};

use crate::archive::{self, Restore, SavedPoint};
use crate::canonical::ListNode;
use crate::checkpoint::hex;
use crate::config::{Configs, Filled, SectionId};
use crate::engine::{self, StepError, Stepping};
use crate::error::{Error, Result};
use crate::events::packed::{put_varint, put_zigzag, Blocks, Cursor, Packer};
use crate::events::EventRow;
use crate::json::Encoder;
use crate::geometry::Geo;
use crate::host::{Contract, Decision, DecisionSink, Foreign, Holding, Host, Llm, Phase, Stage, Trace, TraceLevel, TxnKind, NO_FOREIGN};
use crate::load::{self, FloorPlans, Loaded, Sections};
use crate::rules::action_lifecycle::ActiveActions;
use crate::rules::common_operations::Logged;
use crate::rules::metrics::MetricSample;
use crate::state::resource_ledger::NAME as LEDGER;
use crate::value::V;
use crate::world::WorldCell;

/// The name a contract is kept by: the pydantic model's, or the operator's for an effect's
/// parameters.
fn contract_name(which: &Contract) -> String {
    match which {
        Contract::EffectParameters(operator) => format!("_PARAMETER_MODELS[{operator}]"),
        other => other.model().1.to_owned(),
    }
}

/// The host of a loaded world without Python.
pub struct PlainHost {
    loaded: Rc<Loaded>,
    sections: Sections,
    plans: FloorPlans,
    /// The metric samples, packed (`MetricSamples`).
    pub samples: MetricSamples,
    /// How many times each contract gave its input back, by the model's name.
    pub contracts: BTreeMap<String, u64>,
}

impl PlainHost {
    /// The host of `loaded`, with the sections of the exported entry it was loaded from.
    pub fn new(entry: &Value, loaded: Rc<Loaded>) -> Result<Self> {
        Ok(Self { loaded, sections: Sections::of_entry(entry)?, plans: FloorPlans::default(), samples: MetricSamples::default(), contracts: BTreeMap::new() })
    }

    /// How many times the floor plan was built.
    pub fn plan_builds(&self) -> usize {
        self.plans.builds()
    }

    /// The sha256 of the metric lines joined by `\n`, in hex (`metrics_digest` of
    /// `tests/support/core_tick.py`).
    pub fn metrics_digest(&self) -> String {
        self.samples.digest()
    }
}

/// The metric samples of a Run in a binary form (#252, PR 6-4): each `MetricSample` and its minute,
/// its fields packed as they are, in blocks (`Blocks`). A sample's line (`engine::metric_line`) is
/// written again from them, byte for byte; the digest of the lines is kept as they are taken, so
/// it needs no line written again. After 7 days of 100 people the lines are 52 MiB of JSON, most of
/// it the IDs behind the counts (`source_ids_json`), and 7.5 MiB packed.
///
/// A sample is: its minute (a zigzag varint), each field in the order of `MetricSample` (a count a
/// varint, `toilet_working_units` a zigzag varint, a float its 8 bytes), and `source_ids_json`:
/// 0 then its JSON packed as a value (`Packer::pack_value`), or 1 then its length and its bytes,
/// for a text the packed value would not write again.
///
/// A sample of a world with a resource ledger (#266) also holds the ledger's KPI columns then
/// (`Balances`, the native path's `resource_snapshots`): it starts with the byte 1 (`WITH_BALANCES`),
/// and the twelve columns follow its last field as zigzag varints. A minute is never negative, so
/// the first byte of a sample without them, its minute's, is even: samples stored before (format v2
/// of `run_store.mjs`) read as they did. A Run's samples all have them or none do, as the native
/// path's recorder requires.
#[derive(Clone, Debug, Default)]
pub struct MetricSamples {
    packer: Packer,
    blocks: Blocks,
    starts: Vec<u64>,
    digest: Sha256,
    /// Whether the samples so far hold the ledger's columns; None before the first.
    balances: Option<bool>,
}

/// The resource ledger's KPI columns of a sample (`resource_metric_values`,
/// `rules::resources::metric_values`): initial, delivered, served, losses, remaining and reserved,
/// of water and then of meals.
pub type Balances = [[i64; 6]; 2];

/// The first byte of a packed sample that holds `Balances`.
const WITH_BALANCES: u8 = 1;

impl MetricSamples {
    /// Takes a sample, with the ledger's columns where the world has a ledger: its line goes into
    /// the digest, and the sample is held packed. An error for a negative minute, and where the
    /// ledger came or went after the first sample (the native path's recorder refuses it too).
    pub fn push(&mut self, sample: &MetricSample, minute: i64, balances: Option<&Balances>) -> Result<()> {
        if minute < 0 {
            return Err(Error::Value("a sample's minute is negative".to_owned()));
        }
        match self.balances {
            Some(true) if balances.is_none() => return Err(Error::unsupported("the ledger was removed")),
            Some(false) if balances.is_some() => return Err(Error::unsupported("the ledger came late")),
            _ => {}
        }
        let line = engine::metric_line(sample, minute)?;
        let mut packed = Vec::with_capacity(256);
        if balances.is_some() {
            packed.push(WITH_BALANCES);
        }
        put_zigzag(&mut packed, minute);
        for count in [sample.toilet_queue_length, sample.toilet_active_users] {
            put_varint(&mut packed, count as u64);
        }
        put_zigzag(&mut packed, sample.toilet_working_units);
        for value in [sample.toilet_wait_count, sample.toilet_wait_total_minutes, sample.toilet_wait_mean_minutes, sample.toilet_wait_max_minutes, sample.stress_mean] {
            packed.extend_from_slice(&value.to_bits().to_le_bytes());
        }
        for count in [sample.stress_high_count, sample.staff_assigned_task_count, sample.staff_busy_count] {
            put_varint(&mut packed, count as u64);
        }
        packed.extend_from_slice(&sample.staff_load_ratio.to_bits().to_le_bytes());
        put_varint(&mut packed, sample.unresolved_support_request_count as u64);
        self.put_ids(&mut packed, &sample.source_ids_json)?;
        put_varint(&mut packed, sample.support_no_qualified_worker_count as u64);
        for value in [
            sample.support_no_qualified_worker_request_minutes,
            sample.support_fairness_promoted_assignment_count,
            sample.support_wait_staff_busy_request_minutes,
            sample.support_wait_staff_off_shift_request_minutes,
        ] {
            packed.extend_from_slice(&value.to_bits().to_le_bytes());
        }
        for value in balances.into_iter().flatten().flatten() {
            put_zigzag(&mut packed, *value);
        }
        if !self.starts.is_empty() {
            self.digest.update(b"\n");
        }
        self.digest.update(&line);
        self.starts.push(self.blocks.push(&packed));
        self.balances = Some(balances.is_some());
        Ok(())
    }

    /// `source_ids_json` packed as a value when its value writes the same text again.
    fn put_ids(&mut self, out: &mut Vec<u8>, text: &str) -> Result<()> {
        let value = serde_json::from_str::<Value>(text).ok().as_ref().and_then(V::from_json);
        if let Some(value) = value {
            let mut again = Encoder::with_capacity(text.len());
            if value.write_json(&mut again).is_ok() && again.out == text.as_bytes() {
                out.push(0);
                return self.packer.pack_value(out, &value);
            }
        }
        out.push(1);
        put_varint(out, text.len() as u64);
        out.extend_from_slice(text.as_bytes());
        Ok(())
    }

    /// How many samples were taken.
    pub fn len(&self) -> usize {
        self.starts.len()
    }

    pub fn is_empty(&self) -> bool {
        self.starts.is_empty()
    }

    /// The bytes the packed samples take.
    pub fn size(&self) -> usize {
        self.blocks.size()
    }

    /// The sample `index`, its minute and the ledger's columns it holds, or None past the last.
    pub fn sample(&self, index: usize) -> Result<Option<(MetricSample, i64, Option<Balances>)>> {
        let Some(&start) = self.starts.get(index) else {
            return Ok(None);
        };
        let unpacked = unpack_sample(&self.packer, self.blocks.at(start))?;
        Ok(Some((unpacked.sample, unpacked.minute, unpacked.balances)))
    }

    /// Seals the samples' blocks (`Blocks::seal`).
    pub fn seal(&mut self) {
        self.blocks.seal();
    }

    /// The packer and the blocks of the samples: what a Run in the browser stores (`archive`).
    pub fn store(&self) -> (&Packer, &Blocks) {
        (&self.packer, &self.blocks)
    }

    /// The line of the sample `index` (`engine::metric_line`), or None past the last.
    pub fn line(&self, index: usize) -> Result<Option<Vec<u8>>> {
        self.sample(index)?.map(|(sample, minute, _)| engine::metric_line(&sample, minute)).transpose()
    }

    /// Samples already packed against `packer` in `blocks`, each at its start in `starts`, with
    /// `digest` of their lines so far, and whether they hold the ledger's columns (a stored Run read
    /// back, `archive::Restore`, #251).
    pub(crate) fn adopt(packer: Packer, blocks: Blocks, starts: Vec<u64>, digest: Sha256, balances: Option<bool>) -> Self {
        Self { packer, blocks, starts, digest, balances }
    }

    /// The sha256 of the lines so far joined by `\n`, in hex.
    pub fn digest(&self) -> String {
        hex(&self.digest.clone().finalize())
    }
}

/// A sample read back (`unpack_sample`).
#[derive(Clone, Debug, PartialEq)]
pub struct UnpackedSample {
    pub sample: MetricSample,
    pub minute: i64,
    /// The ledger's columns, where the sample holds them.
    pub balances: Option<Balances>,
    /// How many bytes the sample took.
    pub used: usize,
}

/// The sample `MetricSamples` packed at the start of `bytes` against `packer`'s tables. An error,
/// never a panic, for bytes that are not such a sample.
pub fn unpack_sample(packer: &Packer, bytes: &[u8]) -> Result<UnpackedSample> {
    let bad = |what: &str| Error::Value(format!("not a packed sample: {what}"));
    let mut cursor = Cursor::new(bytes);
    let count = |cursor: &mut Cursor<'_>| -> Result<usize> { usize::try_from(cursor.varint()?).map_err(|_| bad("a count")) };
    let with_balances = match bytes.first() {
        Some(&WITH_BALANCES) => {
            cursor.skip(1)?;
            true
        }
        Some(first) if first & 1 == 1 => return Err(bad("its form")),
        _ => false,
    };
    let minute = cursor.zigzag()?;
    if minute < 0 {
        return Err(bad("its minute"));
    }
    let toilet_queue_length = count(&mut cursor)?;
    let toilet_active_users = count(&mut cursor)?;
    let toilet_working_units = cursor.zigzag()?;
    let toilet_wait_count = cursor.float()?;
    let toilet_wait_total_minutes = cursor.float()?;
    let toilet_wait_mean_minutes = cursor.float()?;
    let toilet_wait_max_minutes = cursor.float()?;
    let stress_mean = cursor.float()?;
    let stress_high_count = count(&mut cursor)?;
    let staff_assigned_task_count = count(&mut cursor)?;
    let staff_busy_count = count(&mut cursor)?;
    let staff_load_ratio = cursor.float()?;
    let unresolved_support_request_count = count(&mut cursor)?;
    let source_ids_json = match cursor.byte()? {
        0 => {
            let (value, used) = packer.unpack_value(cursor.rest())?;
            cursor.skip(used)?;
            let mut text = Encoder::with_capacity(1024);
            value.write_json(&mut text)?;
            String::from_utf8(text.out).map_err(|_| bad("its IDs"))?
        }
        1 => {
            let length = usize::try_from(cursor.varint()?).map_err(|_| bad("a length"))?;
            String::from_utf8(cursor.take(length)?.to_vec()).map_err(|_| bad("its IDs"))?
        }
        _ => return Err(bad("its IDs")),
    };
    let support_no_qualified_worker_count = count(&mut cursor)?;
    let sample = MetricSample {
        toilet_queue_length,
        toilet_active_users,
        toilet_working_units,
        toilet_wait_count,
        toilet_wait_total_minutes,
        toilet_wait_mean_minutes,
        toilet_wait_max_minutes,
        stress_mean,
        stress_high_count,
        staff_assigned_task_count,
        staff_busy_count,
        staff_load_ratio,
        unresolved_support_request_count,
        source_ids_json,
        support_no_qualified_worker_count,
        support_no_qualified_worker_request_minutes: cursor.float()?,
        support_fairness_promoted_assignment_count: cursor.float()?,
        support_wait_staff_busy_request_minutes: cursor.float()?,
        support_wait_staff_off_shift_request_minutes: cursor.float()?,
    };
    let balances = if with_balances {
        let mut balances: Balances = [[0; 6]; 2];
        for value in balances.iter_mut().flatten() {
            *value = cursor.zigzag()?;
        }
        Some(balances)
    } else {
        None
    };
    Ok(UnpackedSample { sample, minute, balances, used: cursor.position() })
}

impl Host for PlainHost {
    type Error = Error;

    fn validate_candidate(&mut self, _kind: TxnKind, _stage: Stage) -> Result<()> {
        Ok(())
    }

    fn validate_world(&mut self) -> Result<()> {
        Ok(())
    }

    fn validate_extensions(&mut self) -> Result<()> {
        Ok(())
    }

    fn find_record(&mut self, event_id: &str) -> Result<Option<Logged>> {
        self.loaded.cell.with(|world| load::find_record(world, event_id))?
    }

    fn floor_plan(&mut self) -> Result<Arc<Geo>> {
        let overlay = self.loaded.cell.with(|world| load::overlay(world))??;
        self.plans.plan(&self.sections, overlay.as_ref())
    }

    fn contract(&mut self, which: &Contract, input: &V) -> Result<V> {
        *self.contracts.entry(contract_name(which)).or_default() += 1;
        Ok(input.clone())
    }

    fn llm(&self) -> Result<Llm> {
        Ok(Llm::default())
    }

    fn prioritize_support_request(&mut self, _request_id: &str, _agent_id: &str, _observed_state_hash: &str) -> Result<()> {
        Ok(())
    }

    fn consider_support_request(&mut self, _agent_id: &str, _request_id: &str) -> Result<()> {
        Ok(())
    }

    fn consider_complaint_response(&mut self, _agent_id: &str, _request_id: &str) -> Result<()> {
        Ok(())
    }

    fn consider_cooperation(&mut self, _initiator: &str, _participant: &str, _trigger_event_id: &str) -> Result<()> {
        Ok(())
    }

    fn consider_information_reaction(&mut self, _agent_id: &str, _participant: &str, _message: &str, _trigger_event_id: &str) -> Result<()> {
        Ok(())
    }

    /// The sample, with the ledger's columns where the world has a ledger (the native path's
    /// `resource_snapshots`, #266), read as the native host reads them: after the sample, with the
    /// world as the sample saw it.
    fn capture_metric(&mut self, sample: &MetricSample, minute: i64) -> Result<()> {
        let balances = self.loaded.cell.with(|world| -> Result<Option<Balances>> {
            if Holding::of_world(world)?.is_absent(LEDGER).map_err(Error::unsupported)? {
                return Ok(None);
            }
            crate::rules::resources::metric_values(world).map(Some)
        })??;
        self.samples.push(sample, minute, balances.as_ref())
    }

    fn decision_sink(&self) -> Result<DecisionSink> {
        Ok(DecisionSink::Silent)
    }

    fn decision(&mut self, _decision: Decision) -> Result<()> {
        Ok(())
    }

    fn trace_level(&self) -> Result<TraceLevel> {
        Ok(TraceLevel::Off)
    }

    fn trace(&mut self, _document: Trace) -> Result<()> {
        Ok(())
    }

    fn phase(&mut self, _phase: Phase, _before: bool) -> Result<()> {
        Ok(())
    }

    fn space_refreshed(&mut self) -> Result<()> {
        Ok(())
    }

    fn fill_section(&mut self, configs: &Arc<Configs>, which: SectionId) -> Result<Filled> {
        if !self.sections.fill(configs, which)? {
            return Err(Error::unsupported(format!("the export has no section {}", which.name())));
        }
        Ok(Filled::Kept)
    }

    fn apply_foreign(&mut self, _event: &EventRow) -> Result<Foreign> {
        Err(Error::unsupported(NO_FOREIGN))
    }
}

/// A Run of an exported world on a `PlainHost`: the ticks one at a time (`step`), the world's
/// digests and the samples' after any of them.
pub struct PlainRun {
    pub loaded: Rc<Loaded>,
    pub host: PlainHost,
    pub stepping: Stepping,
    /// The world document's `rng_state`, which the core's World does not hold: what the world's
    /// digest reads (`frame`).
    pub rng_state: V,
    /// The node of the log's records the last digest read (`canonical::state_digest_with`, #266):
    /// the next digest reads only the records added since. Built at the first digest.
    log_node: RefCell<ListNode>,
}

impl PlainRun {
    /// The Run of an exported entry (`export_world`'s document).
    pub fn of_entry(entry: &Value) -> Result<Self> {
        let loaded = Rc::new(load::loaded(entry)?);
        // The log holds its records in the binary form (#252): about a seventh of their JSON.
        loaded.cell.with(|world| world.log_mut().and_then(|mut log| log.pack_records()))??;
        let host = PlainHost::new(entry, Rc::clone(&loaded))?;
        let rng_state = load::value(&entry["world"]["rng_state"])?;
        Ok(Self { loaded, host, stepping: Stepping::default(), rng_state, log_node: RefCell::default() })
    }

    /// `of_entry` of the entry's JSON text.
    pub fn of_json(raw: &[u8]) -> Result<Self> {
        Self::of_entry(&Self::entry_of_json(raw)?)
    }

    /// `of_entry` of the world `name` of a file of named worlds (`{"worlds": {name: entry}}`, as
    /// `benchmarks/core_fixtures.py` writes `native/core/tests/data/`), from its JSON text: the
    /// entry is read from the file's own bytes, its numbers as written (a JSON reader of another
    /// language may write `1.0` back as `1`).
    pub fn of_worlds_json(raw: &[u8], name: &str) -> Result<Self> {
        Self::of_entry(&Self::entry_of_worlds_json(raw, name)?)
    }

    /// The world's minute: the next tick's.
    pub fn minute(&self) -> Result<i64> {
        self.loaded.cell.with(|world| world.minute_index())?
    }

    /// The world's duration: its clock is complete at this minute.
    pub fn duration(&self) -> Result<i64> {
        self.loaded.cell.with(|world| world.duration_minutes())?
    }

    /// One tick (`engine::step`): the minute it ran at.
    pub fn step(&mut self) -> std::result::Result<i64, StepError<Error>> {
        engine::step(&self.loaded, &mut self.host, &mut self.stepping)
    }

    /// The digest of each part of the world's canonical document (`canonical::digests`).
    pub fn digests(&self) -> Result<Vec<(&'static str, String)>> {
        self.loaded.cell.with(|world| crate::canonical::digests(world))?
    }

    /// The frame of the world now (`frame::frame`, #264): its canonical JSON, what the API's
    /// timeline gives for this minute. It changes nothing.
    /// Without the request's `state_hash`, the frame's is `state_digest`.
    pub fn frame(&mut self, request: &crate::frame::Request<'_>) -> Result<Vec<u8>> {
        let digest = match request.state_hash {
            Some(_) => None,
            None => Some(self.state_digest()?),
        };
        let request = crate::frame::Request { state_hash: request.state_hash.or(digest.as_deref()), ..*request };
        crate::frame::frame(&self.loaded.cell, &mut self.host, &request, &self.rng_state)
    }

    /// The summary of the world now that the reading of an intervention's text is given
    /// (`summary::scenario_summary`, #302): its canonical JSON. It changes nothing.
    pub fn scenario_summary(&self) -> Result<Vec<u8>> {
        self.loaded.cell.with(|world| crate::summary::scenario_summary(world))?
    }

    /// `world.state_digest()` (`canonical::state_digest`) of the world now. The log's node is kept
    /// between calls, so a call reads only the records added since the last
    /// (`canonical::state_digest_with`).
    pub fn state_digest(&self) -> Result<String> {
        let mut log = self.log_node.try_borrow_mut().map_err(|_| Error::unsupported("the log's node is borrowed"))?;
        self.loaded.cell.with(|world| crate::canonical::state_digest_with(world, &self.rng_state, &mut log))?
    }

    /// Seals the blocks of the packed log and of the samples (`Blocks::seal`), between two ticks:
    /// the records and samples taken so far are then all in sealed blocks, which a Run in the
    /// browser stores before a saved point, so that the point can be restored from what is stored
    /// up to it (#267). The records, the samples and every digest stay as they were.
    pub fn seal_stores(&mut self) -> Result<()> {
        self.loaded.cell.with(|world| world.log_mut().map(|mut log| log.seal_packed()))??;
        self.host.samples.seal();
        Ok(())
    }

    /// The Run now as a saved point (`archive::SavedPoint`, #251), between two ticks.
    pub fn saved_point(&self) -> Result<SavedPoint> {
        let (world, records) = self.loaded.cell.with(|world| -> Result<_> { Ok((archive::world_parts(world)?, world.log()?.len() as u64)) })??;
        Ok(SavedPoint { minute: self.minute()?, records, samples: self.host.samples.len() as u64, contracts: self.host.contracts.clone(), world })
    }

    /// The Run at a saved point (#251): its world, the log and the samples `restore` took, and
    /// the exported `entry` the Run started from, whose controller (`run`), slots and sections it
    /// runs with (its world is not read). It goes on as the Run went on from the saved point. An
    /// error where `restore` holds fewer records or samples than the saved point, or the world is
    /// not of the saved point's minute.
    pub fn restore(mut entry: Value, restore: Restore) -> Result<Self> {
        let (point, log, samples) = restore.into_parts()?;
        // The world document's `rng_state` is the Run's from its start (the algorithm and the seed).
        let rng_state = load::value(&entry["world"]["rng_state"])?;
        let entry_object = entry.as_object_mut().ok_or_else(|| Error::Value("not an exported world: the entry is not a dict".to_owned()))?;
        entry_object.insert("world".to_owned(), Value::Object(archive::world_document(&point.world)?));
        let loaded = Rc::new(load::loaded(&entry)?);
        loaded.cell.with(|world| -> Result<()> {
            if world.minute_index()? != point.minute {
                return Err(Error::Value("not a saved point: its world is of another minute".to_owned()));
            }
            *world.log_mut()? = log;
            Ok(())
        })??;
        let actions = loaded.cell.with(|world| world.log().and_then(|log| ActiveActions::of_log(&log)))??;
        loaded.run.try_borrow_mut().map_err(|_| Error::unsupported("the Run is borrowed"))?.actions = actions;
        let mut host = PlainHost::new(&entry, Rc::clone(&loaded))?;
        host.samples = samples;
        host.contracts = point.contracts;
        Ok(Self { loaded, host, stepping: Stepping::default(), rng_state, log_node: RefCell::default() })
    }

    /// The exported entry of `of_json`'s JSON text, for `restore`.
    pub fn entry_of_json(raw: &[u8]) -> Result<Value> {
        serde_json::from_slice(raw).map_err(|error| Error::Value(format!("not an exported world: {error}")))
    }

    /// The entry `name` of `of_worlds_json`'s file of named worlds, for `restore`.
    pub fn entry_of_worlds_json(raw: &[u8], name: &str) -> Result<Value> {
        let mut file: Value = serde_json::from_slice(raw).map_err(|error| Error::Value(format!("not a file of worlds: {error}")))?;
        let entry = file.get_mut("worlds").and_then(|worlds| worlds.get_mut(name)).ok_or_else(|| Error::Value(format!("the file has no world {name}")))?;
        Ok(entry.take())
    }
}
