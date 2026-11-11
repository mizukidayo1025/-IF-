//! The trace documents of a Run, with their shape unchanged (docs/rust-port/step4-plan.md, D1;
//! lane P8 a3): those the Python rules wrote with `AgentPsychologyTrace.to_dict` and
//! `PsychologyUpdateResult.trace_document` (`policies/psychology.py`) and
//! `SocialPropagationResult.trace_document` (`policies/social.py`), which lane L10 deleted.
//! `_record_psychology_trace` of the update phase is here too (`record_psychology`). A trace does not
//! feed back into the state and draws nothing.
//!
//! A document is a `V` with the keys of the Python dict in their order, and it goes to the Run's
//! sink through `Host::trace`. The writer stays in Python (`inspection/traces.py`): it holds the
//! files, the spool, and `_sanitize`, which is not a rule of the model and stays the one place that
//! redacts what a Run writes. So each document is the same Python dict once the host makes it, and
//! the writer gives the same bytes.
//!
//! The kernels keep what a document needs in plain rows (`Traces`, `SocialContact`), and make the
//! document only when a sink takes it.

use std::cmp::Ordering;
use std::sync::Arc;

use indexmap::IndexMap;
use rustc_hash::FxHashMap;

use crate::contacts::Basis;
use crate::error::{Error, Result};
use crate::host::{Host, Trace, TraceLevel};
use crate::mathx::{py_fsum, Num};
use crate::rules::world_view::{status_text, ViewAgent, ViewData, ViewFacility};
use crate::value::V;

/// How many a summary document keeps of each list.
pub const SUMMARY_ITEMS: usize = 20;

/// A map of a document, filled in the order of the Python dict.
struct Doc(IndexMap<Arc<str>, V>);

impl Doc {
    fn new() -> Self {
        Doc(IndexMap::new())
    }

    fn with_capacity(capacity: usize) -> Self {
        Doc(IndexMap::with_capacity(capacity))
    }

    fn set(&mut self, key: &str, value: impl Into<V>) {
        self.0.insert(Arc::from(key), value.into());
    }

    fn done(self) -> V {
        V::Map(self.0)
    }
}

impl From<Doc> for V {
    fn from(doc: Doc) -> V {
        doc.done()
    }
}

fn text(value: &str) -> V {
    V::Str(Arc::from(value))
}

fn option_text(value: Option<&str>) -> V {
    value.map_or(V::Null, text)
}

fn texts(items: &[Arc<str>]) -> V {
    V::List(items.iter().map(|item| V::Str(item.clone())).collect())
}

fn float(value: f64) -> V {
    V::Float(value)
}

fn int(value: i64) -> V {
    V::Int(value)
}

fn count(value: usize) -> V {
    V::Int(value as i64)
}

fn num(value: Num) -> V {
    match value {
        Num::Int(value) => V::Int(value),
        Num::Float(value) => V::Float(value),
    }
}

// ---- A person's psychology -----------------------------------------------------------------------

