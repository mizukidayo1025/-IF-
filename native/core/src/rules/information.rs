//! The information model's sample step (`_sample_information_contacts` of the sample phase, with
//! the rules of `InformationSpreadPolicy.update` and its two subclasses, whose Python bodies lane
//! L10 deleted): the bounded local contacts of a minute, and the claims they may pass on. Moved to
//! the core in lane P8 a4.
//!
//! As in `rules/psychology.rs`, `plan` computes everything from the rows and changes nothing; any
//! value the deleted rules turned down, or that this step does not handle, is Unsupported. Then
//! `sample` writes the plan to the row of `information_spread_v1` (its attached tree follows the
//! row when the world is given back, lane P8-0c) and schedules the transfers through the world's
//! queue (`kit::Events`), as `_schedule_information_contact` did. Every float operation keeps the
//! order of the Python expression. The trace's document goes to the host (`Host::trace`).
//!
//! The model and its config are what the Run read of `controller.information_policy`
//! (`run::information`); a read that raised in Python is a `PolicyFault`, which the PyO3 crate
//! raises again (policy E14).

use std::collections::HashMap;
use std::sync::Arc;

use indexmap::IndexMap;

use crate::config::Fault;
use crate::contacts::{key_head, nearby_buckets, pair_key, quoted_ids, select_contacts, zone_members, Basis, Bucket};
use crate::entities::Entities;
use crate::error::{Error, Result};
use crate::events::writer::{WriteError, WriteResult};
use crate::events::{is_stable_id, EventKind, EventRow};
use crate::host::{Host, Trace, TraceLevel};
use crate::mathx::{expm1, py_max, py_min};
use crate::rng::{label, Key, KeyedDraws};
use crate::rules::kit::Events;
use crate::rules::psychology::{Bad, PolicyFault};
use crate::rules::trace::SUMMARY_ITEMS;
use crate::rules::world_view::{group_of, presence_people, Household};
use crate::run::held::{Held, Obj, NOT_AN_OBJECT};
use crate::run::information::{Information, Model};
use crate::state::information::{belief_rank, ContactRow, InformationRow, NAME, SCHEMA_VERSION};
use crate::value::V;
use crate::world::World;

use rustc_hash::FxHashMap;

#[track_caller]
fn unsupported<T>(reason: &'static str) -> Result<T> {
    Err(Error::unsupported(reason))
}

/// The version a model's events and documents carry.
pub fn version(model: Model) -> &'static str {
    match model {
        Model::Sparse => "sparse-information-contact-v1",
        Model::Hazard => "hazard-information-v1",
        Model::EvidenceAware => "evidence-aware-rumor-v1",
    }
}

// ---- What the Run read of the policy -------------------------------------------------------------

type Got<T> = std::result::Result<T, Bad>;

