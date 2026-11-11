//! Recording a decision with its outcome, the step `_record_outcome` of the propose phase
//! (`experiments/phases/propose.py`; moved to the core in lane P8 a5): the record, then the action's
//! lifecycle, then the person's memory, in that order.
//!
//! The record goes where the Run's sink takes it (`Host::decision_sink`): nothing for a silent sink;
//! the JSON bytes of `_IndexedMetricSink.record_decision` (`decision_bytes`, with the keys in the
//! order of its dict) for an indexed one; the document of `_decision_document` and the observed
//! state for `RunArtifactWriter` while its trace is on. Everything that can turn the record down
//! is read before the host takes it, and what this rule does not write itself (a sink that runs
//! `record_decision` in Python, an observed state that is not the view's person, a NaN, a large
//! observed state the sink measures, a tainted store, a household no row reads) is `Sunk::Python`:
//! the PyO3 crate then calls the sink's own `record_decision` with the Python proposal, and the
//! rules after it as here.
//!
//! The lifecycle and the memory are `action_lifecycle::commit_action` and
//! `agent_dynamics::remember_decision`, called directly. What they refuse has changed nothing and
//! is the KernelRefusal of their Python entries (`Error::refused` with `LIFECYCLE` or `MEMORY`), as
//! when the step called those entries.

use std::sync::Arc;

use crate::error::{Error, Result};
use crate::host::{Decision, DecisionSink, Host, TraceLevel};
use crate::json::Encoder;
use crate::rules::action_lifecycle::{self, ActiveActions};
use crate::rules::agent_dynamics::{self, Decision as Memory};
use crate::rules::behavior::{observed_value, status_text, Proposal};
use crate::rules::kit::PRESENCE;
use crate::rules::world_view::ViewData;
use crate::value::V;
use crate::world::{World, WorldCell};

/// `ActionProposal.trigger` of a scheduled decision.
pub const TRIGGER: &str = "scheduled_decision";
/// `len(json.dumps(observed))` is at most twice the length without its spaces, so a shorter one is
/// within the 16,384 of `record_decision`. A longer one is left to Python, which measures it.
pub const MAX_OBSERVED_BYTES: usize = 8192;
/// The label of the KernelRefusal of the lifecycle's rule.
pub const LIFECYCLE: &str = "action_lifecycle.record_action_proposal";
/// The label of the KernelRefusal of the memory's rule.
pub const MEMORY: &str = "agent_dynamics.record_proposal";

/// What a decision observed (`observed_state` of the step).
#[derive(Clone)]
pub enum Observed<'a> {
    /// `_agent_view_snapshot` of a person of a view, not yet made.
    Row { data: &'a ViewData, index: usize },
    /// A plain value (a dict, or None).
    Value(V),
    /// Anything else, which only the sink's own `record_decision` takes.
    Other,
}

/// A decision and its outcome, as `_record_outcome` takes them.
pub struct Recording<'a> {
    /// The view the proposal was made from.
    pub data: &'a ViewData,
    pub proposal: &'a Proposal,
    pub target_id: Option<&'a str>,
    /// After the proposal's minute.
    pub next_decision_at: i64,
    pub committed: bool,
    pub observed: &'a Observed<'a>,
    /// Whether `model_state` has `agent_dynamics_v1`.
    pub remembers: bool,
}

impl Recording<'_> {
    fn agent_id(&self) -> &str {
        &self.data.agents[self.proposal.index].id
    }

    fn minute(&self) -> i64 {
        self.data.minute_index
    }
}

/// Where the record went.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Sunk {
    /// The sink took it (or a silent sink, nothing).
    Recorded,
    /// Nothing was done: the sink's own `record_decision` takes it, with the Python proposal.
    Python,
}

// ---- The record as JSON ------------------------------------------------------------------------

fn option_text(out: &mut Encoder, value: Option<&str>) {
    match value {
        Some(value) => out.string(value),
        None => out.null(),
    }
}

