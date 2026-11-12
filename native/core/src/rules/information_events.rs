//! The events of `information_spread_v1` in `events/handlers.py`: a rumor, a correction and a
//! contact. Each plans from the rows, the event's row and the inputs the PyO3 crate gathered, and
//! only then writes the namespace's row, where nothing can refuse any more: Unsupported comes before
//! any change. A tree Python reads follows the row (lane P8-0c). Moved to the core in lane P8 d1.
//!
//! A state of another schema than 2 is upgraded by the host where the Python rule upgraded it, after
//! the model's checks and before the event's (`Host::upgrade_information_state`, policy E11): only a
//! test writes one in memory, as a save of it is view-only (`LEGACY_SHAPES_TICKABLE`).
//!
//! `InformationSpreadConfig.from_mapping` and `selected_models` stay Python's: they read the
//! Scenario's contract. The PyO3 crate reads them, kept for the configs' stamp, and raises the
//! config's error before the event is read, which is where the Python rule raises it.

use std::sync::Arc;

use crate::config::information::InformationSpread;
use crate::config::read::Read;
use crate::config::{SectionId, Selection};
use crate::entities::Entities;
use crate::error::{Error, Result};
use crate::events::{EventKind, EventRow};
use crate::host::Host;
use crate::mathx::{py_max, py_min};
use crate::rng::{label, Draws};
use crate::rules::kit::{self, Held, Kept};
use crate::state::information::{belief_rank, is_stable_id, BeliefRow, InformationRow, MessageRow, NAME, SCHEMA_VERSION};
use crate::value::V;
use crate::world::{World, WorldCell, MODEL_CONFIGS};

const EVIDENCE_AWARE: &str = "evidence-aware-rumor-v1";
const CONTACT_SOURCE: &str = "ASM-INFORMATION-SPREAD-V1";
const MAX_MESSAGES: usize = 1024;

/// The failure of an event while the model is off or the world holds no state of it.
pub const NOT_ENABLED: &str = "information spread is not enabled for this run";
/// The refusal of a state of another schema that the host did not upgrade.
pub const OLDER_STATE: &str = "an older state is upgraded in Python";
/// The check of the corrupt `model_configs` that a selection which raised gives: the PyO3 crate
/// raises what `selected_models` raises in its place.
pub const SELECTION_RAISED: &str = "the model selection raised";

/// What the events read outside the world and the event, gathered before the world is borrowed.
pub struct Inputs<'a> {
    /// `InformationSpreadConfig.from_mapping(model_configs.get("information_spread"))`; its errors
    /// are raised before the event is read.
    pub config: &'a InformationSpread,
    /// Whether `model_state` holds the namespace's slot (`kit::Held`); `Absent` stands for any value
    /// that is not a dict too, which the rule takes as no state.
    pub held: &'a Held,
    /// `selected_models(world.model_configs)`, kept for the configs' stamp.
    pub selection: &'a Kept<Selection>,
}

/// The refusal of a namespace the world holds another dict of than the row's.
pub const OTHER_DOCUMENT: &str = "the world holds another document than the row's";

/// What the events read outside the world and the event, read by the core through the host (lane
/// P9-1a): the config and the model selection, kept for the configs' stamp (H2), and whether
/// `model_state` holds the namespace (H1). The config's fault is raised before the event is read
/// (the PyO3 crate's, which parses it again, policy E14), so it is kept here as the read gave it.
pub struct Gathered {
    pub config: Kept<InformationSpread>,
    pub held: Held,
    pub selection: Kept<Selection>,
}

impl Gathered {
    /// The config, whether `model_state` holds the namespace (a value that is not a dict stands for
    /// none), and the selection, read in that order with nothing of the world borrowed.
    pub fn read<C: WorldCell, H: Host>(cell: &C, host: &mut H) -> std::result::Result<Self, H::Error> {
        let holding = kit::holding(cell, host)?;
        let config = kit::section_kept(cell, host, &holding, SectionId::InformationSpread, |configs| &configs.information_spread)?;
        let held = holding.held_dict(NAME, OTHER_DOCUMENT);
        let (selection, _) = kit::selection(cell, host, &holding)?;
        Ok(Self { config, held, selection })
    }

    /// The inputs of an event, with the config the caller took of `config`.
    pub fn inputs<'a>(&'a self, config: &'a InformationSpread) -> Inputs<'a> {
        Inputs { config, held: &self.held, selection: &self.selection }
    }
}