fn fault_at(path: &[&'static str], fault: Fault) -> Bad {
    Bad::Policy(PolicyFault { path: path.to_vec(), fault })
}

/// The model of `controller.information_policy`, by its exact class; None without a policy, when
/// the step does nothing.
pub fn model_of(information: &Information) -> Got<Option<Model>> {
    information.model.clone().map_err(|fault| fault_at(&["information_policy"], fault))
}

/// `information_policy.config`.
fn config_of(information: &Information) -> Got<&Arc<Obj>> {
    const PATH: [&str; 2] = ["information_policy", "config"];
    let policy = information.policy.as_ref().map_err(|fault| fault_at(&["information_policy"], fault.clone()))?;
    let policy = policy.as_ref().ok_or_else(|| fault_at(&["information_policy"], Fault::refused(NOT_AN_OBJECT)))?;
    let held = policy.get("config").map_err(|fault| fault_at(&PATH, fault))?;
    held.object().map_err(|fault| fault_at(&PATH, fault))?.ok_or_else(|| fault_at(&PATH, Fault::refused(NOT_AN_OBJECT)))
}

/// A field of the config as the Run read it.
fn field<'a>(config: &'a Obj, name: &'static str) -> Got<&'a Held> {
    config.get(name).map_err(|fault| fault_at(&["information_policy", "config", name], fault))
}

/// `exact_int` of a field.
fn int_of(held: &Held) -> Result<i64> {
    match held {
        Held::Value(V::Int(number)) => Ok(*number),
        Held::Value(V::Big(_)) => unsupported("int out of range"),
        _ => unsupported("not an int"),
    }
}

/// An exact float, or an exact int the config allows (0..=4 at most): both are the same f64 in the
/// Python expressions, where each meets a float.
fn number_of(held: &Held) -> Result<f64> {
    if let Held::Value(V::Float(number)) = held {
        return Ok(*number);
    }
    let int = int_of(held)?;
    if !(0..=1 << 53).contains(&int) {
        return unsupported("a setting is out of range");
    }
    Ok(int as f64)
}

/// The config's values the rule reads, and the rule's derived forgetting probability.
#[derive(Clone, Copy, Debug, Default)]
pub struct Settings {
    pub maximum_contacts: i64,
    pub minimum_contact_minutes: i64,
    pub duration_scale: f64,
    pub base_probability: f64,
    pub correction_multiplier: f64,
    pub trust_sensitivity: f64,
    pub family_multiplier: f64,
    pub adjacent_multiplier: f64,
    pub pressure_decay: f64,
    pub distress_sensitivity: f64,
    pub share_decay: f64,
    pub forget_probability: f64,
}

/// The config of the policy, in the order the kernel read it.
pub fn settings(information: &Information, model: Model) -> Got<Settings> {
    let config = config_of(information)?;
    let enabled = field(config, "enabled")?;
    if !matches!(enabled, Held::Value(V::Bool(true))) {
        return Err(Bad::Refused(Error::unsupported("the model is not enabled")));
    }
    let int = |name| -> Got<i64> { Ok(int_of(field(config, name)?)?) };
    let read = |name| -> Got<f64> { Ok(number_of(field(config, name)?)?) };
    let maximum_contacts = int("max_contacts_per_agent")?;
    let minimum_contact_minutes = int("minimum_contact_minutes")?;
    let scale = int("contact_duration_scale_minutes")?;
    if !(0..=16).contains(&maximum_contacts) || !(1..=1 << 53).contains(&scale) {
        return Err(Bad::Refused(Error::unsupported("a contact setting is out of range")));
    }
    let forgetting = read("forgetting_rate_per_minute")?;
    let forget_probability =
        if model == Model::EvidenceAware { -expm1(-forgetting).map_err(|error| Bad::Refused(error.into()))? } else { 0.0 };
    Ok(Settings {
        maximum_contacts,
        minimum_contact_minutes,
        duration_scale: scale as f64,
        base_probability: read("base_transmission_probability")?,
        correction_multiplier: read("correction_transmission_multiplier")?,
        trust_sensitivity: read("trust_sensitivity")?,
        family_multiplier: read("family_contact_multiplier")?,
        adjacent_multiplier: read("adjacent_zone_contact_multiplier")?,
        pressure_decay: read("query_pressure_decay_per_minute")?,
        distress_sensitivity: read("distress_sharing_sensitivity")?,
        share_decay: read("share_propensity_decay_per_minute")?,
        forget_probability,
    })
}

/// What the step reads besides the rows.
pub struct Inputs<'a> {
    pub model: Model,
    pub settings: &'a Settings,
    /// The minute, which the caller checked is the world's.
    pub minute: i64,
    /// `str(rng.seed)` of the step's `CounterRNG`.
    pub seed: &'a str,
    /// `controller.zone_neighbors`.
    pub neighbors: &'a HashMap<String, Vec<String>>,
    /// The Run keeps a trace: each contact's trace is kept, and every draw is made.
    pub recording: bool,
}

// ---- The plan -----------------------------------------------------------------------------------

/// One person of the view, in ID order.
struct Person<'a> {
    id: &'a str,
    zone: &'a str,
    trust: f64,
    stress: f64,
    /// `group_id` of the view, read only if it can matter: somebody holds a belief.
    group: Option<Arc<str>>,
    /// The position of the person's beliefs in the row.
    beliefs: usize,
}

/// A belief after the forgetting of this tick.
#[derive(Clone, Copy)]
struct Current<'a> {
    rank: u8,
    status: &'a str,
    share: f64,
}