fn text_list(out: &mut Encoder, items: &[Arc<str>]) {
    out.out.push(b'[');
    for (place, item) in items.iter().enumerate() {
        if place > 0 {
            out.out.push(b',');
        }
        out.string(item);
    }
    out.out.push(b']');
}

/// Writes `,"name":` before a value.
macro_rules! key {
    ($out:expr, $name:literal) => {
        $out.out.extend_from_slice(concat!(",\"", $name, "\":").as_bytes())
    };
}

/// `_agent_view_snapshot` as JSON, with its keys in that function's order (`observed_value`).
pub fn write_observed(out: &mut Encoder, data: &ViewData, index: usize) -> Result<()> {
    let agent = &data.agents[index];
    let zone = data.zone_of(agent);
    out.out.extend_from_slice(b"{\"agent_id\":");
    out.string(&agent.id);
    key!(out, "zone_id");
    out.string(&zone.id);
    key!(out, "activity");
    out.string(agent.activity.text());
    key!(out, "target_id");
    option_text(out, agent.target_id.as_deref());
    out.out.extend_from_slice(b",\"needs\":{\"toilet_urgency\":");
    out.try_float(agent.needs.toilet_urgency)?;
    key!(out, "hunger");
    out.try_float(agent.needs.hunger)?;
    key!(out, "fatigue");
    out.try_float(agent.needs.fatigue)?;
    key!(out, "hygiene_need");
    out.try_float(agent.body.hygiene_need)?;
    if data.resource_service_enabled {
        key!(out, "hydration_need");
        out.try_float(agent.needs.hydration_need)?;
    }
    out.out.extend_from_slice(b"},\"psychological\":{\"sleep_debt_minutes\":");
    out.try_float(agent.body.sleep_debt_minutes)?;
    key!(out, "uncertainty");
    out.try_float(agent.body.uncertainty)?;
    key!(out, "reassurance");
    out.try_float(agent.body.reassurance)?;
    key!(out, "discontent");
    out.try_float(agent.body.discontent)?;
    key!(out, "positive_affect");
    out.try_float(agent.body.positive_affect)?;
    key!(out, "negative_affect");
    out.try_float(agent.body.negative_affect)?;
    out.out.extend_from_slice(b"},\"sanitation_level\":");
    out.try_float(zone.sanitation_level)?;
    key!(out, "stress");
    out.try_float(agent.stress)?;
    key!(out, "trust");
    out.try_float(agent.trust)?;
    out.out.extend_from_slice(b",\"queue\":{\"task_id\":");
    option_text(out, agent.queue_task_id.as_deref());
    key!(out, "kind");
    option_text(out, agent.queue_kind.as_deref());
    key!(out, "target_id");
    option_text(out, agent.queue_target_id.as_deref());
    key!(out, "wait_minutes");
    out.int(agent.queue_wait_minutes);
    out.out.extend_from_slice(b"},\"support\":{\"request_id\":");
    option_text(out, agent.support_request_id.as_deref());
    key!(out, "status");
    option_text(out, agent.support_request_status.map(status_text));
    key!(out, "assigned_staff_id");
    option_text(out, agent.support_assigned_staff_id.as_deref());
    key!(out, "wait_minutes");
    out.int(agent.support_wait_minutes);
    key!(out, "cooldown_remaining_minutes");
    out.int(agent.support_cooldown_remaining_minutes);
    out.out.push(b'}');
    if !zone.zone_environment_modifier_event_ids.is_empty() || !zone.demand_modifiers.is_empty() {
        out.out.extend_from_slice(b",\"zone_environment\":{\"ambient_noise_level\":");
        out.try_float(zone.ambient_noise_level)?;
        key!(out, "rest_efficiency_multiplier");
        out.try_float(zone.rest_efficiency_multiplier)?;
        key!(out, "modifier_event_ids");
        text_list(out, &zone.zone_environment_modifier_event_ids);
        key!(out, "assumption_ids");
        text_list(out, &zone.zone_environment_assumption_ids);
        out.out.extend_from_slice(b",\"demand_modifiers\":[");
        for (place, item) in zone.demand_modifiers.iter().enumerate() {
            if place > 0 {
                out.out.push(b',');
            }
            out.out.extend_from_slice(b"{\"action\":");
            out.string(&item.action);
            key!(out, "probability_multiplier");
            out.try_float(item.probability_multiplier)?;
            key!(out, "duration_multiplier");
            out.try_float(item.duration_multiplier)?;
            key!(out, "event_ids");
            text_list(out, &item.modifier_event_ids);
            key!(out, "assumption_ids");
            text_list(out, &item.assumption_ids);
            out.out.push(b'}');
        }
        out.out.extend_from_slice(b"]}");
    }
    out.out.push(b'}');
    Ok(())
}