/// What an event that ran did: it applied, or it no longer applies (`EventSuppressed`, of this
/// text). A failure is `Error::Outcome`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Applied {
    Applied,
    Suppressed(&'static str),
}

/// A value of the config as its read site takes it: a refusal its `Unsupported`, any other fault a
/// corrupt `model_configs` (`configs::read` of the PyO3 crate).
#[track_caller]
fn read<T>(value: Read<T>) -> Result<T> {
    match value {
        Ok(value) => Ok(value),
        Err(fault) if fault.is_refusal() => Err(Error::unsupported(fault.text.to_string())),
        Err(_) => Err(Error::corrupt(MODEL_CONFIGS, "a value of a section holds an error")),
    }
}

/// `selected_models(world.model_configs)["information"]`: a selection that is not one refused, one
/// that raised the corrupt `model_configs` of `SELECTION_RAISED`, and a selection without the key
/// its `KeyError`.
#[track_caller]
pub fn selected_information(selection: &Kept<Selection>) -> Result<Arc<str>> {
    match selection {
        Ok(selection) => selection.get("information").cloned().ok_or_else(|| Error::Key("information".to_owned())),
        Err(fault) if fault.is_refusal() => Err(Error::unsupported("not a selection")),
        Err(_) => Err(Error::corrupt(MODEL_CONFIGS, SELECTION_RAISED)),
    }
}

fn evidence_aware(selection: &Kept<Selection>) -> Result<bool> {
    Ok(&*selected_information(selection)? == EVIDENCE_AWARE)
}

fn trust_of(world: &World, agent_id: &str) -> Result<f64> {
    world.rows()?.agents.get(agent_id).map(|agent| agent.trust).ok_or_else(|| Error::Key(agent_id.to_owned()))
}

/// `config.correction_source_reliability_by_id.get(source_id, config.default_source_reliability)`.
fn reliability_of(config: &InformationSpread, source_id: &str) -> Result<f64> {
    read(config.reliability(source_id))
}

/// `_sample_claim_belief_strength`: `CounterRNG(world.seed)` keyed by the minute, the person and the
/// message, which do not depend on the order events are made in (model version 2).
pub fn sample_strength(world: &World, config: &InformationSpread, agent_id: &str, source_id: &str, minute: i64, message_id: &str) -> Result<f64> {
    let reliability = reliability_of(config, source_id)?;
    let trust = trust_of(world, agent_id)?;
    let probability = py_min(
        1.0,
        py_max(
            0.0,
            read(config.rumor_belief_probability.clone())?
                + 2.0 * read(config.rumor_trust_sensitivity.clone())? * (trust - 0.5)
                + 2.0 * read(config.rumor_source_sensitivity.clone())? * (reliability - 0.5),
        ),
    );
    let drawn = Draws::of_world(world)?.random(label::RUMOR_BELIEF, minute, &[agent_id, message_id]);
    Ok(if drawn < probability { 1.0 } else { 0.0 })
}

/// `min(0.95, max(0.0, acceptance + 2.0 * trust_sensitivity * (trust - 0.5) + 2.0 * source_sensitivity * (reliability - 0.5)))`.
pub fn acceptance_probability(config: &InformationSpread, trust: f64, reliability: f64) -> Result<f64> {
    Ok(py_min(
        0.95,
        py_max(
            0.0,
            read(config.correction_acceptance_probability.clone())?
                + 2.0 * read(config.correction_trust_sensitivity.clone())? * (trust - 0.5)
                + 2.0 * read(config.correction_source_sensitivity.clone())? * (reliability - 0.5),
        ),
    ))
}

/// A correction's acceptance as the rules draw it: `ids` are the key's parts after the minute.
#[allow(clippy::too_many_arguments)]
pub fn accepts(world: &World, config: &InformationSpread, label: &str, trust: f64, source_id: &str, minute: i64, ids: &[&str]) -> Result<bool> {
    let chance = acceptance_probability(config, trust, reliability_of(config, source_id)?)?;
    Ok(Draws::of_world(world)?.random(label, minute, ids) < chance)
}

/// `_claim_is_misbelieved`.
fn misbelieved(truth: &str, strength: f64) -> bool {
    (truth == "true" && strength < 0.5) || (truth == "false" && strength > 0.5)
}

/// What `AgentInformationState.__post_init__` would turn down of a belief a rule made, beyond what
/// the row's types guarantee.
fn is_valid(record: &BeliefRow) -> bool {
    is_stable_id(&record.message_id)
        && is_stable_id(&record.source)
        && is_stable_id(&record.last_event_id)
        && record.last_contact >= 0
        && (!record.correction_accepted || record.correction_exposed)
        && (!record.last_correction_accepted || record.correction_exposed)
        && (0.0..=1.0).contains(&record.belief_strength)
        && (0.0..=1.0).contains(&record.share_propensity)
        && (&*record.belief != "unknown" || record.share_propensity == 0.0)
}

