//! The rule of `SocialInteractionHandler` in `events/handlers.py`. Nearly every social event is a
//! social action between two people (`reassurance_exchange`, `family_care`, `voluntary_support`):
//! it reads the event's row, the entity rows, `body_state_v1` and `population_presence_v1`,
//! computes, and writes the outcome to the entity rows, whose objects follow when the world is given
//! back. It calls no Python and draws nothing. The other types of interaction (`complaint_response`,
//! `cooperation`, `information_reaction`, an untyped event) are rare and come at the end; they read
//! the same rows, and the record of the contact they answer, which the PyO3 crate looks up in the
//! EventLog (`Inputs::trigger`). Moved to the core in lane P8 d1.
//!
//! An event that does not apply gets the Python rule's text, which is saved as the event's `detail`,
//! from the Python rule's first failing check (docs/rust-port/lanes/l8-texts-events.md). Unsupported
//! comes before any change, for what Rust cannot decide as Python does; it is a `KernelRefusal`.
//!
//! The values of the payload are the event row's `V`s, compared as Python compares a JSON value
//! with a str: a value of another type equals no str. A mapping used as a key raises Python's
//! `TypeError` of an unhashable `mappingproxy`, as the frozen payload's value does.

use std::sync::Arc;

use crate::config::information::InformationSpread;
use crate::config::SectionId;
use crate::host::Host;
use crate::rules::information_events::OTHER_DOCUMENT;
use crate::entities::{AgentRow, RequestStatus};
use crate::error::{Error, Outcome, Result};
use crate::events::{EventKind, EventRow, EventStatus};
use crate::mathx::{py_max, py_min};
use crate::rules::common_operations::Logged;
use crate::rules::kit::{self, Held, Kept};
use crate::rules::social_dialogue::context_verdict;
use crate::rules::social_network::action_relief;
use crate::state::body_state::{BodyRow, NAME as BODY_STATE};
use crate::state::information::{InformationRow, NAME as INFORMATION, SCHEMA_VERSION as INFORMATION_VERSION};
use crate::value::V;
use crate::world::{World, WorldCell};

const SOURCE_ID: &str = "ASM-EVENT-TRIGGERED-SOCIAL-INTERACTIONS-V1";
const PAYLOAD_FIELDS: usize = 8;
/// A helper's need rises by this, and the target must need this much more than the helper.
const ACTOR_FATIGUE_COST: f64 = 0.002;
const REQUIRED_NEED_MARGIN: f64 = 0.15;
const PARTICIPANT_TRUST_SHARE: f64 = 0.5;
const ACTOR_TRUST_SHARE: f64 = 0.25;
const MAX_DURATION_MINUTES: i64 = 1440;