/// `current_household_id` as the indexed sink writes it: the household of the person's record in
/// `population_presence_v1` (None for none), or the profile's group without a dict there or with a
/// record that is not one. A value no row reads is refused (the sink's rule then refuses it).
fn household(world: &World, agent_id: &str, group_id: Option<&Arc<str>>) -> Result<Option<Arc<str>>> {
    let presence = world.slots.try_value(PRESENCE)?;
    let Some(V::Map(presence)) = presence.as_deref() else {
        return Ok(group_id.cloned());
    };
    let Some(people) = presence.get("people") else {
        return Ok(None);
    };
    let V::Map(people) = people else {
        return Err(Error::unsupported("the presence's people are not a dict"));
    };
    let Some(person) = people.get(agent_id) else {
        return Ok(None);
    };
    let V::Map(person) = person else {
        return Ok(group_id.cloned());
    };
    match person.get("household_id") {
        None | Some(V::Null) => Ok(None),
        Some(V::Str(household)) => Ok(Some(household.clone())),
        Some(_) => Err(Error::unsupported("a value is not a str")),
    }
}

/// What `_IndexedMetricSink.record_decision` keeps of a decision that the view does not hold: the
/// person's zone and household now, and the sink's source of the policy.
pub struct DecisionRecord<'a> {
    pub recording: &'a Recording<'a>,
    pub zone_id: Arc<str>,
    pub household_id: Option<Arc<str>>,
    /// `policy_sources.get(policy_version)`.
    pub source_id: Option<Arc<str>>,
}

/// The record of a decision, read from the world.
pub fn decision_record<'a>(world: &World, recording: &'a Recording<'a>, source_id: Option<Arc<str>>) -> Result<DecisionRecord<'a>> {
    let agent_id = recording.agent_id();
    let (zone_id, group_id) = {
        let rows = world.rows()?;
        let Some(agent) = rows.agents.get(agent_id) else {
            return Err(Error::unsupported("the person is not in the world"));
        };
        (agent.zone_id.clone(), agent.profile.group_id.clone())
    };
    let household_id = household(world, agent_id, group_id.as_ref())?;
    Ok(DecisionRecord { recording, zone_id, household_id, source_id })
}