/// A belief that was there, which the rule reads as `AgentInformationState.from_dict` does.
fn prior_of(record: &BeliefRow) -> Result<BeliefRow> {
    if !record.parses() {
        return Err(Error::unsupported("a belief is for the Python rule to read"));
    }
    Ok(record.clone())
}

/// The flags of a belief a rule makes, in the order `AgentInformationState.to_dict` lists them.
struct Flags {
    correction_exposed: bool,
    correction_accepted: bool,
    ever_misbelieved: bool,
    ever_forgotten: bool,
    last_correction_accepted: bool,
}

/// A new belief record, as `AgentInformationState.to_dict` lists it.
#[allow(clippy::too_many_arguments)]
fn belief(
    message_id: &Arc<str>,
    belief: Arc<str>,
    source: Arc<str>,
    last_contact: i64,
    last_event_id: Arc<str>,
    belief_strength: f64,
    share_propensity: f64,
    flags: Flags,
) -> BeliefRow {
    BeliefRow {
        message_id: message_id.clone(),
        belief,
        source,
        last_contact,
        last_event_id,
        exposed: true,
        belief_strength,
        share_propensity,
        correction_exposed: flags.correction_exposed,
        correction_accepted: flags.correction_accepted,
        ever_misbelieved: flags.ever_misbelieved,
        ever_forgotten: flags.ever_forgotten,
        last_correction_accepted: flags.last_correction_accepted,
    }
}

/// `set(payload) == set(names)`, of distinct names.
fn fields_are(payload: &V, names: &[&str]) -> bool {
    match payload {
        V::Map(map) => map.len() == names.len() && names.iter().all(|name| map.contains_key(*name)),
        _ => false,
    }
}

/// `payload[key]` of a key `fields_are` found.
fn field<'a>(payload: &'a V, key: &str) -> &'a V {
    static NULL: V = V::Null;
    match payload {
        V::Map(map) => map.get(key).unwrap_or(&NULL),
        _ => &NULL,
    }
}

/// `value if isinstance(value, str) else None`.
fn str_of(value: &V) -> Option<&Arc<str>> {
    match value {
        V::Str(text) => Some(text),
        _ => None,
    }
}

/// `Enum(value)` of a str enum whose values are `members`: the text, or None where the Python rule
/// fails.
fn member_of<'a>(value: &'a V, members: &[&str]) -> Option<&'a str> {
    str_of(value).map(|text| &**text).filter(|text| members.contains(text))
}

/// `value == text` of an event's optional str and a str.
fn is_text(value: &Option<Arc<str>>, text: &str) -> bool {
    value.as_deref() == Some(text)
}

/// `value == text` as Python compares a JSON value with a str.
fn text_is(value: &V, text: &str) -> bool {
    matches!(value, V::Str(value) if &**value == text)
}

/// `counters[name] += 1`, of a count `room` said has room.
fn count(value: &mut i64) {
    *value += 1;
}

/// Unsupported for a count `adds` more would overflow, before anything is written.
fn room(value: i64, adds: i64) -> Result<()> {
    match value.checked_add(adds) {
        Some(_) => Ok(()),
        None => Err(Error::unsupported("a count is too large")),
    }
}

/// `isinstance(value, int) and not isinstance(value, bool)`: the int, or None; Unsupported for one
/// past 64 bits.
#[track_caller]
pub fn int_not_bool(value: &V) -> Result<Option<i64>> {
    match value {
        V::Int(number) => Ok(Some(*number)),
        V::Big(_) => Err(Error::unsupported("an int is too large")),
        _ => Ok(None),
    }
}

/// `not isinstance(value, bool) and isinstance(value, (int, float))`, as `float(value)`: None when it
/// isn't one. An int a float can't hold exactly is refused.
#[track_caller]
pub fn real_number(value: &V) -> Result<Option<f64>> {
    match value {
        V::Float(number) => Ok(Some(*number)),
        V::Int(number) if (-(1 << 53)..=1 << 53).contains(number) => Ok(Some(*number as f64)),
        V::Int(_) => Err(Error::unsupported("a number does not fit a float")),
        V::Big(_) => Err(Error::unsupported("an int is too large")),
        _ => Ok(None),
    }
}