/// What the rule says of an event that does not apply: its outcome and text. None: it applied.
pub type Said = Option<(Outcome, &'static str)>;

/// What the rare types read outside the world and the event, gathered before the world is borrowed.
#[derive(Default)]
pub struct Inputs {
    /// `event_log.find(trigger_event_id)` of a cooperation or an information reaction whose trigger
    /// is a str: the ID, and the record if there is one.
    pub trigger: Option<(Arc<str>, Option<Logged>)>,
    /// What an information reaction reads of the information model: the config kept for the
    /// configs' stamp, and whether `model_state` holds the namespace (`Absent` stands for any value
    /// that is not a dict too).
    pub information: Option<(Kept<InformationSpread>, Held)>,
}

impl Inputs {
    /// What the rare types read, read by the core through the host (lane P9-1a): the EventLog's
    /// record of the contact a cooperation or an information reaction answers
    /// (`Host::find_record`), and the information model of a reaction (H1, H2). Nothing of the
    /// world is borrowed while the host runs.
    pub fn read<C: WorldCell, H: Host>(cell: &C, host: &mut H, event: &EventRow) -> std::result::Result<Self, H::Error> {
        let mut inputs = Inputs::default();
        if let Some(trigger) = trigger_of(event) {
            inputs.trigger = Some((trigger.clone(), host.find_record(trigger)?));
        }
        if interaction_type(event) == Some("information_reaction") {
            let holding = kit::holding(cell, host)?;
            let config = kit::kept_section(cell, host, &holding, SectionId::InformationSpread, |configs| &configs.information_spread)?;
            inputs.information = Some((config, holding.held_dict(INFORMATION, OTHER_DOCUMENT)));
        }
        Ok(inputs)
    }
}

/// `SocialInteractionHandler.apply` with its inputs read through the host (lane P9-1a).
pub fn apply<C: WorldCell, H: Host>(cell: &C, host: &mut H, event: &EventRow) -> std::result::Result<Said, H::Error> {
    let inputs = Inputs::read(cell, host, event)?;
    Ok(cell.with(|world| apply_event(world, event, &inputs))??)
}

/// The type of interaction of an event's payload, for the PyO3 crate to know which inputs it reads:
/// `payload.get("interaction_type")` when it is a str.
pub fn interaction_type(event: &EventRow) -> Option<&str> {
    match kit::payload(event, "interaction_type") {
        Some(V::Str(kind)) => Some(kind),
        _ => None,
    }
}

/// The trigger a cooperation or an information reaction reads in the EventLog, if it is a str.
pub fn trigger_of(event: &EventRow) -> Option<&Arc<str>> {
    match interaction_type(event) {
        Some("cooperation" | "information_reaction") => match kit::payload(event, "trigger_event_id") {
            Some(V::Str(trigger)) => Some(trigger),
            _ => None,
        },
        _ => None,
    }
}

fn failure(text: &'static str) -> Result<Said> {
    Ok(Some((Outcome::Failed, text)))
}

fn suppression(text: &'static str) -> Result<Said> {
    Ok(Some((Outcome::Suppressed, text)))
}

static NULL: V = V::Null;

/// `payload.get(key)`: None (`V::Null`) for a key it does not have.
fn get<'a>(payload: &'a V, key: &str) -> &'a V {
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

/// `value == text` as Python compares a JSON value with a str.
fn text_is(value: &V, text: &str) -> bool {
    matches!(value, V::Str(value) if &**value == text)
}

/// `event.<field> == value` of an event's optional str and a JSON value: None equals only None.
fn same_text(field: &Option<Arc<str>>, value: &V) -> bool {
    match (field, value) {
        (None, V::Null) => true,
        (Some(field), V::Str(value)) => field == value,
        _ => false,
    }
}

/// `hash(value)` of a frozen payload value: a mapping proxy, or a tuple holding one, has none.
#[track_caller]
fn hashable(value: &V) -> Result<()> {
    match value {
        V::Map(_) => Err(Error::Type("unhashable type: 'mappingproxy'".to_owned())),
        V::List(items) => items.iter().try_for_each(hashable),
        _ => Ok(()),
    }
}

/// `mapping.get(value)` of a mapping of str keys: the key, if the value is a str.
#[track_caller]
fn key_of(value: &V) -> Result<Option<&str>> {
    hashable(value)?;
    Ok(str_of(value).map(|text| &**text))
}

/// `value not in {names}`, as Python's set decides it.
#[track_caller]
fn not_in(value: &V, names: &[&str]) -> Result<bool> {
    Ok(!key_of(value)?.is_some_and(|text| names.contains(&text)))
}

/// `type(value) is int and 1 <= value <= 1440`. An int past 64 bits is outside.
fn is_duration(value: &V) -> bool {
    matches!(value, V::Int(minutes) if (1..=MAX_DURATION_MINUTES).contains(minutes))
}

/// `isinstance(stress_threshold, (int, float)) and not isinstance(it, bool) and 0.0 <= it <= 1.0`.
fn threshold_of(value: &V) -> Result<Option<f64>> {
    Ok(crate::rules::information_events::real_number(value)?.filter(|threshold| (0.0..=1.0).contains(threshold)))
}

/// `world.agents.get(agent_id) is not None`.
fn agent_is_known(world: &World, agent_id: &str) -> Result<bool> {
    Ok(world.rows()?.agents.contains_key(agent_id))
}

fn immediate_need(row: &AgentRow) -> f64 {
    // max(a, b, ...) keeps the first of equal values, as the fold does.
    [row.needs.toilet_urgency, row.needs.hunger, row.needs.fatigue, row.needs.hydration_need]
        .into_iter()
        .fold(row.stress, py_max)
}