struct BeliefChange {
    agent: usize,
    message: usize,
    share: f64,
    forgotten: bool,
}

struct Proposal {
    message: Arc<str>,
    sender: usize,
    recipient: usize,
    belief: Arc<str>,
    basis: &'static str,
    minutes: i64,
    probability: f64,
    source: Arc<str>,
    causation: Arc<str>,
}

/// One contact of `InformationContactTrace`, for its trace document and the bit comparison.
pub struct ContactTrace {
    pub sender: usize,
    pub recipient: usize,
    pub basis: &'static str,
    pub minutes: i64,
    pub message: Option<Arc<str>>,
    pub belief: Option<Arc<str>>,
    pub probability: f64,
    pub selected: bool,
    pub source: Option<Arc<str>>,
    pub causation: Option<Arc<str>>,
}

/// A minute's plan: what `sample` writes and schedules, and the contacts of its trace.
pub struct Plan {
    /// The people's IDs, in ID order.
    pub ids: Vec<Arc<str>>,
    pressure: Vec<f64>,
    beliefs: Vec<BeliefChange>,
    contacts: Vec<(Arc<str>, ContactRow)>,
    contact_attempts: i64,
    proposals: Vec<Proposal>,
    /// Each contact, in the order they were made (only when recording).
    pub traces: Vec<ContactTrace>,
}

impl Plan {
    /// How many transfers the plan proposes.
    pub fn proposed(&self) -> usize {
        self.proposals.len()
    }
}

fn basis_name(basis: Basis) -> &'static str {
    match basis {
        Basis::Family => "family",
        Basis::SameZone => "same_zone",
        Basis::Nearby => "adjacent_zone",
    }
}

/// `InformationSpreadPolicy._transmission_probability`, and its override in
/// `HazardInformationPolicy`.
pub fn transmission_probability(
    settings: &Settings,
    model: Model,
    trust: f64,
    basis: Basis,
    contact_minutes: i64,
    correction: bool,
) -> Result<f64> {
    let duration_factor = py_min(1.0, contact_minutes as f64 / settings.duration_scale);
    let trust_factor = 1.0 + settings.trust_sensitivity * (2.0 * trust - 1.0);
    let basis_factor = match basis {
        Basis::Family => settings.family_multiplier,
        Basis::SameZone => 1.0,
        Basis::Nearby => settings.adjacent_multiplier,
    };
    let correction_factor = if correction { settings.correction_multiplier } else { 1.0 };
    let propensity = py_min(
        1.0,
        settings.base_probability * duration_factor * trust_factor * basis_factor * correction_factor,
    );
    if model != Model::Hazard {
        return Ok(propensity);
    }
    Ok(-expm1(-propensity)?)
}

/// `_sender_sharing_factor`.
fn sharing_factor(settings: &Settings, model: Model, share: f64, stress: f64) -> f64 {
    if model != Model::EvidenceAware {
        return 1.0;
    }
    let distress_multiplier = 1.0 + settings.distress_sensitivity * stress;
    py_min(1.0, share * distress_multiplier)
}

/// `EvidenceAwareInformationPolicy.update` before the contacts: sharing fades, and some people
/// forget. Returns each belief as the contacts see it, by position in the row, and what changes.
fn forget<'a>(
    row: &'a InformationRow,
    settings: &Settings,
    model: Model,
    minute: i64,
    draws: &KeyedDraws,
    recording: bool,
) -> (Vec<Vec<Current<'a>>>, Vec<BeliefChange>) {
    let mut changes = Vec::new();
    let current = row
        .beliefs
        .iter()
        .enumerate()
        .map(|(agent_place, (agent_id, records))| {
            records
                .iter()
                .enumerate()
                .map(|(message_place, (message_id, record))| {
                    let rank = belief_rank(&record.belief).unwrap_or(0);
                    if model != Model::EvidenceAware {
                        return Current { rank, status: &record.belief, share: record.share_propensity };
                    }
                    // A draw is never below 0.0, so a zero probability needs none.
                    let forgotten = (settings.forget_probability > 0.0 || recording) && {
                        let mut key = Key::labelled(label::INFORMATION_FORGETTING);
                        key.int(minute).text(agent_id).text(message_id);
                        key.draw(draws) < settings.forget_probability
                    };
                    let share = if forgotten {
                        0.0
                    } else {
                        py_max(0.0, record.share_propensity - settings.share_decay)
                    };
                    if forgotten || share.to_bits() != record.share_propensity.to_bits() {
                        changes.push(BeliefChange { agent: agent_place, message: message_place, share, forgotten });
                    }
                    if forgotten {
                        Current { rank: 0, status: "unknown", share }
                    } else {
                        Current { rank, status: &record.belief, share }
                    }
                })
                .collect()
        })
        .collect();
    (current, changes)
}

