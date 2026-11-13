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
//! - The metric samples are kept as the lines `engine::metric_line` writes; `metrics_digest` is the
//!   digest `tests/support/core_tick.py` writes of a Run's samples.
//! - The core writes `world.resources` and the namespaces it starts itself (decision 2): the host
//!   is only told.

use std::collections::BTreeMap;
use std::rc::Rc;
use std::sync::Arc;

use serde_json::Value;

use crate::checkpoint::sha256_hex;
use crate::config::{Configs, Filled, SectionId};
use crate::engine::{self, StepError, Stepping};
use crate::error::{Error, Result};
use crate::events::EventRow;
use crate::geometry::Geo;
use crate::host::{Contract, Decision, DecisionSink, Foreign, Host, Llm, Phase, Stage, Trace, TraceLevel, TxnKind, NO_FOREIGN};
use crate::load::{self, FloorPlans, Loaded, Sections};
use crate::rules::common_operations::Logged;
use crate::rules::metrics::MetricSample;
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
    /// The metric samples, each a JSON list of the snapshot's fields (`engine::metric_line`).
    pub metric_lines: Vec<Vec<u8>>,
    /// How many times each contract gave its input back, by the model's name.
    pub contracts: BTreeMap<String, u64>,
}

impl PlainHost {
    /// The host of `loaded`, with the sections of the exported entry it was loaded from.
    pub fn new(entry: &Value, loaded: Rc<Loaded>) -> Result<Self> {
        Ok(Self { loaded, sections: Sections::of_entry(entry)?, plans: FloorPlans::default(), metric_lines: Vec::new(), contracts: BTreeMap::new() })
    }

    /// How many times the floor plan was built.
    pub fn plan_builds(&self) -> usize {
        self.plans.builds()
    }

    /// The sha256 of the metric lines joined by `\n`, in hex (`metrics_digest` of
    /// `tests/support/core_tick.py`).
    pub fn metrics_digest(&self) -> String {
        sha256_hex(&self.metric_lines.join(&b'\n'))
    }
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

    fn capture_metric(&mut self, sample: &MetricSample, minute: i64) -> Result<()> {
        self.metric_lines.push(engine::metric_line(sample, minute)?);
        Ok(())
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
}

impl PlainRun {
    /// The Run of an exported entry (`export_world`'s document).
    pub fn of_entry(entry: &Value) -> Result<Self> {
        let loaded = Rc::new(load::loaded(entry)?);
        // The log holds its records in the binary form (#252): about a seventh of their JSON.
        loaded.cell.with(|world| world.log_mut().and_then(|mut log| log.pack_records()))??;
        let host = PlainHost::new(entry, Rc::clone(&loaded))?;
        Ok(Self { loaded, host, stepping: Stepping::default() })
    }

    /// `of_entry` of the entry's JSON text.
    pub fn of_json(raw: &[u8]) -> Result<Self> {
        let entry: Value = serde_json::from_slice(raw).map_err(|error| Error::Value(format!("not an exported world: {error}")))?;
        Self::of_entry(&entry)
    }

    /// `of_entry` of the world `name` of a file of named worlds (`{"worlds": {name: entry}}`, as
    /// `benchmarks/core_fixtures.py` writes `native/core/tests/data/`), from its JSON text: the
    /// entry is read from the file's own bytes, its numbers as written (a JSON reader of another
    /// language may write `1.0` back as `1`).
    pub fn of_worlds_json(raw: &[u8], name: &str) -> Result<Self> {
        let mut file: Value = serde_json::from_slice(raw).map_err(|error| Error::Value(format!("not a file of worlds: {error}")))?;
        let entry = file.get_mut("worlds").and_then(|worlds| worlds.get_mut(name)).ok_or_else(|| Error::Value(format!("the file has no world {name}")))?;
        Self::of_entry(&entry.take())
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
}