/// `model_state.get("body_state_v1")` as its row, None without it; a tainted one is refused.
fn body_of(world: &World) -> Result<Option<crate::cell::ShardRef<'_, BodyRow>>> {
    if kit::namespace_absent(world, BODY_STATE)? {
        return Ok(None);
    }
    world.row::<BodyRow>(BODY_STATE).map(Some)
}

const BAD_TIME: &str = "typed social interaction provenance or time is invalid";
const NO_REASON: &str = "typed social interaction requires a reason code";
const UNKNOWN_AGENT: &str = "typed social interaction references an unknown Agent";
const BAD_CONTEXT: &str = "social dialogue state context is invalid";
const NOT_ALLOWED: &str = "typed social interaction type is not allowed";
const ACTION_FIELDS: &str = "autonomous social action payload fields do not match";
const NOT_COLOCATED: &str = "social action participants are no longer co-located";
const OFF_TOPIC: &str = "social action does not match its typed topic";
const ACTION_REASON: &str = "social action reason code is not allowed";
const ACTION_DURATION: &str = "social action duration must be within 1..1440 minutes";
const NOT_SAME_HOUSEHOLD: &str = "family support target is no longer in the same household";
const NOT_HIGHER_NEED: &str = "the selected person no longer has a higher immediate need";