fn plan_rows(
    inputs: &Inputs<'_>,
    presence_people: Option<&FxHashMap<Arc<str>, Household>>,
    entities: &Entities,
    row: &InformationRow,
    draws: &KeyedDraws,
) -> Result<Plan> {
    if row.schema_version != SCHEMA_VERSION {
        return unsupported("the state has another schema");
    }
    if !row.beliefs_parse() {
        return unsupported("the claims or beliefs do not parse");
    }
    let recording = inputs.recording;
    let settings = inputs.settings;
    let minute = inputs.minute;
    let any_belief = row.beliefs.values().any(|records| !records.is_empty());

    let mut agents: Vec<_> = entities.agents.iter().collect();
    agents.sort_unstable_by(|a, b| a.0.as_bytes().cmp(b.0.as_bytes()));
    let mut people = Vec::with_capacity(agents.len());
    let mut pressure = Vec::with_capacity(agents.len());
    for (id, agent) in &agents {
        if ***id != *agent.profile.id {
            return unsupported("a person is under another ID");
        }
        let (Some(beliefs), Some(value)) = (row.beliefs.get_index_of(&***id), row.query_pressure.get(&***id)) else {
            return unsupported("a person has no beliefs or no query pressure");
        };
        pressure.push(py_max(0.0, *value - settings.pressure_decay));
        let group = if any_belief || recording { group_of(presence_people, id, &agent.profile.group_id)? } else { None };
        people.push(Person { id, zone: &agent.zone_id, trust: agent.trust, stress: agent.stress, group, beliefs });
    }

    let (current, belief_changes) = forget(row, settings, inputs.model, minute, draws, recording);

    let id_texts: Vec<&str> = people.iter().map(|person| person.id).collect();
    let zone_texts: Vec<&str> = people.iter().map(|person| person.zone).collect();
    let quoted = quoted_ids(&id_texts);
    let members = zone_members(&zone_texts);

    let mut contacts: Vec<(Arc<str>, ContactRow)> = Vec::with_capacity(row.contacts.len());
    let mut attempts = row.counters.contact_attempts;
    let mut proposals = Vec::new();
    let mut traces = Vec::new();
    let mut planned: Vec<(usize, &str)> = Vec::new();
    let mut selected = Vec::new();
    for (me, person) in people.iter().enumerate() {
        let zone = person.zone;
        let mut buckets = Vec::new();
        let same_zone = &members[zone];
        if same_zone.len() > 1 {
            buckets.push(Bucket { basis: Basis::SameZone, members: same_zone, includes_self: true });
        }
        for neighbors in nearby_buckets(zone, inputs.neighbors, &members) {
            buckets.push(Bucket { basis: Basis::Nearby, members: neighbors, includes_self: false });
        }
        let mut head = key_head(label::INFORMATION_CONTACT_PARTNER, minute, &quoted[me]);
        select_contacts(draws, &mut head, me, &buckets, settings.maximum_contacts, false, &mut selected);
        for &(source, drawn_basis) in &selected {
            let sender = &people[source];
            let basis = if person.group.is_some() && person.group == sender.group {
                Basis::Family
            } else {
                drawn_basis
            };
            let key = pair_key(&quoted[me], &quoted[source])?;
            let contact_minutes = match row.contacts.get(key.as_str()) {
                Some(previous) if previous.last_seen_minute == minute - 1 => {
                    match previous.consecutive_minutes.checked_add(1) {
                        Some(minutes) if (minutes as u64) < 1 << 53 => minutes,
                        _ => return unsupported("a contact's minutes do not fit"),
                    }
                }
                _ => 1,
            };
            contacts.push((Arc::from(key), ContactRow { last_seen_minute: minute, consecutive_minutes: contact_minutes }));
            attempts = match attempts.checked_add(1) {
                Some(attempts) => attempts,
                None => return unsupported("the contact counter does not fit"),
            };

            let trace = |message, belief, probability, selected, message_source, causation| ContactTrace {
                sender: source,
                recipient: me,
                basis: basis_name(basis),
                minutes: contact_minutes,
                message,
                belief,
                probability,
                selected,
                source: message_source,
                causation,
            };

            // The transferable claim first by rank, then age, then ID, as in the Python min().
            let sender_records = &row.beliefs[sender.beliefs];
            let sender_current = &current[sender.beliefs];
            let recipient_records = &row.beliefs[person.beliefs];
            let recipient_current = &current[person.beliefs];
            let mut best: Option<(usize, i64)> = None;
            for (place, (message_id, _)) in sender_records.iter().enumerate() {
                let rank = sender_current[place].rank;
                let receiver_rank =
                    recipient_records.get_index_of(&**message_id).map_or(0, |index| recipient_current[index].rank);
                if rank <= receiver_rank {
                    continue;
                }
                let time = row.messages[&**message_id].time;
                let better = match best {
                    None => true,
                    Some((other, other_time)) => {
                        let other_id = sender_records.get_index(other).map_or("", |(id, _)| &**id);
                        (std::cmp::Reverse(rank), time, message_id.as_bytes())
                            < (std::cmp::Reverse(sender_current[other].rank), other_time, other_id.as_bytes())
                    }
                };
                if better {
                    best = Some((place, time));
                }
            }
            let Some((place, _)) = best else {
                if recording {
                    traces.push(trace(None, None, 0.0, false, None, None));
                }
                continue;
            };
            let Some((message_id, record)) = sender_records.get_index(place) else {
                return unsupported("a belief left the row");
            };
            let belief = sender_current[place];
            let message = &row.messages[&**message_id];
            if contact_minutes < settings.minimum_contact_minutes {
                if recording {
                    traces.push(trace(
                        Some(message_id.clone()),
                        Some(Arc::from(belief.status)),
                        0.0,
                        false,
                        Some(message.source_id.clone()),
                        Some(record.last_event_id.clone()),
                    ));
                }
                continue;
            }
            let propensity =
                transmission_probability(settings, inputs.model, person.trust, basis, contact_minutes, belief.rank == 2)?;
            let probability = py_min(1.0, propensity * sharing_factor(settings, inputs.model, belief.share, sender.stress));
            let mut key = Key::labelled(label::INFORMATION_CONTACT_TRANSFER);
            key.int(minute).quoted(&quoted[me]).quoted(&quoted[source]).text(message_id).text(belief.status);
            let transfers = key.draw(draws) < probability;
            if recording {
                traces.push(trace(
                    Some(message_id.clone()),
                    Some(Arc::from(belief.status)),
                    probability,
                    transfers,
                    Some(message.source_id.clone()),
                    Some(record.last_event_id.clone()),
                ));
            }
            if transfers && !planned.contains(&(me, &**message_id)) {
                planned.push((me, message_id));
                proposals.push(Proposal {
                    message: message_id.clone(),
                    sender: source,
                    recipient: me,
                    belief: Arc::from(belief.status),
                    basis: basis_name(basis),
                    minutes: contact_minutes,
                    probability,
                    source: message.source_id.clone(),
                    causation: record.last_event_id.clone(),
                });
            }
        }
    }
    // People are in ID order, so their places sort as their IDs do.
    proposals.sort_by(|a, b| (a.recipient, a.sender, a.message.as_bytes()).cmp(&(b.recipient, b.sender, b.message.as_bytes())));
    Ok(Plan {
        ids: agents.iter().map(|(id, _)| Arc::clone(id)).collect(),
        pressure,
        beliefs: belief_changes,
        contacts,
        contact_attempts: attempts,
        proposals,
        traces,
    })
}