/// What a person's part of the psychology document holds that the view doesn't.
#[derive(Clone, Debug)]
pub struct PersonTrace {
    /// In the view's people.
    pub index: usize,
    /// The name, the value before and after.
    pub needs: Vec<(&'static str, f64, f64)>,
    pub stress_before: f64,
    pub stress_after: f64,
    pub highest_need: &'static str,
    pub highest_need_value: f64,
    pub need_pressure: f64,
    pub need_pressure_stress: f64,
    pub queue_wait_stress: f64,
    pub support_wait_stress: f64,
    pub recovery_stress: f64,
    pub environmental_stress: f64,
    pub natural_recovery_stress: f64,
    pub life_state_feedback_stress: f64,
    pub information_uncertainty_stress: f64,
    /// `life_state_before` and `life_state_after`, of the life-state models.
    pub life_state: Option<(LifeStateTrace, LifeStateTrace)>,
    /// Whether `family_separation_pressure` there is the int `discontent_stress_rate` of the
    /// config (a separated family), which Python keeps as an int.
    pub family_pressure_int: bool,
    pub components: Vec<(&'static str, f64)>,
    pub raw_stress_delta: f64,
    pub candidate_before_clip: f64,
    pub clipped_delta: f64,
}

/// The values of `LIFE_STATE_NAMES`.
pub type LifeStateTrace = [f64; 9];

/// `PsychologyUpdateResult.traces`: the rows `record_psychology` makes the document of, in the
/// order of the view's people, and the model's `need_pressure_threshold`.
#[derive(Clone, Debug)]
pub struct Traces {
    pub threshold: f64,
    pub rows: Vec<PersonTrace>,
}

/// The place of `family_separation_pressure` in `LIFE_STATE_NAMES`.
const FAMILY_PRESSURE: usize = 7;

const LIFE_STATE_NAMES: [&str; 9] = [
    "sleep_debt_minutes",
    "uncertainty",
    "reassurance",
    "discontent",
    "positive_affect",
    "negative_affect",
    "hygiene_need",
    "family_separation_pressure",
    "information_belief_pressure",
];

fn queue_source_of(agent: &ViewAgent) -> Option<&'static str> {
    match agent.queue_kind.as_deref() {
        Some("toilet_queue") => Some("ASM-TOILET-SERVICE-DURATION-V1"),
        Some("resource_service_queue") => Some("ASM-H02-RESOURCE-SERVICE-V1"),
        _ => None,
    }
}

/// `view.facilities[agent.queue_target_id]`, if it is there.
fn queue_facility<'a>(data: &'a ViewData, agent: &ViewAgent) -> Option<&'a ViewFacility> {
    let target = agent.queue_target_id.as_deref()?;
    let place = data.facilities.binary_search_by(|facility| (*facility.id).cmp(target)).ok()?;
    Some(&data.facilities[place])
}

fn before_after(before: f64, after: f64) -> V {
    let mut item = Doc::with_capacity(3);
    item.set("before", float(before));
    item.set("after", float(after));
    item.set("delta", float(after - before));
    item.done()
}