/// `SocialInteractionHandler.apply`: None for an applied event, or the outcome and text of one that
/// does not apply. The event's payload is a mapping (an `EventRow` of a `ScheduledEvent`).
pub fn apply_event(world: &World, event: &EventRow, inputs: &Inputs) -> Result<Said> {
    let payload = &event.payload;
    let interaction = kit::mapping_get(payload, "interaction_type")?.filter(|value| !matches!(value, V::Null));
    let Some(interaction) = interaction else {
        return untyped_event(world, payload);
    };
    let kind = str_of(interaction).map(|kind| &**kind);
    let (expected_action, expected_topic) = match kind {
        Some("reassurance_exchange") => ("offer_reassurance", "emotional_check_in"),
        Some("family_care") => ("provide_family_support", "family_care"),
        Some("voluntary_support") => ("offer_practical_help", "mutual_aid"),
        _ => return other_typed_event(world, event, interaction, kind, inputs),
    };
    let interaction = kind.expect("one of the three");

    // The event's provenance and time.
    if event.kind != EventKind::Endogenous {
        return failure(BAD_TIME);
    }
    let due_minute = event.due_minute;
    if due_minute != world.minute_index()? {
        return failure(BAD_TIME);
    }
    let provenance = &event.provenance;
    let lacking = || Error::unsupported("the payload lacks a field");
    let source = kit::mapping_get(provenance, "source_id")?.ok_or_else(lacking)?;
    let source_type = kit::mapping_get(provenance, "source_type")?.ok_or_else(lacking)?;
    let version = kit::mapping_get(provenance, "policy_version")?.ok_or_else(lacking)?;
    let event_source = || event.source_id.as_ref().ok_or_else(|| Error::unsupported("a value is not a str"));
    if &**kit::exact_str(source)? != SOURCE_ID
        || &**kit::exact_str(source_type)? != "assumption"
        || **event_source()? != **kit::exact_str(version)?
    {
        return failure(BAD_TIME);
    }

    // The reason code, then the Agent: here only that it is a str. The rows say below whether it is
    // known; a text the Python rule gives after that check asks `agent_is_known` first.
    let Some(reason) = kit::mapping_get(payload, "reason_short")?.filter(|value| !matches!(value, V::Null)) else {
        return failure(NO_REASON);
    };
    let Some(actor_object) = kit::mapping_get(payload, "agent_id")? else {
        return failure(UNKNOWN_AGENT);
    };
    let Some(actor_id) = str_of(actor_object).map(|id| &**id) else {
        return failure(UNKNOWN_AGENT);
    };
    let participant_object = kit::mapping_get(payload, "participant_agent_id")?;
    let participant_id = participant_object.and_then(str_of).map(|id| &**id);

    // The dialogue context, of the actor and of the other person if that is a str.
    let context = kit::mapping_get(payload, "conversation_state_context")?;
    if let Some(context) = context {
        let holds = match participant_id {
            Some(id) if id != actor_id => context_verdict(context, &[actor_id, id], due_minute)?,
            _ => context_verdict(context, &[actor_id], due_minute)?,
        };
        if !holds {
            if !agent_is_known(world, actor_id)? {
                return failure(UNKNOWN_AGENT);
            }
            return failure(BAD_CONTEXT);
        }
    }

    // The payload's fields: these eight, and the context if there is one.
    let zone = kit::mapping_get(payload, "zone_id")?;
    let topic = kit::mapping_get(payload, "topic_id")?;
    let action = kit::mapping_get(payload, "action")?;
    let duration = kit::mapping_get(payload, "interaction_duration_minutes")?;
    let length = match payload {
        V::Map(map) => map.len(),
        _ => 0,
    };
    let counted = participant_object.is_some() && length == PAYLOAD_FIELDS + usize::from(context.is_some());
    let (Some(zone), Some(topic), Some(action), Some(duration), true) = (zone, topic, action, duration, counted) else {
        if !agent_is_known(world, actor_id)? {
            return failure(UNKNOWN_AGENT);
        }
        return failure(ACTION_FIELDS);
    };
    let zone = str_of(zone).map(|zone| &**zone);
    let topic = str_of(topic).map(|topic| &**topic);
    let action = str_of(action).map(|action| &**action);
    let reason = str_of(reason).map(|reason| &**reason);
    let duration_holds = is_duration(duration);

    let relief = action_relief(body_of(world)?.as_deref(), actor_id)?;
    let helps = interaction != "reassurance_exchange";

    // The rows: read, decide, write.
    enum Pair {
        Together,
        Apart,
        NoActor,
    }
    let pair = {
        let rows = world.rows()?;
        match rows.agents.get(actor_id) {
            None => Pair::NoActor,
            Some(actor) => match participant_id.and_then(|id| rows.agents.get(id).map(|participant| (id, participant))) {
                // An ID that is not a str, an unknown person, or the actor themselves.
                None => Pair::Apart,
                Some((id, _)) if id == actor_id => Pair::Apart,
                Some((_, participant)) if actor.profile.id == participant.profile.id => {
                    return Err(Error::unsupported("two people of one profile"));
                }
                Some((_, participant)) if zone != Some(&*actor.zone_id) || participant.zone_id != actor.zone_id => Pair::Apart,
                Some(_) => Pair::Together,
            },
        }
    };
    match pair {
        Pair::Together => {}
        Pair::Apart => return suppression(NOT_COLOCATED),
        Pair::NoActor => return failure(UNKNOWN_AGENT),
    }
    if action != Some(expected_action) || topic != Some(expected_topic) {
        return failure(OFF_TOPIC);
    }
    if reason != Some("autonomous_need_choice") {
        return failure(ACTION_REASON);
    }
    if !duration_holds {
        return failure(ACTION_DURATION);
    }
    let participant_id = participant_id.expect("a known person");
    if interaction == "family_care" {
        let actor_home = kit::household_in(world, actor_id)?;
        if actor_home.is_none() || actor_home != kit::household_in(world, participant_id)? {
            return suppression(NOT_SAME_HOUSEHOLD);
        }
    }
    let mut rows = world.rows_mut()?;
    let (Some(actor), Some(participant)) = (rows.agents.get(actor_id), rows.agents.get(participant_id)) else {
        return Err(Error::unsupported("an unknown person"));
    };
    if interaction == "voluntary_support" && immediate_need(participant) <= immediate_need(actor) + REQUIRED_NEED_MARGIN {
        return suppression(NOT_HIGHER_NEED);
    }
    let stress = py_min(1.0, py_max(0.0, participant.stress - relief));
    let trust = py_min(1.0, py_max(0.0, participant.trust + relief * PARTICIPANT_TRUST_SHARE));
    let fatigue = py_min(1.0, actor.needs.fatigue + ACTOR_FATIGUE_COST);
    let actor_trust = py_min(1.0, actor.trust + py_max(0.0, relief * ACTOR_TRUST_SHARE));
    let participant = rows.agents.get_mut(participant_id).expect("found above");
    participant.stress = stress;
    participant.trust = trust;
    if helps {
        let actor = rows.agents.get_mut(actor_id).expect("found above");
        actor.needs.fatigue = fatigue;
        actor.trust = actor_trust;
    }
    Ok(None)
}