/// The plan of the minute from the rows, changing nothing: the presence ledger's households, the
/// entity rows and the namespace's row.
pub fn plan(world: &World, inputs: &Inputs<'_>) -> Result<Plan> {
    let presence = presence_people(world)?;
    let draws = KeyedDraws::new(inputs.seed);
    let entities = world.rows()?;
    let row = world.row::<InformationRow>(NAME)?;
    plan_rows(inputs, presence.as_ref(), &entities, &row, &draws)
}

// ---- The writes ---------------------------------------------------------------------------------

fn write(world: &World, plan: &Plan) -> Result<()> {
    let mut row = world.row_mut::<InformationRow>(NAME)?;
    if !plan.beliefs.is_empty() {
        let unknown: Arc<str> = Arc::from("unknown");
        for change in &plan.beliefs {
            let Some((_, records)) = row.beliefs.get_index_mut(change.agent) else {
                return Err(Error::Runtime("the row changed under the plan".to_owned()));
            };
            let Some((_, record)) = records.get_index_mut(change.message) else {
                return Err(Error::Runtime("the row changed under the plan".to_owned()));
            };
            record.share_propensity = change.share;
            if change.forgotten {
                record.belief = unknown.clone();
                record.ever_forgotten = true;
            }
        }
    }
    for (id, value) in plan.ids.iter().zip(&plan.pressure) {
        match row.query_pressure.get_mut(&**id) {
            Some(pressure) => *pressure = *value,
            None => {
                row.query_pressure.insert(id.clone(), *value);
            }
        }
    }
    row.counters.contact_attempts = plan.contact_attempts;
    row.contacts.clear();
    for (key, contact) in &plan.contacts {
        row.contacts.insert(key.clone(), *contact);
    }
    Ok(())
}