/// What `_IndexedMetricSink.record_decision` appends, as JSON with the keys in that order, with the
/// observed state of `data`'s person `index`.
pub fn decision_bytes(record: &DecisionRecord<'_>, data: &ViewData, index: usize) -> Result<Vec<u8>> {
    let recording = record.recording;
    let proposal = recording.proposal;
    let outcome = &proposal.outcome;
    let mut out = Encoder::with_capacity(3072);
    out.out.extend_from_slice(b"{\"record_kind\":\"actions\",\"minute\":");
    out.int(recording.minute());
    key!(out, "agent_id");
    out.string(recording.agent_id());
    key!(out, "household_id");
    option_text(&mut out, record.household_id.as_deref());
    key!(out, "zone_id");
    out.string(&record.zone_id);
    key!(out, "action");
    out.string(outcome.action.value());
    key!(out, "target_id");
    option_text(&mut out, recording.target_id);
    key!(out, "trigger");
    out.string(TRIGGER);
    key!(out, "policy_version");
    out.string(&proposal.common.policy_version);
    key!(out, "source_id");
    option_text(&mut out, record.source_id.as_deref());
    key!(out, "observed_state_hash");
    out.string(&proposal.common.state_hash);
    key!(out, "observed_state");
    let start = out.out.len();
    write_observed(&mut out, data, index)?;
    if out.out.len() - start > MAX_OBSERVED_BYTES {
        return Err(Error::unsupported("the observed state is long"));
    }
    out.out.extend_from_slice(b",\"observed_state_status\":\"recorded\",\"candidates\":[");
    for (place, candidate) in outcome.candidates.iter().enumerate() {
        if place > 0 {
            out.out.push(b',');
        }
        out.out.extend_from_slice(b"{\"action\":");
        out.string(candidate.action.value());
        out.out.extend_from_slice(if candidate.eligible { b",\"eligible\":true" } else { b",\"eligible\":false" });
        key!(out, "trigger");
        out.string(&proposal.trigger_of(recording.data, candidate)?);
        key!(out, "utility");
        out.try_float(candidate.utility)?;
        key!(out, "probability");
        out.try_float(candidate.probability)?;
        key!(out, "urgency_priority");
        out.int(candidate.priority);
        out.out.push(b'}');
    }
    out.out.extend_from_slice(b"],\"candidate_count\":");
    out.int(outcome.candidates.len() as i64);
    out.out.extend_from_slice(b",\"candidates_truncated\":false,\"next_decision_at\":");
    out.int(recording.next_decision_at);
    out.out.extend_from_slice(
        b",\"status\":\"selected_proposal\",\"execution_evidence\":\
          \"consult event history; selection is not proof of completion\"}",
    );
    Ok(out.out)
}

// ---- The record of a trace ---------------------------------------------------------------------

fn text(value: &str) -> V {
    V::Str(Arc::from(value))
}

/// `_decision_document` of `inspection/traces.py`, without the observed state its writer adds last.
pub fn decision_document(recording: &Recording<'_>, full: bool) -> Result<V> {
    let data = recording.data;
    let proposal = recording.proposal;
    let outcome = &proposal.outcome;
    let action = outcome.action.value();
    let version = &proposal.common.policy_version;
    let key = proposal
        .rng_key(data)
        .into_iter()
        .map(|part| match part {
            Ok(part) => text(part),
            Err(number) => V::Int(number),
        })
        .collect();
    let source_id = if version.starts_with("priority-rule-") {
        "ASM-PRIORITY-RULE-BEHAVIOR-V1"
    } else if version.starts_with("sleep-aware-utility-softmax-v2") {
        "ASM-SLEEP-AWARE-BEHAVIOR-V2"
    } else {
        "ASM-UTILITY-BEHAVIOR-V1"
    };
    let mut document: Vec<(&str, V)> = vec![
        ("minute_index", V::Int(data.minute_index)),
        ("agent_id", text(recording.agent_id())),
        ("action", text(action)),
        ("target_id", recording.target_id.map_or(V::Null, text)),
        ("observed_state_hash", V::Str(proposal.common.state_hash.clone())),
        ("trigger", text(TRIGGER)),
        ("selected_action", text(action)),
        ("selected_probability", V::Float(outcome.selection_probability)),
        ("choice_value", V::Float(outcome.choice_value)),
        ("rng_key", V::List(key)),
        ("next_decision_at", V::Int(recording.next_decision_at)),
        ("policy_version", V::Str(version.clone())),
        ("source_type", text("assumption")),
        ("source_id", text(source_id)),
    ];
    if full {
        let mut candidates = Vec::with_capacity(outcome.candidates.len());
        for candidate in &outcome.candidates {
            candidates.push(V::Map(
                [
                    ("action", text(candidate.action.value())),
                    ("eligible", V::Bool(candidate.eligible)),
                    ("trigger", text(&proposal.trigger_of(data, candidate)?)),
                    ("utility", V::Float(candidate.utility)),
                    ("urgency_priority", V::Int(candidate.priority)),
                    ("probability", V::Float(candidate.probability)),
                ]
                .into_iter()
                .map(|(name, value)| (Arc::from(name), value))
                .collect(),
            ));
        }
        document.push(("candidates", V::List(candidates)));
    }
    Ok(V::Map(document.into_iter().map(|(name, value)| (Arc::from(name), value)).collect()))
}