// ---- The other social events -------------------------------------------------------------------------
//
// Each is rare and checks in the Python rule's order with Python's comparisons, so each text comes
// where the Python rule's does.

const REASSURANCE_STRESS: f64 = 0.03;
const REASSURANCE_TRUST: f64 = 0.01;
const PRACTICAL_TRUST: f64 = 0.005;
const COMPLAINT_ACKNOWLEDGE_STRESS: f64 = 0.02;
const COMPLAINT_EXPLAIN_STRESS: f64 = 0.01;
const COMPLAINT_TRUST: f64 = 0.01;
const COOPERATION_STRESS: f64 = 0.02;
const COOPERATION_TRUST: f64 = 0.01;

const UNTYPED_IDS: &str = "social interaction requires Agent, request, and action IDs";
const UNTYPED_UNKNOWN: &str = "social interaction references an unknown request or Agent";
const UNTYPED_ACTION: &str = "social interaction action is not allowed";

/// `isinstance(value, str) and value`.
fn nonempty_str(value: &V) -> Option<&str> {
    str_of(value).map(|text| &**text).filter(|text| !text.is_empty())
}

/// A person's stress and trust: `max(0.0, stress - stress_relief)` and `min(1.0, trust + trust_gain)`.
fn relieve(world: &World, agent_id: &str, stress_relief: Option<f64>, trust_gain: f64) -> Result<()> {
    let mut rows = world.rows_mut()?;
    let agent = rows.agents.get_mut(agent_id).ok_or_else(|| Error::unsupported("an unknown person"))?;
    if let Some(relief) = stress_relief {
        agent.stress = py_max(0.0, agent.stress - relief);
    }
    agent.trust = py_min(1.0, agent.trust + trust_gain);
    Ok(())
}

/// `SocialInteractionHandler.apply` of an event without a typed interaction.
fn untyped_event(world: &World, payload: &V) -> Result<Said> {
    // `all(isinstance(item, str) and item for item in (agent_id, request_id, action))`
    let Some(agent_id) = nonempty_str(get(payload, "agent_id")) else {
        return failure(UNTYPED_IDS);
    };
    let Some(request_id) = nonempty_str(get(payload, "request_id")) else {
        return failure(UNTYPED_IDS);
    };
    let Some(action) = nonempty_str(get(payload, "action")) else {
        return failure(UNTYPED_IDS);
    };
    let known = {
        let rows = world.rows()?;
        let request = rows.support_requests.get(request_id);
        request.is_some_and(|request| &*request.agent_id == agent_id) && rows.agents.contains_key(agent_id)
    };
    if !known {
        return failure(UNTYPED_UNKNOWN);
    }
    match action {
        "offer_reassurance" => relieve(world, agent_id, Some(REASSURANCE_STRESS), REASSURANCE_TRUST)?,
        "share_practical_information" => relieve(world, agent_id, None, PRACTICAL_TRUST)?,
        _ => return failure(UNTYPED_ACTION),
    }
    Ok(None)
}

/// `_social_payload_fields(payload) == set(names)`: these fields, with or without
/// `conversation_state_context`.
fn typed_fields(payload: &V, names: &[&str]) -> bool {
    let V::Map(map) = payload else {
        return false;
    };
    let extra = usize::from(map.contains_key("conversation_state_context"));
    map.len() == names.len() + extra && names.iter().all(|name| map.contains_key(*name))
}

/// What the three other types read of the event, after the checks they share.
struct Typed<'a> {
    world: &'a World,
    event: &'a EventRow,
    payload: &'a V,
    inputs: &'a Inputs,
    /// The actor, a known person.
    agent_id: &'a Arc<str>,
    action: &'a V,
    reason: &'a V,
}