/// What `ScheduledEvent._check` raises for a contact whose row `contact_event` refuses, in its order.
fn refused_contact_event(proposal: &Proposal, due_minute: i64, sequence: i64) -> Error {
    Error::Value(
        if !is_stable_id(&proposal.source) {
            "source_id must be a stable ID"
        } else if !is_stable_id(&proposal.causation) {
            "causation_id must be a stable ID"
        } else if due_minute < 0 {
            "event due_minute must be a non-negative integer"
        } else if sequence < 0 {
            "event sequence must be a non-negative integer"
        } else {
            "event payload and provenance must contain finite JSON values"
        }
        .to_owned(),
    )
}

/// The event of `_schedule_information_contact`, with the keys in its order.
fn contact_event(proposal: &Proposal, plan: &Plan, model: Model, due_minute: i64, sequence: i64) -> Option<EventRow> {
    let text = |value: &str| V::Str(Arc::from(value));
    let mut payload = IndexMap::with_capacity(6);
    payload.insert(Arc::from("message_id"), V::Str(proposal.message.clone()));
    payload.insert(Arc::from("sender_agent_id"), V::Str(plan.ids[proposal.sender].clone()));
    payload.insert(Arc::from("recipient_agent_id"), V::Str(plan.ids[proposal.recipient].clone()));
    payload.insert(Arc::from("belief"), V::Str(proposal.belief.clone()));
    payload.insert(Arc::from("contact_basis"), text(proposal.basis));
    payload.insert(Arc::from("contact_minutes"), V::Int(proposal.minutes));
    let mut provenance = IndexMap::with_capacity(4);
    provenance.insert(Arc::from("source_type"), text("assumption"));
    provenance.insert(Arc::from("source_id"), text("ASM-INFORMATION-SPREAD-V1"));
    provenance.insert(Arc::from("policy_version"), text(version(model)));
    provenance.insert(Arc::from("transmission_probability"), V::Float(proposal.probability));
    EventRow::new(
        Arc::from(format!("information-contact-{sequence}")),
        EventKind::Endogenous,
        due_minute,
        40,
        sequence,
        Arc::from("information_contact_update"),
        payload,
        Some(proposal.source.clone()),
        Some(proposal.causation.clone()),
        provenance,
    )
}