/// `zone in world.zones`.
fn zone_is_known(rows: &Entities, zone: &str) -> Result<bool> {
    if rows.zones_tainted().is_some() {
        return Err(Error::unsupported("the zones are tainted"));
    }
    Ok(rows.zones.contains_key(zone))
}

// ---- The state, and its upgrade --------------------------------------------------------------------

/// Whether the state is ready to read (true), or of another schema, or a tree the row no longer
/// holds, for the host to upgrade first (false). The model's checks come first.
fn state_ready(world: &World, config: &InformationSpread, held: &Held) -> Result<bool> {
    if !config.enabled || matches!(held, Held::Absent) {
        return Err(Error::failed(NOT_ENABLED));
    }
    if let Held::Other(reason) = held {
        return Err(Error::unsupported(*reason));
    }
    let slot = world.slots.get(NAME).ok_or_else(|| Error::unsupported("the namespace is not owned"))?.try_borrow()?;
    if slot.row.is_none() {
        return Ok(false);
    }
    Ok(slot.row::<InformationRow>()?.schema_version == SCHEMA_VERSION)
}

/// `_information_runtime_state`'s checks of the model and its state, for the configs' tests: the
/// failure of a model that is off, the refusal of a state the host would have to upgrade.
pub fn check_state(world: &World, config: &InformationSpread, held: &Held) -> Result<()> {
    if state_ready(world, config, held)? {
        Ok(())
    } else {
        Err(Error::unsupported(OLDER_STATE))
    }
}

/// An event's plan and write on the world, after the host upgraded a state of another schema where
/// the Python rule did: after the model's checks, before the event's. Nothing of the world is
/// borrowed while the host runs, and the state is read again after it (rule R6).
fn run<C: WorldCell, H: Host, T>(cell: &C, host: &mut H, inputs: &Inputs<'_>, step: impl Fn(&World) -> Result<T>) -> std::result::Result<T, H::Error> {
    let first = cell.with(|world| -> Result<Option<T>> {
        if !state_ready(world, inputs.config, inputs.held)? {
            return Ok(None);
        }
        step(world).map(Some)
    })??;
    if let Some(done) = first {
        return Ok(done);
    }
    host.upgrade_information_state()?;
    Ok(cell.with(|world| -> Result<T> {
        if !state_ready(world, inputs.config, inputs.held)? {
            return Err(Error::unsupported(OLDER_STATE));
        }
        step(world)
    })??)
}

// ---- InformationRumorHandler --------------------------------------------------------------------------

/// `InformationRumorHandler.apply`: the claim, its origin's belief and query pressure.
pub fn rumor<C: WorldCell, H: Host>(cell: &C, host: &mut H, inputs: &Inputs<'_>, event: &EventRow) -> std::result::Result<(), H::Error> {
    run(cell, host, inputs, |world| rumor_in(world, inputs, event))
}

fn rumor_in(world: &World, inputs: &Inputs<'_>, event: &EventRow) -> Result<()> {
    let config = inputs.config;
    let payload = &event.payload;
    if !fields_are(payload, &["message_id", "truth_status", "origin_agent_id"]) {
        return Err(Error::failed("information_rumor payload fields do not match"));
    }
    let Some(source) = event.source_id.clone() else {
        return Err(Error::failed("information_rumor requires a source_id"));
    };
    let (Some(message_id), Some(origin)) = (str_of(field(payload, "message_id")), str_of(field(payload, "origin_agent_id"))) else {
        return Err(Error::failed("information_rumor requires stable message and Agent IDs"));
    };
    let Some(truth) = member_of(field(payload, "truth_status"), &["unknown", "true", "false"]) else {
        return Err(Error::failed("information_rumor truth_status is invalid"));
    };
    let (taken, messages, has_agent, pressure) = {
        let row = world.row::<InformationRow>(NAME)?;
        (row.messages.contains_key(&**message_id), row.messages.len(), row.beliefs.contains_key(&**origin), row.query_pressure.get(&**origin).copied())
    };
    let zone = world.rows()?.agents.get(origin).map(|agent| agent.zone_id.clone());
    if taken {
        return Err(Error::failed("information message ID already exists"));
    }
    if messages >= MAX_MESSAGES {
        return Err(Error::failed("information-spread message limit was reached"));
    }
    let Some(zone) = zone else {
        return Err(Error::failed("information_rumor references an unknown Agent"));
    };
    let Some(pressure) = pressure.filter(|_| has_agent) else {
        return Err(Error::unsupported("the person has no record"));
    };
    let due = event.due_minute;
    if due < 0 || ![&**message_id, &*source, &*zone, &*event.event_id, &**origin].iter().all(|id| is_stable_id(id)) {
        return Err(Error::unsupported("an ID for the Python rule to judge"));
    }
    let message = MessageRow {
        id: message_id.clone(),
        truth_status: Arc::from(truth),
        source_id: source.clone(),
        zone_id: zone,
        time: due,
        origin_event_id: event.event_id.clone(),
        origin_agent_id: origin.clone(),
    };
    let strength = if evidence_aware(inputs.selection)? { sample_strength(world, config, origin, &source, due, message_id)? } else { 0.5 };
    let flags = Flags {
        correction_exposed: false,
        correction_accepted: false,
        ever_misbelieved: misbelieved(truth, strength),
        ever_forgotten: false,
        last_correction_accepted: false,
    };
    let record = belief(message_id, Arc::from("heard"), origin.clone(), due, event.event_id.clone(), strength, 0.5, flags);
    let next = py_min(1.0, pressure + read(config.query_pressure_per_claim.clone())?);
    let mut row = world.row_mut::<InformationRow>(NAME)?;
    row.messages.insert(message_id.clone(), message);
    row.beliefs.get_mut(&**origin).ok_or_else(|| Error::unsupported("the person has no beliefs"))?.insert(message_id.clone(), record);
    row.query_pressure.insert(origin.clone(), next);
    Ok(())
}