impl Typed<'_> {
    /// The applied contact a minute ago that an interaction answers, from the EventLog, whose
    /// payload holds these people in this order of checks.
    fn contact_matches(&self, trigger: &V, people: [(&str, &str); 2]) -> Result<bool> {
        let Some(trigger) = str_of(trigger) else {
            return Ok(false);
        };
        let Some((looked_up, record)) = &self.inputs.trigger else {
            return Err(Error::Runtime(format!("a record the plan did not look up: {trigger}")));
        };
        if looked_up != trigger {
            return Err(Error::Runtime(format!("a record the plan did not look up: {trigger}")));
        }
        let Some(contact) = record else {
            return Ok(false);
        };
        if contact.status != EventStatus::Applied
            || &*contact.event.event_type != "information_contact_update"
            || contact.realized_minute != self.event.due_minute - 1
        {
            return Ok(false);
        }
        Ok(people.iter().all(|(name, id)| text_is(kit::payload(&contact.event, name).unwrap_or(&NULL), id)))
    }
}

/// `_apply_typed_interaction` of every type but the three social actions.
fn other_typed_event(world: &World, event: &EventRow, interaction: &V, kind: Option<&str>, inputs: &Inputs) -> Result<Said> {
    let payload = &event.payload;
    let provenance = &event.provenance;
    if event.kind != EventKind::Endogenous
        || event.due_minute != world.minute_index()?
        || !text_is(kit::get(provenance, "source_id")?, SOURCE_ID)
        || !text_is(kit::get(provenance, "source_type")?, "assumption")
        || !same_text(&event.source_id, kit::get(provenance, "policy_version")?)
    {
        return failure(BAD_TIME);
    }
    let due = event.due_minute;
    let reason = get(payload, "reason_short");
    if matches!(reason, V::Null) {
        return failure(NO_REASON);
    }
    let Some(agent_id) = known_agent(world, get(payload, "agent_id"))? else {
        return failure(UNKNOWN_AGENT);
    };
    if let Some(context) = kit::mapping_get(payload, "conversation_state_context")? {
        let holds = match str_of(get(payload, "participant_agent_id")) {
            Some(id) if id != agent_id => context_verdict(context, &[agent_id, id], due)?,
            _ => context_verdict(context, &[agent_id], due)?,
        };
        if !holds {
            return failure(BAD_CONTEXT);
        }
    }
    let typed = Typed { world, event, payload, inputs, agent_id, action: get(payload, "action"), reason };
    match kind {
        Some("complaint_response") => complaint_response(&typed),
        Some("cooperation") => cooperation(&typed),
        Some("information_reaction") => information_reaction(&typed),
        Some(_) => failure(NOT_ALLOWED),
        // Another scalar JSON value is in no set of texts and equals none.
        None if matches!(interaction, V::Bool(_) | V::Int(_) | V::Big(_) | V::Float(_)) => failure(NOT_ALLOWED),
        None => Err(Error::unsupported("another type of interaction is for the Python rule to judge")),
    }
}

/// `world.agents.get(value) if isinstance(value, str) else None`, as the person's ID.
fn known_agent<'a>(world: &World, value: &'a V) -> Result<Option<&'a Arc<str>>> {
    match str_of(value) {
        Some(id) if agent_is_known(world, id)? => Ok(Some(id)),
        _ => Ok(None),
    }
}