/// `_sample_information_contacts` of the sample phase, with a model: the plan of the minute, its
/// writes, then each transfer scheduled for the next minute (`_schedule_information_contact`). The
/// plan is returned for the trace.
pub fn sample(world: &World, inputs: &Inputs<'_>) -> WriteResult<Plan> {
    let events = Events::of(world)?;
    let plan = plan(world, inputs)?;
    write(world, &plan)?;
    for proposal in &plan.proposals {
        // `_schedule_information_contact`.
        let due_minute = inputs.minute + 1;
        if due_minute >= events.duration() {
            continue;
        }
        let sequence = events.next_sequence()?;
        match contact_event(proposal, &plan, inputs.model, due_minute, sequence) {
            Some(row) => {
                events.allocate_sequence()?;
                events.schedule(row)?;
            }
            None => {
                // It raises after taking the sequence. The state is written, so nothing may take
                // this for the step's refusal.
                events.allocate_sequence()?;
                return Err(WriteError::Error(refused_contact_event(proposal, due_minute, sequence)));
            }
        }
    }
    Ok(plan)
}

// ---- The trace ----------------------------------------------------------------------------------

fn optional(text: &Option<Arc<str>>) -> V {
    text.as_ref().map_or(V::Null, |text| V::Str(text.clone()))
}

/// `InformationSpreadResult.trace_document`: every contact with a full trace, 20 with a summary,
/// sorted by recipient, sender and message. `minute` is the value the document holds.
pub fn information_document(plan: &Plan, model: Model, minute: &V, level: TraceLevel) -> V {
    let kept = if level == TraceLevel::Full { plan.traces.len() } else { plan.traces.len().min(SUMMARY_ITEMS) };
    // The sort of `InformationSpreadPolicy.update`, which is stable.
    let mut traces: Vec<&ContactTrace> = plan.traces.iter().collect();
    let empty: Arc<str> = Arc::from("");
    traces.sort_by(|first, second| {
        let key = |trace: &ContactTrace| {
            (plan.ids[trace.recipient].clone(), plan.ids[trace.sender].clone(), trace.message.clone().unwrap_or_else(|| empty.clone()))
        };
        key(first).cmp(&key(second))
    });
    let text = |value: &str| V::Str(Arc::from(value));
    let mut contacts = Vec::with_capacity(kept);
    for trace in &traces[..kept] {
        let mut item = IndexMap::with_capacity(10);
        item.insert(Arc::from("sender_agent_id"), V::Str(plan.ids[trace.sender].clone()));
        item.insert(Arc::from("recipient_agent_id"), V::Str(plan.ids[trace.recipient].clone()));
        item.insert(Arc::from("contact_basis"), text(trace.basis));
        item.insert(Arc::from("contact_minutes"), V::Int(trace.minutes));
        item.insert(Arc::from("message_id"), optional(&trace.message));
        item.insert(Arc::from("belief"), optional(&trace.belief));
        item.insert(Arc::from("transmission_probability"), V::Float(trace.probability));
        item.insert(Arc::from("transmission_selected"), V::Bool(trace.selected));
        item.insert(Arc::from("message_source_id"), optional(&trace.source));
        item.insert(Arc::from("causation_id"), optional(&trace.causation));
        contacts.push(V::Map(item));
    }
    let mut document = IndexMap::with_capacity(6);
    document.insert(Arc::from("minute_index"), minute.clone());
    document.insert(Arc::from("policy_version"), text(version(model)));
    document.insert(Arc::from("selected_contact_count"), V::Int(plan.traces.len() as i64));
    document.insert(Arc::from("truncated_contact_count"), V::Int((plan.traces.len() - kept) as i64));
    document.insert(Arc::from("contacts"), V::List(contacts));
    document.insert(Arc::from("proposed_transfer_count"), V::Int(plan.proposals.len() as i64));
    V::Map(document)
}

/// The trace's document of a minute to the host's sink, when the Run keeps a trace.
pub fn record_information<H: Host>(host: &mut H, plan: &Plan, model: Model, minute: &V) -> std::result::Result<(), H::Error> {
    let level = host.trace_level()?;
    if level == TraceLevel::Off {
        return Ok(());
    }
    host.trace(Trace::InformationSpread(information_document(plan, model, minute, level)))
}