// ---- InformationCorrectionHandler ----------------------------------------------------------------------

/// `InformationCorrectionHandler.apply`: the correction, to everyone in the zone.
pub fn correction<C: WorldCell, H: Host>(cell: &C, host: &mut H, inputs: &Inputs<'_>, event: &EventRow) -> std::result::Result<(), H::Error> {
    run(cell, host, inputs, |world| correction_in(world, inputs, event))
}

fn correction_in(world: &World, inputs: &Inputs<'_>, event: &EventRow) -> Result<()> {
    struct Change {
        agent_id: Arc<str>,
        record: BeliefRow,
        pressure: Option<f64>,
    }
    let config = inputs.config;
    let payload = &event.payload;
    if !fields_are(payload, &["message_id", "zone_id", "belief"]) {
        return Err(Error::failed("information_correction payload fields do not match"));
    }
    let Some(source) = event.source_id.clone() else {
        return Err(Error::failed("information_correction requires a source_id"));
    };
    let (Some(message_id), Some(zone)) = (str_of(field(payload, "message_id")), str_of(field(payload, "zone_id"))) else {
        return Err(Error::failed("information_correction requires stable message and Zone IDs"));
    };
    let Some(belief_text) = member_of(field(payload, "belief"), &["unknown", "heard", "verified", "rejected"]) else {
        return Err(Error::failed("information_correction belief is invalid"));
    };
    if !matches!(belief_text, "verified" | "rejected") {
        return Err(Error::failed("information_correction must verify or reject a claim"));
    }
    if !zone_is_known(&*world.rows()?, zone)? {
        return Err(Error::failed("information_correction references an unknown Zone"));
    }
    let found = {
        let row = world.row::<InformationRow>(NAME)?;
        match row.messages.get(&**message_id) {
            None => None,
            Some(message) if !message.parses() => return Err(Error::unsupported("a message is for the Python rule to read")),
            Some(message) => Some((message.truth_status.clone(), message.origin_event_id.clone())),
        }
    };
    let Some((truth, origin_event)) = found else {
        return Err(Error::failed("information_correction references an unknown message"));
    };
    if !is_text(&event.causation_id, &origin_event) {
        return Err(Error::failed("information_correction must reference its rumor event"));
    }
    match &*truth {
        "true" if belief_text != "verified" => return Err(Error::failed("information correction conflicts with message truth")),
        "false" if belief_text != "rejected" => return Err(Error::failed("information correction conflicts with message truth")),
        _ => {}
    }
    let aware = evidence_aware(inputs.selection)?;
    let reliability = reliability_of(config, &source)?;
    // The people in the zone, by ID.
    let mut targets: Vec<(Arc<str>, f64)> = world
        .rows()?
        .agents
        .iter()
        .filter(|(_, agent)| agent.zone_id == *zone)
        .map(|(_, agent)| (agent.profile.id.clone(), agent.trust))
        .collect();
    targets.sort_unstable_by(|first, second| first.0.cmp(&second.0));
    let due = event.due_minute;
    let draws = Draws::of_world(world)?;
    let relief = read(config.query_pressure_relief_per_correction.clone())?;
    let verified = belief_text == "verified";
    let belief_status: Arc<str> = Arc::from(belief_text);
    let mut changes = Vec::new();
    {
        let row = world.row::<InformationRow>(NAME)?;
        for (agent_id, trust) in &targets {
            let Some(beliefs) = row.beliefs.get(&**agent_id) else {
                return Err(Error::unsupported("the person has no beliefs"));
            };
            let previous = beliefs.get(&**message_id).map(prior_of).transpose()?;
            let pressure = row.query_pressure.get(&**agent_id).copied();
            let accepted = if aware {
                let probability = acceptance_probability(config, *trust, reliability)?;
                draws.random(label::CORRECTION_ACCEPTANCE, due, &[&source, agent_id, message_id]) < probability
            } else {
                true
            };
            if let Some(previous) = &previous {
                if matches!(&*previous.belief, "verified" | "rejected") {
                    if previous.belief != belief_status {
                        return Err(Error::failed("conflicting verified information state"));
                    }
                    if !aware {
                        continue;
                    }
                }
            }
            let prior_strength = previous.as_ref().map_or(0.5, |prior| prior.belief_strength);
            let status = if accepted { belief_status.clone() } else { previous.as_ref().map_or_else(|| Arc::from("heard"), |prior| prior.belief.clone()) };
            let strength = if !accepted {
                prior_strength
            } else if verified {
                1.0
            } else {
                0.0
            };
            let flags = Flags {
                correction_exposed: true,
                correction_accepted: accepted || previous.as_ref().is_some_and(|prior| prior.correction_accepted),
                ever_misbelieved: previous.as_ref().is_some_and(|prior| prior.ever_misbelieved) || (!accepted && misbelieved(&truth, prior_strength)),
                ever_forgotten: previous.as_ref().is_some_and(|prior| prior.ever_forgotten),
                last_correction_accepted: accepted,
            };
            let share = previous.as_ref().map_or(0.5, |prior| prior.share_propensity);
            let record = belief(message_id, status, source.clone(), due, event.event_id.clone(), strength, share, flags);
            if !is_valid(&record) {
                return Err(Error::unsupported("a belief for the Python rule to judge"));
            }
            let pressure = if accepted {
                let Some(pressure) = pressure else {
                    return Err(Error::unsupported("the person has no record"));
                };
                Some(py_max(0.0, pressure - relief))
            } else {
                None
            };
            changes.push(Change { agent_id: agent_id.clone(), record, pressure });
        }
        let deliveries = changes.iter().filter(|change| change.pressure.is_some()).count() as i64;
        room(row.counters.correction_deliveries, deliveries)?;
    }
    let new_truth = (&*truth == "unknown").then(|| if verified { "true" } else { "false" });
    let mut row = world.row_mut::<InformationRow>(NAME)?;
    if let Some(truth) = new_truth {
        // `replace(message, truth_status=...).to_dict()`: the same fields, the record in its place.
        row.messages.get_mut(&**message_id).ok_or_else(|| Error::unsupported("an unknown message"))?.truth_status = Arc::from(truth);
    }
    for change in changes {
        row.beliefs.get_mut(&*change.agent_id).ok_or_else(|| Error::unsupported("the person has no beliefs"))?.insert(message_id.clone(), change.record);
        if let Some(pressure) = change.pressure {
            row.query_pressure.insert(change.agent_id.clone(), pressure);
            count(&mut row.counters.correction_deliveries);
        }
    }
    Ok(())
}