fn complaint_response(typed: &Typed<'_>) -> Result<Said> {
    let Typed { world, event, payload, agent_id, action, reason, .. } = *typed;
    let names = ["interaction_type", "agent_id", "request_id", "zone_id", "stress_threshold", "complaint_wait_minutes", "action", "reason_short"];
    if !typed_fields(payload, &names) {
        return failure("complaint response payload fields do not match");
    }
    let request_id = get(payload, "request_id");
    let request_key = key_of(request_id)?;
    let threshold = threshold_of(get(payload, "stress_threshold"))?;
    let wait = crate::rules::information_events::int_not_bool(get(payload, "complaint_wait_minutes"))?;
    let (Some(threshold), Some(wait)) = (threshold, wait.filter(|minutes| *minutes > 0)) else {
        return failure("complaint response thresholds are invalid");
    };
    let (current, stress) = {
        let rows = world.rows()?;
        let agent = rows.agents.get(&**agent_id).ok_or_else(|| Error::unsupported("an unknown person"))?;
        let current = match request_key.and_then(|key| rows.support_requests.get(key)) {
            None => false,
            Some(request) => {
                request.agent_id == *agent_id
                    && request.status == RequestStatus::Pending
                    && text_is(get(payload, "zone_id"), &agent.zone_id)
                    && same_text(&event.causation_id, request_id)
                    && (i128::from(event.due_minute) - i128::from(request.requested_minute)) >= i128::from(wait)
            }
        };
        (current, agent.stress)
    };
    if !current {
        return suppression("complaint response context is no longer current");
    }
    if stress < threshold {
        return suppression("complaint response stress candidate is no longer current");
    }
    if not_in(reason, &["long_wait", "high_stress", "request_still_pending", "rule_fallback"])? {
        return failure("complaint response reason code is not allowed");
    }
    if text_is(action, "acknowledge_complaint") {
        relieve(world, agent_id, Some(COMPLAINT_ACKNOWLEDGE_STRESS), COMPLAINT_TRUST)?;
    } else if text_is(action, "explain_staff_process") {
        relieve(world, agent_id, Some(COMPLAINT_EXPLAIN_STRESS), COMPLAINT_TRUST)?;
    } else if text_is(action, "share_practical_information") {
        relieve(world, agent_id, None, PRACTICAL_TRUST)?;
    } else {
        return failure("complaint response action is not allowed");
    }
    Ok(None)
}

fn cooperation(typed: &Typed<'_>) -> Result<Said> {
    let Typed { world, event, payload, agent_id, action, reason, .. } = *typed;
    let names = ["interaction_type", "agent_id", "participant_agent_id", "zone_id", "stress_threshold", "relationship", "trigger_event_id", "action", "reason_short"];
    if !typed_fields(payload, &names) {
        return failure("cooperation payload fields do not match");
    }
    let participant = known_agent(world, get(payload, "participant_agent_id"))?;
    let trigger = get(payload, "trigger_event_id");
    let Some(threshold) = threshold_of(get(payload, "stress_threshold"))? else {
        return failure("cooperation stress threshold is invalid");
    };
    let participant = match participant {
        Some(id) if id != agent_id => id,
        _ => return failure("cooperation requires two different known Agents"),
    };
    let actor_home = kit::household_in(world, agent_id)?;
    let same_group = actor_home.is_some() && actor_home == kit::household_in(world, participant)?;
    let local = {
        let rows = world.rows()?;
        let (Some(agent), Some(other)) = (rows.agents.get(&**agent_id), rows.agents.get(&**participant)) else {
            return Err(Error::unsupported("an unknown person"));
        };
        text_is(get(payload, "zone_id"), &agent.zone_id)
            && agent.zone_id == other.zone_id
            && same_group
            && text_is(get(payload, "relationship"), "same_group")
            && same_text(&event.causation_id, trigger)
    };
    if !local {
        return suppression("cooperation participants or relationship changed");
    }
    if !typed.contact_matches(trigger, [("sender_agent_id", agent_id), ("recipient_agent_id", participant)])? {
        return failure("cooperation does not match an applied local contact");
    }
    let highest = {
        let rows = world.rows()?;
        let (Some(agent), Some(other)) = (rows.agents.get(&**agent_id), rows.agents.get(&**participant)) else {
            return Err(Error::unsupported("an unknown person"));
        };
        // `max(agent.stress, participant.stress)` keeps the first unless the second is greater.
        if other.stress > agent.stress { other.stress } else { agent.stress }
    };
    if highest < threshold {
        return suppression("cooperation stress candidate is no longer current");
    }
    if not_in(reason, &["shared_group_contact", "elevated_stress", "rule_fallback"])? {
        return failure("cooperation reason code is not allowed");
    }
    if !text_is(action, "offer_mutual_support") {
        return failure("cooperation action is not allowed");
    }
    relieve(world, agent_id, Some(COOPERATION_STRESS), COOPERATION_TRUST)?;
    relieve(world, participant, Some(COOPERATION_STRESS), COOPERATION_TRUST)?;
    Ok(None)
}