/// A person's part of the psychology document (`AgentPsychologyTrace.to_dict`).
fn person_document(data: &ViewData, row: &PersonTrace, threshold: f64, stress_after: f64, social_delta: f64, source_id: &V) -> V {
    let agent = &data.agents[row.index];
    let zone = data.zone_of(agent);
    let facility = queue_facility(data, agent);
    let queue_source = queue_source_of(agent);
    let facility_events = facility.map_or(&[][..], |facility| &facility.facility_event_context_ids);

    let mut causes = Vec::new();
    {
        let mut cause = Doc::new();
        cause.set("kind", text("unmet_need_pressure"));
        cause.set("need", text(row.highest_need));
        cause.set("value", float(row.highest_need_value));
        cause.set("threshold", float(threshold));
        cause.set("excess_above_threshold", float(row.need_pressure));
        causes.push(cause.done());
    }
    if agent.queue_task_id.is_some() {
        let mut cause = Doc::new();
        cause.set("kind", text("facility_queue_wait"));
        cause.set("queue_kind", option_text(agent.queue_kind.as_deref()));
        cause.set("queue_task_id", option_text(agent.queue_task_id.as_deref()));
        cause.set("source_id", option_text(queue_source));
        cause.set("facility_id", option_text(agent.queue_target_id.as_deref()));
        cause.set("wait_minutes", int(agent.queue_wait_minutes));
        cause.set("facility_capacity", facility.map_or(V::Null, |facility| int(facility.capacity)));
        cause.set("facility_working_units", facility.map_or(V::Null, |facility| int(facility.working_units)));
        cause.set("capacity_event_context_ids", texts(facility_events));
        causes.push(cause.done());
    }
    if agent.support_request_id.is_some() {
        let mut cause = Doc::new();
        cause.set("kind", text("staff_support_wait"));
        cause.set("support_request_id", option_text(agent.support_request_id.as_deref()));
        cause.set("status", option_text(agent.support_request_status.map(status_text)));
        cause.set("assigned_staff_id", option_text(agent.support_assigned_staff_id.as_deref()));
        cause.set("wait_minutes", int(agent.support_wait_minutes));
        causes.push(cause.done());
    }
    let environmental = row.environmental_stress > 0.0;
    if environmental {
        let mut cause = Doc::new();
        cause.set("kind", text("experimental_environmental_stress"));
        cause.set("stress_per_minute", float(row.environmental_stress));
        cause.set("stressor_event_ids", texts(&zone.environmental_stressor_event_ids));
        cause.set("assumption_source_id", option_text(zone.environmental_assumption_source_id.as_deref()));
        causes.push(cause.done());
    }
    let sources = &agent.misinformation_effect_sources;
    let belief_pressure = agent.misinformation_belief_pressure;
    let of_sources = |field: fn(&crate::rules::world_view::MisinformationSource) -> &Arc<str>| {
        V::List(sources.iter().map(|source| V::Str(field(source).clone())).collect())
    };
    if !sources.is_empty() || belief_pressure > 0.0 {
        let mut cause = Doc::new();
        cause.set("kind", text("active_false_claim_belief"));
        cause.set("pressure", float(belief_pressure));
        cause.set("message_ids", of_sources(|source| &source.message_id));
        cause.set("origin_event_ids", of_sources(|source| &source.origin_event_id));
        cause.set("belief_event_ids", of_sources(|source| &source.last_event_id));
        cause.set("source_ids", of_sources(|source| &source.source_id));
        causes.push(cause.done());
    }
    let noise_events = &zone.zone_environment_modifier_event_ids;
    if !noise_events.is_empty() {
        let mut cause = Doc::new();
        cause.set("kind", text("experimental_zone_noise_reduced_rest_efficiency"));
        cause.set("ambient_noise_level", float(zone.ambient_noise_level));
        cause.set("rest_efficiency_multiplier", float(zone.rest_efficiency_multiplier));
        cause.set("modifier_event_ids", texts(noise_events));
        cause.set("assumption_ids", texts(&zone.zone_environment_assumption_ids));
        causes.push(cause.done());
    }

    let mut components = Doc::with_capacity(row.components.len() + 3);
    for &(name, amount) in &row.components {
        components.set(name, float(amount));
    }
    if row.life_state_feedback_stress != 0.0 {
        components.set("life_state_feedback", float(row.life_state_feedback_stress));
    }
    if row.information_uncertainty_stress != 0.0 {
        components.set("information_belief_via_uncertainty", float(row.information_uncertainty_stress));
    }
    if environmental {
        components.set("experimental_environmental_stress", float(row.environmental_stress));
    }

    let mut references = Doc::new();
    references.set("psychology_assumption_id", source_id.clone());
    references.set("queue_task_id", option_text(agent.queue_task_id.as_deref()));
    references.set("queue_source_id", option_text(queue_source));
    references.set("facility_event_context_ids", texts(facility_events));
    references.set("support_request_id", option_text(agent.support_request_id.as_deref()));
    references.set(
        "support_source_id",
        option_text(agent.support_request_id.as_ref().map(|_| "ASM-STAFF-SUPPORT-LIFECYCLE-V1")),
    );
    if environmental {
        references.set("environmental_assumption_id", option_text(zone.environmental_assumption_source_id.as_deref()));
        references.set("environmental_stressor_event_ids", texts(&zone.environmental_stressor_event_ids));
    }

    let mut needs = Doc::with_capacity(row.needs.len());
    for &(name, before, after) in &row.needs {
        needs.set(name, before_after(before, after));
    }
    let mut stress = Doc::with_capacity(9);
    stress.set("before", float(row.stress_before));
    stress.set("after", float(stress_after));
    stress.set("applied_delta", float(stress_after - row.stress_before));
    stress.set("raw_delta", float(row.raw_stress_delta));
    stress.set("candidate_before_clip", float(row.candidate_before_clip));
    stress.set("psychology_clipped_delta", float(row.clipped_delta));
    stress.set("social_delta", float(social_delta));
    stress.set("clip_delta", float(row.clipped_delta - row.raw_stress_delta));
    stress.set("components", components);

    let mut document = Doc::with_capacity(10);
    document.set("agent_id", V::Str(agent.id.clone()));
    document.set("needs", needs);
    document.set("stress", stress);
    document.set("direct_causes", V::List(causes));
    document.set("source_references", references);
    if !noise_events.is_empty() {
        let mut item = Doc::new();
        item.set("ambient_noise_level", float(zone.ambient_noise_level));
        item.set("rest_efficiency_multiplier", float(zone.rest_efficiency_multiplier));
        item.set("modifier_event_ids", texts(noise_events));
        item.set("assumption_ids", texts(&zone.zone_environment_assumption_ids));
        item.set("source_type", text("assumption"));
        document.set("zone_environment", item);
    }
    if !zone.condition_modifier_event_ids.is_empty() || !zone.condition_assumption_ids.is_empty() {
        let mut item = Doc::new();
        item.set("ventilation_effectiveness", float(zone.ventilation_effectiveness));
        item.set("lighting_level", float(zone.lighting_level));
        item.set("privacy_level", float(zone.privacy_level));
        item.set("temperature_c", float(zone.temperature_c));
        item.set("modifier_event_ids", texts(&zone.condition_modifier_event_ids));
        item.set("assumption_ids", texts(&zone.condition_assumption_ids));
        item.set("source_type", text("assumption"));
        document.set("environment_conditions", item);
    }
    if let Some((before, after)) = &row.life_state {
        let mut item = Doc::with_capacity(LIFE_STATE_NAMES.len());
        for (place, name) in LIFE_STATE_NAMES.into_iter().enumerate() {
            if place == FAMILY_PRESSURE && row.family_pressure_int {
                // `after - before` of two equal ints: the int 0.
                let mut entry = Doc::with_capacity(3);
                entry.set("before", int(before[place] as i64));
                entry.set("after", int(after[place] as i64));
                entry.set("delta", int(after[place] as i64 - before[place] as i64));
                item.set(name, entry);
                continue;
            }
            item.set(name, before_after(before[place], after[place]));
        }
        document.set("life_state", item);
    }
    if !sources.is_empty() || belief_pressure != 0.0 {
        let mut records = Vec::with_capacity(sources.len());
        for source in sources {
            let mut record = Doc::with_capacity(5);
            record.set("message_id", V::Str(source.message_id.clone()));
            record.set("origin_event_id", V::Str(source.origin_event_id.clone()));
            record.set("belief_event_id", V::Str(source.last_event_id.clone()));
            record.set("source_id", V::Str(source.source_id.clone()));
            record.set("origin_minute", int(source.time));
            records.push(record.done());
        }
        let mut item = Doc::with_capacity(3);
        item.set("active_false_claim_pressure", float(belief_pressure));
        item.set("source_records", V::List(records));
        item.set("effect_kind", text("persistent_belief_state_input"));
        document.set("information_belief_effect", item);
    }
    document.done()
}