// ---- InformationContactHandler ---------------------------------------------------------------------------

/// `_current_information_contact_basis` of two known people, or None.
fn contact_basis(world: &World, sender: &str, recipient: &str) -> Result<Option<&'static str>> {
    let same_zone = {
        let rows = world.rows()?;
        let (Some(first), Some(second)) = (rows.agents.get(sender), rows.agents.get(recipient)) else {
            return Err(Error::unsupported("an unknown person"));
        };
        let (first, second) = (&*first.zone_id, &*second.zone_id);
        let same_zone = first == second;
        if rows.zones_tainted().is_some() {
            return Err(Error::unsupported("the zone edges are tainted"));
        }
        // `{from, to} == {first, second}` of an open edge.
        let adjacent = rows.zone_edges.iter().any(|(_, edge)| {
            let (from, to) = (&*edge.from_zone_id, &*edge.to_zone_id);
            !edge.closed && ((from == first && to == second) || (from == second && to == first))
        });
        if !same_zone && !adjacent {
            return Ok(None);
        }
        same_zone
    };
    let home = kit::household_in(world, sender)?;
    if home.is_some() && home == kit::household_in(world, recipient)? {
        return Ok(Some("family"));
    }
    Ok(Some(if same_zone { "same_zone" } else { "adjacent_zone" }))
}