fn information_reaction(typed: &Typed<'_>) -> Result<Said> {
    let Typed { world, event, payload, inputs, agent_id, action, reason } = *typed;
    let names = ["interaction_type", "agent_id", "participant_agent_id", "zone_id", "message_id", "trigger_event_id", "action", "reason_short"];
    if !typed_fields(payload, &names) {
        return failure("information reaction payload fields do not match");
    }
    let participant = known_agent(world, get(payload, "participant_agent_id"))?;
    let message_id = get(payload, "message_id");
    let trigger = get(payload, "trigger_event_id");
    let participant = match participant {
        Some(id) if id != agent_id => id,
        _ => return failure("information reaction requires two known Agents"),
    };
    let local = {
        let rows = world.rows()?;
        let (Some(agent), Some(other)) = (rows.agents.get(&**agent_id), rows.agents.get(&**participant)) else {
            return Err(Error::unsupported("an unknown person"));
        };
        text_is(get(payload, "zone_id"), &agent.zone_id) && agent.zone_id == other.zone_id && same_text(&event.causation_id, trigger)
    };
    if !local {
        return suppression("information reaction contact is no longer local");
    }
    if !typed.contact_matches(trigger, [("recipient_agent_id", agent_id), ("sender_agent_id", participant)])? {
        return failure("information reaction does not match an applied contact");
    }
    let Some((config, held)) = &inputs.information else {
        return Err(Error::Runtime("the information model was not read for a reaction".to_owned()));
    };
    match held {
        Held::Absent => return suppression("information state is no longer enabled"),
        Held::Other(reason) => return Err(Error::unsupported(*reason)),
        Held::Held => {}
    }
    let message_key = key_of(message_id)?;
    let (settled, pressure) = {
        let row = world.row::<InformationRow>(INFORMATION)?;
        let message = message_key.and_then(|key| row.messages.get(key));
        let belief = message_key.and_then(|key| row.beliefs.get(&**agent_id).and_then(|beliefs| beliefs.get(key)));
        let (Some(message), Some(belief)) = (message, belief) else {
            return suppression("information claim is no longer current");
        };
        // `TypedMessage.from_dict` and `AgentInformationState.from_dict` of the records.
        if row.schema_version != INFORMATION_VERSION {
            return Err(Error::unsupported("an older state is for the Python rule to read"));
        }
        if !message.parses() || !belief.parses() {
            return Err(Error::unsupported("the record is for the Python rule to judge"));
        }
        let settled = &*message.truth_status != "false" || &*belief.belief != "heard";
        (settled, row.query_pressure.get(&**agent_id).copied())
    };
    if settled {
        return suppression("information claim has already been verified or rejected");
    }
    if !text_is(action, "seek_claim_verification") {
        return failure("information reaction action is not allowed");
    }
    if not_in(reason, &["heard_through_contact", "request_verification", "rule_fallback"])? {
        return failure("information reaction reason code is not allowed");
    }
    let gain = pressure_gain(config)?;
    let Some(pressure) = pressure else {
        return Err(Error::unsupported("the person has no record"));
    };
    world.row_mut::<InformationRow>(INFORMATION)?.query_pressure.insert(agent_id.clone(), py_min(1.0, pressure + gain));
    Ok(None)
}

/// `InformationSpreadConfig.from_mapping(model_configs["information_spread"]).query_pressure_per_claim`:
/// a configs without the key, and whatever the parse raised, is refused.
pub fn pressure_gain(config: &Kept<InformationSpread>) -> Result<f64> {
    let config = match config {
        Ok(config) if config.present => config,
        _ => return Err(Error::unsupported("the config is for the Python rule to read")),
    };
    match config.query_pressure_per_claim.clone() {
        Ok(value) => Ok(value),
        Err(fault) if fault.is_refusal() => Err(Error::unsupported(fault.text.to_string())),
        Err(_) => Err(Error::corrupt(crate::world::MODEL_CONFIGS, "a value of a section holds an error")),
    }
}