// ---- A minute's psychology -----------------------------------------------------------------------

/// `math.fsum` of one number of each person.
fn total(values: impl Iterator<Item = f64>) -> Result<f64> {
    let values: Vec<f64> = values.collect();
    py_fsum(&values).ok_or_else(|| Error::unsupported("a total of the trace is not finite"))
}

/// What `_record_psychology_trace` reads besides the rows: the model's `source_id`, the result's
/// `policy_version` and the minute, each as the Python value it was.
pub struct TraceInputs<'a> {
    pub source_id: &'a V,
    pub policy_version: &'a V,
    pub minute: &'a V,
}

/// The psychology document of a minute and its `agent_contributions` (`trace_document` on
/// `traced_result`): each person's stress after is the final delta's (`finals`, ID and stress), so
/// `social_stress_delta` is what the social step added. A full trace keeps every person, a summary
/// the 20 whose stress changed most (then by ID); the component totals are over everyone.
pub fn psychology_document(view: &ViewData, traces: &Traces, finals: &[(&str, f64)], level: TraceLevel, inputs: &TraceInputs<'_>) -> Result<Trace> {
    let rows = &traces.rows;
    // `final_by_agent`: a later delta of one person replaces an earlier one, as in a dict.
    let mut by_agent: FxHashMap<&str, f64> = FxHashMap::with_capacity_and_hasher(finals.len(), Default::default());
    for &(id, stress) in finals {
        by_agent.insert(id, stress);
    }
    let mut stress_after = Vec::with_capacity(rows.len());
    for row in rows {
        match by_agent.get(&*view.agents[row.index].id) {
            Some(stress) => stress_after.push(*stress),
            None => return Err(Error::unsupported("a person has no final delta")),
        }
    }

    // The order: most changed first, then by ID.
    let mut order: Vec<usize> = (0..rows.len()).collect();
    let change = |place: usize| -(stress_after[place] - rows[place].stress_before).abs();
    order.sort_by(|&first, &second| {
        change(first).partial_cmp(&change(second)).unwrap_or(Ordering::Equal).then_with(|| {
            view.agents[rows[first].index].id.as_bytes().cmp(view.agents[rows[second].index].id.as_bytes())
        })
    });
    let kept = if level == TraceLevel::Full { order.len() } else { order.len().min(SUMMARY_ITEMS) };

    let mut totals = Doc::with_capacity(8);
    totals.set("unmet_need_pressure", float(total(rows.iter().map(|row| row.need_pressure_stress))?));
    totals.set("facility_queue_wait", float(total(rows.iter().map(|row| row.queue_wait_stress))?));
    totals.set("staff_support_wait", float(total(rows.iter().map(|row| row.support_wait_stress))?));
    totals.set("recovery", float(total(rows.iter().map(|row| row.recovery_stress))?));
    totals.set("natural_recovery", float(total(rows.iter().map(|row| row.natural_recovery_stress))?));
    let applied = total(rows.iter().zip(&stress_after).map(|(row, after)| after - row.stress_before))?;
    totals.set("applied_stress_delta", float(applied));
    let environmental = total(rows.iter().map(|row| row.environmental_stress))?;
    if environmental != 0.0 {
        totals.set("experimental_environmental_stress", float(environmental));
    }
    let feedback = total(rows.iter().map(|row| row.life_state_feedback_stress))?;
    if feedback != 0.0 {
        totals.set("life_state_feedback", float(feedback));
    }

    let mut agents = Vec::with_capacity(kept);
    for &place in &order[..kept] {
        let row = &rows[place];
        agents.push(person_document(
            view,
            row,
            traces.threshold,
            stress_after[place],
            stress_after[place] - row.stress_after,
            inputs.source_id,
        ));
    }
    let mut document = Doc::with_capacity(8);
    document.set("minute_index", inputs.minute.clone());
    document.set("policy_version", inputs.policy_version.clone());
    document.set("assumption_id", inputs.source_id.clone());
    document.set("social_propagation_is_separate", V::Bool(true));
    document.set("agent_count", count(rows.len()));
    document.set("truncated_agent_count", count(rows.len() - kept));
    document.set("component_totals", totals);
    document.set("agents", V::List(agents));

    let mut contributions = Vec::with_capacity(rows.len());
    for (row, after) in rows.iter().zip(&stress_after) {
        let mut item = Doc::with_capacity(6);
        item.set("agent_id", V::Str(view.agents[row.index].id.clone()));
        item.set("stress_before", float(row.stress_before));
        item.set("stress_after", float(*after));
        item.set("raw_stress_delta", float(row.raw_stress_delta));
        item.set("social_stress_delta", float(after - row.stress_after));
        let mut components = Doc::with_capacity(row.components.len());
        for &(name, amount) in &row.components {
            components.set(name, float(amount));
        }
        item.set("component_contributions", components);
        contributions.push(item.done());
    }
    Ok(Trace::Psychology { document: document.done(), contributions: V::List(contributions) })
}