/// `InformationContactHandler.apply`: the recipient's belief, query pressure and the counters, or
/// the text of a contact that no longer applies.
pub fn contact<C: WorldCell, H: Host>(cell: &C, host: &mut H, inputs: &Inputs<'_>, event: &EventRow) -> std::result::Result<Applied, H::Error> {
    run(cell, host, inputs, |world| contact_in(world, inputs, event))
}

fn contact_in(world: &World, inputs: &Inputs<'_>, event: &EventRow) -> Result<Applied> {
    let config = inputs.config;
    let payload = &event.payload;
    if !fields_are(payload, &["message_id", "sender_agent_id", "recipient_agent_id", "belief", "contact_basis", "contact_minutes"]) {
        return Err(Error::failed("information_contact_update payload fields do not match"));
    }
    if event.kind != EventKind::Endogenous {
        return Err(Error::failed("information contact updates must be endogenous"));
    }
    if event.due_minute != world.minute_index()? {
        return Err(Error::failed("information contact update must run at its due minute"));
    }
    let due = event.due_minute;
    let provenance = &event.provenance;
    let model = selected_information(inputs.selection)?;
    let aware = &*model == EVIDENCE_AWARE;
    if !text_is(kit::get(provenance, "source_type")?, "assumption")
        || !text_is(kit::get(provenance, "source_id")?, CONTACT_SOURCE)
        || !text_is(kit::get(provenance, "policy_version")?, &model)
    {
        return Err(Error::failed("information contact provenance is missing its assumption ID"));
    }
    let (Some(message_id), Some(sender_id), Some(recipient_id)) =
        (str_of(field(payload, "message_id")), str_of(field(payload, "sender_agent_id")), str_of(field(payload, "recipient_agent_id")))
    else {
        return Err(Error::failed("information contact requires stable claim and Agent IDs"));
    };
    if sender_id == recipient_id {
        return Err(Error::failed("information contact requires two different Agents"));
    }
    let known_people = {
        let rows = world.rows()?;
        rows.agents.contains_key(sender_id) && rows.agents.contains_key(recipient_id)
    };
    let known_message = world.row::<InformationRow>(NAME)?.messages.contains_key(&**message_id);
    if !known_people || !known_message {
        return Err(Error::failed("information contact references unknown state"));
    }
    // The sender's belief, the claim, and the signal.
    let found = {
        let row = world.row::<InformationRow>(NAME)?;
        let Some(message) = row.messages.get(&**message_id) else {
            return Err(Error::unsupported("an unknown message"));
        };
        if !message.parses() {
            return Err(Error::unsupported("a message is for the Python rule to read"));
        }
        let Some(sender) = row.beliefs.get(&**sender_id) else {
            return Err(Error::unsupported("a person has no beliefs"));
        };
        match sender.get(&**message_id) {
            None => None,
            Some(known) if !known.parses() => return Err(Error::unsupported("a belief is for the Python rule to read")),
            Some(known) => Some((message.truth_status.clone(), message.source_id.clone(), known.belief.clone(), known.source.clone(), known.last_event_id.clone())),
        }
    };
    // What no longer applies is suppressed, with the rule's text.
    let Some((truth, claim_source, sender_belief, sender_source, sender_event)) = found else {
        return Ok(Applied::Suppressed("contact sender no longer knows the message"));
    };
    let Some(belief_text) = member_of(field(payload, "belief"), &["unknown", "heard", "verified", "rejected"]) else {
        return Err(Error::failed("information contact belief is invalid"));
    };
    let wanted_rank = belief_rank(belief_text).ok_or_else(|| Error::unsupported("a belief without a rank"))?;
    if &*sender_belief != belief_text {
        return Ok(Applied::Suppressed("contact sender belief changed before transmission"));
    }
    let Some(basis) = contact_basis(world, sender_id, recipient_id)? else {
        return Ok(Applied::Suppressed("Agents are no longer in a modeled contact zone"));
    };
    if !text_is(field(payload, "contact_basis"), basis) {
        return Ok(Applied::Suppressed("sampled contact basis is no longer current"));
    }
    let minimum = read(config.minimum_contact_minutes.clone())?;
    if !int_not_bool(field(payload, "contact_minutes"))?.is_some_and(|minutes| minutes >= minimum) {
        return Err(Error::failed("information contact duration is below its configured minimum"));
    }
    if !real_number(kit::get(provenance, "transmission_probability")?)?.is_some_and(|probability| 0.0 < probability && probability <= 1.0) {
        return Err(Error::failed("information contact probability is invalid"));
    }
    if !is_text(&event.source_id, &claim_source) || !is_text(&event.causation_id, &sender_event) {
        return Err(Error::failed("information contact provenance does not match sender state"));
    }
    let correcting = matches!(belief_text, "verified" | "rejected");
    if correcting && !matches!((&*truth, belief_text), ("true", "verified") | ("false", "rejected")) {
        return Err(Error::failed("transmitted correction conflicts with message truth"));
    }
    let (previous, pressure, contacts, corrections) = {
        let row = world.row::<InformationRow>(NAME)?;
        let Some(recipient) = row.beliefs.get(&**recipient_id) else {
            return Err(Error::unsupported("a person has no beliefs"));
        };
        let previous = recipient.get(&**message_id).map(prior_of).transpose()?;
        (previous, row.query_pressure.get(&**recipient_id).copied(), row.counters.contact_deliveries, row.counters.correction_deliveries)
    };
    if let Some(prior) = &previous {
        let Some(rank) = belief_rank(&prior.belief) else {
            return Err(Error::unsupported("a belief is for the Python rule to read"));
        };
        if rank >= wanted_rank && !(aware && correcting) {
            return Ok(Applied::Suppressed("recipient already has an equally strong belief"));
        }
    }
    let mut accepted = true;
    if aware && correcting {
        let reliability = reliability_of(config, &sender_source)?;
        let trust = trust_of(world, recipient_id)?;
        let chance = acceptance_probability(config, trust, reliability)?;
        let drawn = Draws::of_world(world)?.random(label::CONTACT_CORRECTION_ACCEPTANCE, due, &[sender_id, recipient_id, message_id]);
        accepted = drawn < chance;
    }
    let strength = if aware && belief_text == "heard" {
        sample_strength(world, config, recipient_id, &claim_source, due, message_id)?
    } else {
        previous.as_ref().map_or(0.5, |prior| prior.belief_strength)
    };
    let status = if accepted { Arc::from(belief_text) } else { previous.as_ref().map_or_else(|| Arc::from("heard"), |prior| prior.belief.clone()) };
    let written_strength = match (accepted && correcting, belief_text) {
        (true, "verified") => 1.0,
        (true, _) => 0.0,
        (false, _) => strength,
    };
    let flags = Flags {
        correction_exposed: correcting || previous.as_ref().is_some_and(|prior| prior.correction_exposed),
        correction_accepted: (accepted && correcting) || previous.as_ref().is_some_and(|prior| prior.correction_accepted),
        ever_misbelieved: previous.as_ref().is_some_and(|prior| prior.ever_misbelieved) || misbelieved(&truth, strength),
        ever_forgotten: previous.as_ref().is_some_and(|prior| prior.ever_forgotten),
        last_correction_accepted: if correcting { accepted } else { previous.as_ref().is_some_and(|prior| prior.last_correction_accepted) },
    };
    let share = previous.as_ref().map_or(0.5, |prior| prior.share_propensity);
    let record = belief(message_id, status, sender_id.clone(), due, event.event_id.clone(), written_strength, share, flags);
    if !is_valid(&record) {
        return Err(Error::unsupported("a belief for the Python rule to judge"));
    }
    let Some(pressure) = pressure else {
        return Err(Error::unsupported("the person has no record"));
    };
    // `heard` adds, an accepted correction relieves and counts, and every contact counts.
    let heard = belief_text == "heard";
    let next = if heard {
        Some(py_min(1.0, pressure + read(config.query_pressure_per_claim.clone())?))
    } else if accepted {
        Some(py_max(0.0, pressure - read(config.query_pressure_relief_per_correction.clone())?))
    } else {
        None
    };
    let delivered = !heard && accepted;
    room(contacts, 1)?;
    room(corrections, i64::from(delivered))?;
    let mut row = world.row_mut::<InformationRow>(NAME)?;
    row.beliefs.get_mut(&**recipient_id).ok_or_else(|| Error::unsupported("a person has no beliefs"))?.insert(message_id.clone(), record);
    if let Some(next) = next {
        row.query_pressure.insert(recipient_id.clone(), next);
    }
    if delivered {
        count(&mut row.counters.correction_deliveries);
    }
    count(&mut row.counters.contact_deliveries);
    Ok(Applied::Applied)
}