// ---- The step ----------------------------------------------------------------------------------

/// The decision's record, as the Run's sink takes it. Nothing of the world is borrowed while the host
/// runs (rule R6).
pub fn record<C: WorldCell, H: Host>(cell: &C, host: &mut H, recording: &Recording<'_>) -> std::result::Result<Sunk, H::Error> {
    match host.decision_sink()? {
        DecisionSink::Python => Ok(Sunk::Python),
        DecisionSink::Silent => Ok(Sunk::Recorded),
        DecisionSink::Artifacts => {
            let observed = match recording.observed {
                Observed::Row { data, index } => observed_value(data, *index),
                Observed::Value(value) => value.clone(),
                Observed::Other => return Ok(Sunk::Python),
            };
            let full = host.trace_level()? == TraceLevel::Full;
            let document = decision_document(recording, full)?;
            host.decision(Decision::Document { document, observed })?;
            Ok(Sunk::Recorded)
        }
        DecisionSink::Indexed => {
            let Observed::Row { data, index } = recording.observed else {
                return Ok(Sunk::Python);
            };
            let Some(source) = host.decision_source(&recording.proposal.common.policy_version)? else {
                return Ok(Sunk::Python);
            };
            let written = cell
                .with(|world| decision_record(world, recording, source).and_then(|record| decision_bytes(&record, data, *index)))
                .and_then(|written| written);
            let raw = match written {
                Ok(raw) => raw,
                Err(Error::Unsupported(_)) => return Ok(Sunk::Python),
                Err(error) => return Err(error.into()),
            };
            host.decision(Decision::Indexed(raw))?;
            Ok(Sunk::Recorded)
        }
    }
}

/// The action's lifecycle of a committed decision (`record_action_proposal`).
pub fn commit(world: &World, active: &mut ActiveActions, recording: &Recording<'_>) -> Result<()> {
    if !recording.committed {
        return Ok(());
    }
    let proposal = action_lifecycle::Proposal {
        agent_id: recording.agent_id(),
        action: recording.proposal.outcome.action.value(),
        target_id: recording.target_id,
        minute: recording.minute(),
        trigger: TRIGGER,
    };
    action_lifecycle::commit_action(world, active, &proposal)
}

/// The person's memory of the decision (`record_proposal`), when the world keeps one.
pub fn remember(world: &World, recording: &Recording<'_>) -> Result<()> {
    if !recording.remembers {
        return Ok(());
    }
    let decision = Memory {
        agent_id: recording.agent_id(),
        action: recording.proposal.outcome.action.value(),
        target_id: recording.target_id,
        minute: recording.minute(),
        next_decision_at: recording.next_decision_at,
        trigger: Some(TRIGGER),
    };
    agent_dynamics::remember_decision(world, &decision)
}

/// `_record_outcome`: the record, the lifecycle and the memory. `Sunk::Python` has done nothing.
pub fn record_outcome<C: WorldCell, H: Host>(
    cell: &C,
    host: &mut H,
    active: &mut ActiveActions,
    recording: &Recording<'_>,
) -> std::result::Result<Sunk, H::Error> {
    if record(cell, host, recording)? == Sunk::Python {
        return Ok(Sunk::Python);
    }
    cell.with(|world| commit(world, active, recording))
        .and_then(|done| done)
        .map_err(|error| Error::refused(LIFECYCLE, error))?;
    cell.with(|world| remember(world, recording)).and_then(|done| done).map_err(|error| Error::refused(MEMORY, error))?;
    Ok(Sunk::Recorded)
}