/// `_record_psychology_trace` of the update phase: nothing when the Run keeps no trace, otherwise
/// the minute's document goes to the host's sink. It changes nothing but what the sink keeps.
pub fn record_psychology<H: Host>(
    host: &mut H,
    view: &ViewData,
    traces: &Traces,
    finals: &[(&str, f64)],
    inputs: &TraceInputs<'_>,
) -> std::result::Result<(), H::Error> {
    let level = host.trace_level()?;
    if level == TraceLevel::Off {
        return Ok(());
    }
    let document = psychology_document(view, traces, finals, level, inputs)?;
    host.trace(document)
}

// ---- Contacts ------------------------------------------------------------------------------------

/// `SocialContactTrace`.
#[derive(Clone)]
pub struct SocialContact {
    pub recipient: usize,
    pub source: usize,
    pub basis: Basis,
    pub minutes: i64,
    pub cooldown_remaining: i64,
    pub difference: f64,
    /// An int where the Python rule's `min` returned an int cap (see rules/social_propagation.rs).
    pub contribution: Num,
    pub weight: f64,
    pub aggregate_rate: f64,
    pub pre_pair_cap: f64,
    pub pair_cap: Num,
    pub aggregate_scale: f64,
}

/// `contact_basis` of `_contact_buckets`.
fn social_basis(basis: Basis) -> &'static str {
    match basis {
        Basis::Family => "family",
        Basis::SameZone => "same_zone",
        Basis::Nearby => "nearby_zone",
    }
}

/// `SocialPropagationResult.trace_document`, with the bounded sample of contacts it has always had.
/// `minute` and `policy_version` are the Python values the document holds.
pub fn social_document(
    minute: &V,
    policy_version: &V,
    ids: &[&str],
    contacts: &[SocialContact],
    contributing: i64,
    total_added: f64,
) -> V {
    let contact_of = |contact: &SocialContact| -> V {
        let mut item = Doc::with_capacity(12);
        item.set("recipient_id", text(ids[contact.recipient]));
        item.set("source_id", text(ids[contact.source]));
        item.set("contact_basis", text(social_basis(contact.basis)));
        item.set("consecutive_contact_minutes", int(contact.minutes));
        item.set("cooldown_remaining_minutes", int(contact.cooldown_remaining));
        item.set("stress_difference", float(contact.difference));
        item.set("stress_contribution", num(contact.contribution));
        item.set("influence_weight", float(contact.weight));
        item.set("aggregate_rate", float(contact.aggregate_rate));
        item.set("pre_pair_cap_contribution", float(contact.pre_pair_cap));
        item.set("pair_cap_contribution", num(contact.pair_cap));
        item.set("aggregate_scale", float(contact.aggregate_scale));
        item.done()
    };
    let kept = contacts.len().min(SUMMARY_ITEMS);
    let sample: Vec<V> = contacts[..kept].iter().map(contact_of).collect();
    let contributions: Vec<V> =
        contacts.iter().filter(|contact| contact.contribution.value() != 0.0).map(contact_of).collect();
    let channel = matches!(policy_version, V::Str(version) if &**version == "expressiveness-channel-openness-v1");
    let mut document = Doc::with_capacity(9);
    document.set("minute_index", minute.clone());
    document.set("policy_version", policy_version.clone());
    document.set("selected_contact_count", count(contacts.len()));
    document.set("contributing_contact_count", int(contributing));
    document.set("total_stress_added", float(total_added));
    document.set(
        "aggregation",
        text(if channel {
            "snapshot weighted synchrony; pair cap then recipient cap then [0,1] bound"
        } else {
            "legacy directed positive stress propagation"
        }),
    );
    document.set("truncated_contact_count", count(contacts.len() - kept));
    document.set("contacts", V::List(sample));
    document.set("contributions", V::List(contributions));
    document.done()
}
