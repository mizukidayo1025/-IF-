//! `record_social_event` of `domain/body_state.py`, with the affect operators of
//! `policies/affect.py` and the trust operators of `policies/relationships.py`
//! (docs/rust-port/lanes/m3-body-state.md). The Python rule's body is gone (`kernel_only`). Moved to
//! the core in lane P8 a2.
//!
//! One event is one plan: the plan is computed from the rows and the event's row, and only then is
//! anything written, so an Unsupported changes nothing. It is a `KernelRefusal` in Python, left for
//! what no applied event or valid state holds: an ID or a distance of another type, a person
//! without a body record, a relationship without trust, a tainted namespace, a model this kernel
//! doesn't know.
//!
//! The rule writes the row of `body_state_v1` alone; a tree Python reads follows it (lane P8-0c).
//! Each history item is its canonical JSON, as the row holds it.

use std::sync::Arc;

use indexmap::IndexMap;

use crate::config::body::{AffectModel, BodyModels, TrustModel, DISTANCE_TRUST, DURATION_TRUST};
use crate::config::{SectionId, Selection};
use crate::host::Host;
use crate::error::{Error, Result};
use crate::mathx::{hypot, py_max, py_min};
use crate::row::IdMap;
use crate::rules::kit;
use crate::state::body_state::{
    new_relationship, BodyRecord, BodyRow, Relationship, MAX_SOCIAL_HISTORY, MAX_SOCIAL_RELATIONSHIPS, NAME,
};
use crate::state::information::{InformationRow, NAME as INFORMATION};
use crate::state::movement::{MovementRow, Position};
use crate::value::V;
use crate::world::{World, WorldCell};

const DISTANCE_TRUST_SOURCE: &str = "ASM-DISTANCE-DURATION-REPEAT-TRUST-V1";
const DURATION_TRUST_SOURCE: &str = "ASM-DURATION-CONTACT-TRUST-V1";
const RELATIONSHIP_POLICY_VERSION: &str = "1";
const EVIDENCE_AWARE_INFORMATION: &str = "evidence-aware-rumor-v1";
const MOVEMENT: &str = "movement_v1";
/// Larger than this a float's ceil may not fit the row's int.
const LARGEST_DURATION: f64 = 9e15;

/// What the rule reads of an applied event: its row's fields, or those of an object Python made.
pub struct SocialEvent<'a> {
    pub event_id: &'a str,
    pub event_type: &'a str,
    pub causation_id: Option<&'a str>,
    /// A mapping (`V::Map`); anything else reads as an empty payload.
    pub payload: &'a V,
}

impl<'a> SocialEvent<'a> {
    /// The fields of an event's row.
    pub fn of(row: &'a crate::events::EventRow) -> Self {
        Self { event_id: &row.event_id, event_type: &row.event_type, causation_id: row.causation_id.as_deref(), payload: &row.payload }
    }

    /// `payload.get(key)`.
    fn get(&self, key: &str) -> Option<&'a V> {
        match self.payload {
            V::Map(map) => map.get(key),
            _ => None,
        }
    }
}

/// The refusal of a model selection that is not one.
pub const NOT_A_SELECTION: &str = "not a selection";
/// The refusal of a model selection whose read raised.
pub const SELECTION_RAISES: &str = "the model selection raises";

/// Whether the rule has nothing to record to: `model_state` has no namespace, or None there, read
/// through the host (H1, lane P9-1a). A table that is not a plain dict is refused.
pub fn namespace_absent<C: WorldCell, H: Host>(cell: &C, host: &mut H) -> std::result::Result<bool, H::Error> {
    let holding = kit::holding(cell, host)?;
    Ok(holding.is_absent(NAME).map_err(Error::unsupported)?)
}

/// What the rule reads of the configs, read through the host (H2, lane P9-1a): the model selection,
/// whose refusal and error are this rule's own refusals, and the operators it selects, whose fault
/// is raised where the rule reads them (by the PyO3 crate, which parses again, policy E14).
pub struct Inputs {
    pub selection: Arc<Selection>,
    pub models: kit::Kept<BodyModels>,
}

impl Inputs {
    pub fn read<C: WorldCell, H: Host>(cell: &C, host: &mut H) -> std::result::Result<Self, H::Error> {
        let holding = kit::holding(cell, host)?;
        let (selection, kept) = kit::selection(cell, host, &holding)?;
        let selection = match selection {
            Ok(selection) => selection,
            Err(fault) if fault.is_refusal() => return Err(Error::unsupported(NOT_A_SELECTION).into()),
            Err(_) => return Err(Error::unsupported(SELECTION_RAISES).into()),
        };
        let models = kit::kept_of(kit::section_of_selection(cell, host, &holding, kept, SectionId::BodyModels, |configs| &configs.body_models)?);
        Ok(Self { selection, models })
    }
}

/// The event types the rule records; any other changes nothing.
pub fn records_event(event_type: &str) -> bool {
    matches!(
        event_type,
        "proximity_contact_update" | "information_contact_update" | "social_interaction" | "information_correction"
    )
}

// ---- The operators ------------------------------------------------------------------------------

fn unit(value: f64) -> f64 {
    py_min(1.0, py_max(0.0, value))
}

#[derive(Clone, Copy, PartialEq)]
enum Affect {
    Linear,
    TrustWeighted,
}

#[derive(Default, Clone, Copy)]
struct Contribution {
    positive_affect: f64,
    negative_affect: f64,
    reassurance: f64,
    uncertainty: f64,
    discontent: f64,
}

fn bounded_difference(source: f64, target: f64, rate: f64) -> f64 {
    py_max(-rate, py_min(rate, (source - target) * rate))
}

impl Affect {
    fn weighted(self, rate: f64, relationship_trust: f64) -> f64 {
        match self {
            Self::Linear => rate,
            Self::TrustWeighted => rate * (0.5 + unit(relationship_trust)),
        }
    }

    /// `contact` of the operators.
    fn contact(self, source: &BodyRecord, target: &BodyRecord, rate: f64, relationship_trust: f64) -> Contribution {
        let rate = self.weighted(rate, relationship_trust);
        let positive = bounded_difference(source.positive_affect, target.positive_affect, rate);
        let negative = bounded_difference(source.negative_affect, target.negative_affect, rate);
        Contribution {
            positive_affect: positive,
            negative_affect: negative,
            reassurance: py_max(0.0, positive - negative * 0.25),
            uncertainty: py_max(0.0, negative - positive) * 0.25,
            discontent: 0.0,
        }
    }

    /// `social_action` of the operators.
    fn social_action(self, signal: f64, rate: f64, relationship_trust: f64) -> Contribution {
        let effect = self.weighted(rate, relationship_trust) * signal;
        Contribution {
            positive_affect: effect,
            negative_affect: -effect * 0.5,
            reassurance: effect,
            discontent: -effect * 0.5,
            uncertainty: -py_max(0.0, effect),
        }
    }
}

#[derive(Clone, Copy)]
enum TrustPolicy {
    DistanceDurationRepeat { distance_scale_m: f64, repeat_contact_bonus: f64 },
    DurationOnly,
}

impl TrustPolicy {
    /// `update_trust` of the operators. The arguments are the rule's, in its units.
    #[allow(clippy::too_many_arguments)]
    fn update_trust(
        self,
        current: f64,
        duration_minutes: f64,
        interaction_scale_minutes: i64,
        prior_contact_count: i64,
        elapsed_minutes: i64,
        trust_gain: f64,
        trust_decay_per_day: f64,
        distance_m: Option<f64>,
    ) -> f64 {
        let decay = py_min(1.0, py_max(0.0, trust_decay_per_day * elapsed_minutes as f64 / 1440.0));
        let baseline = current * (1.0 - decay) + 0.5 * decay;
        let duration_factor =
            py_min(1.0, py_max(0.0, duration_minutes) / py_max(1.0, interaction_scale_minutes as f64));
        match self {
            Self::DistanceDurationRepeat { distance_scale_m, repeat_contact_bonus } => {
                let distance_factor = match distance_m {
                    Some(distance) => 1.0 / (1.0 + py_max(0.0, distance) / distance_scale_m),
                    None => 1.0,
                };
                let repeats = prior_contact_count.clamp(0, 10);
                let repeat_factor = 1.0 + repeat_contact_bonus * repeats as f64;
                let result = baseline + trust_gain * duration_factor * repeat_factor * distance_factor;
                py_min(1.0, py_max(0.0, result))
            }
            Self::DurationOnly => py_min(1.0, py_max(0.0, baseline + trust_gain * duration_factor)),
        }
    }

    fn identity(self) -> (&'static str, &'static str) {
        match self {
            Self::DistanceDurationRepeat { .. } => (DISTANCE_TRUST, DISTANCE_TRUST_SOURCE),
            Self::DurationOnly => (DURATION_TRUST, DURATION_TRUST_SOURCE),
        }
    }
}

// ---- The event, as plain values ------------------------------------------------------------------

/// A value of a frozen payload. Only what the rule tells apart.
#[derive(Clone)]
enum Item {
    Nothing,
    Bool,
    Int(i64),
    Float(f64),
    Text(Arc<str>),
    Other,
}

impl Item {
    /// `payload.get(key)` of the event's row.
    #[track_caller]
    fn read(event: &SocialEvent<'_>, key: &str) -> Result<Self> {
        Ok(match event.get(key) {
            None | Some(V::Null) => Self::Nothing,
            Some(V::Bool(_)) => Self::Bool,
            Some(V::Int(value)) => Self::Int(*value),
            Some(V::Big(_)) => return Err(Error::unsupported("an int is too large")),
            Some(V::Float(value)) => Self::Float(*value),
            Some(V::Str(text)) => Self::Text(text.clone()),
            Some(V::List(_) | V::Map(_)) => Self::Other,
        })
    }

    fn text(&self) -> Option<&Arc<str>> {
        match self {
            Self::Text(text) => Some(text),
            _ => None,
        }
    }

    /// An ID the rule looks up in `world.agents`. None and a number are simply not there; what can
    /// raise there is left to Python, and so is an ID of another type than a str, kept simple.
    #[track_caller]
    fn id(&self) -> Result<Option<Arc<str>>> {
        match self {
            Self::Text(text) => Ok(Some(text.clone())),
            Self::Nothing => Ok(None),
            _ => Err(Error::unsupported("an ID is not a str")),
        }
    }

    /// The `duration` that `_record_relationship` computes.
    fn relationship_duration(&self) -> f64 {
        match self {
            Self::Int(value) if *value >= 0 => *value as f64,
            Self::Float(value) if value.is_finite() && *value >= 0.0 => *value,
            _ => 1.0,
        }
    }

    /// `float(duration)` when `record_social_event` writes it into a transfer's deltas.
    fn transfer_duration(&self) -> Option<f64> {
        match self {
            Self::Int(value) if *value > 0 => Some(*value as f64),
            Self::Float(value) if value.is_finite() && *value > 0.0 => Some(*value),
            _ => None,
        }
    }
}

/// `key in payload`.
fn holds(event: &SocialEvent<'_>, key: &str) -> bool {
    event.get(key).is_some()
}

enum Interaction {
    Cooperation,
    /// `include_uncertainty` is true only for `reassurance_exchange`.
    Transfer { kind: Arc<str>, include_uncertainty: bool },
    ComplaintResponse,
    None,
}

enum Action {
    Proximity { first: Arc<str>, second: Arc<str>, observed_minute: i64, distance: Option<f64>, duration: f64 },
    Contact { sender: Arc<str>, recipient: Arc<str>, topic: Option<Arc<str>>, duration: f64 },
    Social { kind: Interaction, actor: Arc<str>, participant: Option<Arc<str>>, topic: Option<Arc<str>>, duration: f64 },
    Correction { actors: Vec<Arc<str>> },
    Nothing,
}

/// What the items a rule appends all share.
struct EventFacts {
    minute: i64,
    transfer_topic: Option<Arc<str>>,
    transfer_duration: Option<f64>,
}

// ---- The plan ------------------------------------------------------------------------------------

/// A delta of a history item: a number or a name.
enum DeltaValue {
    Number(f64),
    Name(&'static str),
}

type Deltas = Vec<(&'static str, DeltaValue)>;

struct PlannedItem {
    minute: i64,
    kind: Arc<str>,
    actors: Vec<Arc<str>>,
    topic: Option<Arc<str>>,
    deltas: Deltas,
}

/// A relationship after the event, as `new` when the row has no such key yet.
struct PlannedRelationship {
    key: Arc<str>,
    record: Relationship,
    new: bool,
}

#[derive(Default)]
struct Plan {
    records: Vec<(Arc<str>, BodyRecord)>,
    relationships: Vec<PlannedRelationship>,
    items: Vec<PlannedItem>,
}

struct Models {
    affect: Affect,
    trust: TrustPolicy,
}

impl Models {
    fn of(models: &BodyModels) -> Self {
        Self {
            affect: match models.affect {
                AffectModel::Linear => Affect::Linear,
                AffectModel::TrustWeighted => Affect::TrustWeighted,
            },
            trust: match models.trust {
                TrustModel::DurationOnly => TrustPolicy::DurationOnly,
                TrustModel::DistanceDurationRepeat { distance_scale_m, repeat_contact_bonus } => {
                    TrustPolicy::DistanceDurationRepeat { distance_scale_m, repeat_contact_bonus }
                }
            },
        }
    }
}

/// Python's `sorted({first, second})`.
fn sorted_pair(first: &Arc<str>, second: &Arc<str>) -> Vec<Arc<str>> {
    let mut actors = Vec::new();
    if first == second {
        actors.push(first.clone());
    } else if first.as_bytes() < second.as_bytes() {
        actors.push(first.clone());
        actors.push(second.clone());
    } else {
        actors.push(second.clone());
        actors.push(first.clone());
    }
    actors
}

fn pair_key(first: &str, second: &str) -> Option<Arc<str>> {
    // `ordered_pair_key`, for IDs that JSON writes as they are; the rest are Python's.
    let plain = |id: &str| id.bytes().all(|byte| byte.is_ascii_graphic() && byte != b'"' && byte != b'\\');
    (plain(first) && plain(second)).then(|| Arc::from(format!("[\"{first}\",\"{second}\"]")))
}

#[track_caller]
fn config_float(config: &V, name: &str) -> Result<f64> {
    if let V::Map(map) = config {
        if let Some(V::Float(value)) = map.get(name) {
            return Ok(*value);
        }
    }
    Err(Error::unsupported("a body config value is not a float"))
}

/// `_recorded_contact_distance`.
fn contact_distance(positions: Option<&IdMap<Position>>, first: &str, second: &str) -> Option<f64> {
    let positions = positions?;
    let first = positions.get(first)?.point.as_ref()?;
    let second = positions.get(second)?.point.as_ref()?;
    Some(hypot(&[second.x - first.x, second.y - first.y]))
}

/// The rule computed from the rows. Reads and changes nothing but its own plan.
struct Planner<'a> {
    row: &'a BodyRow,
    positions: Option<&'a IdMap<Position>>,
    models: &'a Models,
    event: &'a EventFacts,
    plan: Plan,
    relationship_count: usize,
}

impl Planner<'_> {
    fn record(&self, id: &Arc<str>) -> Result<BodyRecord> {
        if let Some((_, record)) = self.plan.records.iter().find(|(known, _)| known == id) {
            return Ok(*record);
        }
        match self.row.agents.get(&**id) {
            Some(record) => Ok(*record),
            None => Err(Error::unsupported("a person has no body record")),
        }
    }

    fn store(&mut self, id: &Arc<str>, record: BodyRecord) {
        match self.plan.records.iter_mut().find(|(known, _)| known == id) {
            Some(slot) => slot.1 = record,
            None => self.plan.records.push((id.clone(), record)),
        }
    }

    /// `_relationship_trust`.
    fn relationship_trust(&self, recipient: &str, sender: &str) -> Result<f64> {
        let Some(key) = pair_key(recipient, sender) else {
            return Err(Error::unsupported("an ID is not plain"));
        };
        let known = self.plan.relationships.iter().find(|planned| planned.key == key).map(|planned| &planned.record);
        let record = known.or_else(|| self.row.relationships.map.get(&*key));
        Ok(record.and_then(|record| record.trust).unwrap_or(0.5))
    }

    /// `_record_relationship`, with an event ID, which every event has.
    #[allow(clippy::too_many_arguments)]
    fn record_relationship(
        &mut self,
        recipient: &Arc<str>,
        sender: &Arc<str>,
        minute: i64,
        kind: &Arc<str>,
        topic: Option<&Arc<str>>,
        duration: f64,
        distance_m: Option<f64>,
    ) -> Result<()> {
        let Some(key) = pair_key(recipient, sender) else {
            return Err(Error::unsupported("an ID is not plain"));
        };
        let place = self.plan.relationships.iter().position(|planned| planned.key == key);
        let (mut record, new) = match place {
            Some(place) => {
                let planned = &self.plan.relationships[place];
                (planned.record.clone(), planned.new)
            }
            None => match self.row.relationships.map.get(&*key) {
                Some(record) => (record.clone(), false),
                None => {
                    if self.relationship_count >= MAX_SOCIAL_RELATIONSHIPS {
                        return Ok(());
                    }
                    self.relationship_count += 1;
                    (new_relationship(), true)
                }
            },
        };
        let elapsed = match record.last_contact_minute {
            None => 0,
            Some(previous) => match minute.checked_sub(previous) {
                Some(elapsed) => elapsed.max(0),
                None => return Err(Error::unsupported("a minute does not fit")),
            },
        };
        let config = &self.row.config;
        let gain = config_float(config, "relationship_trust_gain")?;
        let decay = config_float(config, "relationship_trust_decay_per_day")?;
        let scale = match &config {
            V::Map(map) => match map.get("social_interaction_minutes") {
                Some(V::Int(minutes)) => (*minutes).max(1),
                None => 5,
                _ => return Err(Error::unsupported("the interaction minutes are not an int")),
            },
            _ => return Err(Error::unsupported("the body config is not a map")),
        };
        let Some(before_trust) = record.trust else {
            return Err(Error::unsupported("a relationship has no trust"));
        };
        // The bound keeps int(math.ceil(duration)) in the row's int.
        if duration > LARGEST_DURATION {
            return Err(Error::unsupported("a duration is too long"));
        }
        let trust =
            self.models.trust.update_trust(before_trust, duration, scale, record.contact_count, elapsed, gain, decay, distance_m);
        let Some(contact_count) = record.contact_count.checked_add(1) else {
            return Err(Error::unsupported("a count does not fit"));
        };
        let Some(contact_minutes) = record.contact_minutes.checked_add(duration.ceil() as i64) else {
            return Err(Error::unsupported("a count does not fit"));
        };
        record.trust = Some(trust);
        record.contact_count = contact_count;
        record.contact_minutes = contact_minutes;
        record.last_contact_minute = Some(minute);
        record.last_interaction = Some(kind.clone());
        if let Some(topic) = topic {
            if !record.topic_ids.contains(topic) {
                record.topic_ids.push(topic.clone());
                if record.topic_ids.len() > 32 {
                    let extra = record.topic_ids.len() - 32;
                    record.topic_ids.drain(..extra);
                }
            }
        }
        let (policy_id, source_id) = self.models.trust.identity();
        let mut deltas: Deltas = Vec::new();
        deltas.push(("relationship_trust_before", DeltaValue::Number(before_trust)));
        deltas.push(("relationship_trust_after", DeltaValue::Number(trust)));
        deltas.push(("relationship_trust_delta", DeltaValue::Number(trust - before_trust)));
        deltas.push(("contact_duration_minutes", DeltaValue::Number(duration)));
        deltas.push(("relationship_policy_id", DeltaValue::Name(policy_id)));
        deltas.push(("relationship_policy_version", DeltaValue::Name(RELATIONSHIP_POLICY_VERSION)));
        deltas.push(("relationship_policy_source_id", DeltaValue::Name(source_id)));
        if let Some(distance) = distance_m {
            if distance.is_finite() && distance >= 0.0 {
                deltas.push(("contact_distance_m", DeltaValue::Number(distance)));
            }
        }
        self.plan.items.push(PlannedItem {
            minute,
            kind: Arc::from("relationship_update"),
            actors: sorted_pair(recipient, sender),
            topic: topic.cloned(),
            deltas,
        });
        let planned = PlannedRelationship { key, record, new };
        match place {
            Some(place) => self.plan.relationships[place] = planned,
            None => self.plan.relationships.push(planned),
        }
        Ok(())
    }

    /// One item of `transfers`, from `_body_deltas` and the loop at the end of the rule. The two
    /// lists of the Python rule keep their order: an item here is added after the relationships'.
    fn transfer_deltas(before: &BodyRecord, after: &BodyRecord) -> Deltas {
        let mut deltas: Deltas = Vec::new();
        // The fields in sorted order, as `sorted(set(before) & set(after))` has them.
        for (name, first, second) in [
            ("discontent", before.discontent, after.discontent),
            ("hygiene_need", before.hygiene_need, after.hygiene_need),
            ("negative_affect", before.negative_affect, after.negative_affect),
            ("positive_affect", before.positive_affect, after.positive_affect),
            ("reassurance", before.reassurance, after.reassurance),
            ("sleep_debt_minutes", before.sleep_debt_minutes, after.sleep_debt_minutes),
            ("uncertainty", before.uncertainty, after.uncertainty),
        ] {
            if second != first {
                deltas.push((name, DeltaValue::Number(second - first)));
            }
        }
        deltas
    }

    fn apply_contribution(record: &mut BodyRecord, contribution: &Contribution, include_uncertainty: bool) {
        record.positive_affect = unit(record.positive_affect + contribution.positive_affect);
        record.negative_affect = unit(record.negative_affect + contribution.negative_affect);
        record.reassurance = unit(record.reassurance + contribution.reassurance);
        record.discontent = unit(record.discontent + contribution.discontent);
        if include_uncertainty {
            record.uncertainty = unit(record.uncertainty + contribution.uncertainty);
        }
    }

    fn run(&mut self, action: &Action) -> Result<()> {
        let mut transfers: Vec<(Arc<str>, Arc<str>, Arc<str>, Deltas)> = Vec::new();
        match action {
            Action::Nothing => {}
            Action::Proximity { first, second, observed_minute, distance, duration } => {
                let kind: Arc<str> = Arc::from("observed_proximity_contact");
                self.record_relationship(first, second, *observed_minute, &kind, None, *duration, *distance)?;
                self.record_relationship(second, first, *observed_minute, &kind, None, *duration, *distance)?;
            }
            Action::Contact { sender, recipient, topic, duration } => {
                let source = self.record(sender)?;
                let mut target = self.record(recipient)?;
                let before = target;
                let rate = config_float(&self.row.config, "affect_transfer_rate")?;
                let trust = self.relationship_trust(recipient, sender)?;
                let contribution = self.models.affect.contact(&source, &target, rate, trust);
                Self::apply_contribution(&mut target, &contribution, true);
                self.store(recipient, target);
                let kind: Arc<str> = Arc::from("information_contact");
                transfers.push((recipient.clone(), sender.clone(), kind.clone(), Self::transfer_deltas(&before, &target)));
                let distance = contact_distance(self.positions, recipient, sender);
                self.record_relationship(recipient, sender, self.event.minute, &kind, topic.as_ref(), *duration, distance)?;
            }
            Action::Social { kind, actor, participant, topic, duration } => match kind {
                Interaction::None => {}
                Interaction::Cooperation => {
                    let Some(participant) = participant else {
                        return Ok(());
                    };
                    let name: Arc<str> = Arc::from("cooperation");
                    for (agent_id, other_id) in [(actor, participant), (participant, actor)] {
                        let before = self.record(agent_id)?;
                        let mut record = before;
                        let trust = self.relationship_trust(agent_id, other_id)?;
                        let trust_scale = self.models.affect.social_action(1.0, 1.0, trust).positive_affect;
                        let config = &self.row.config;
                        record.positive_affect =
                            unit(record.positive_affect + config_float(config, "cooperation_positive_gain")? * trust_scale);
                        record.negative_affect = unit(
                            record.negative_affect - config_float(config, "cooperation_negative_reduction")? * trust_scale,
                        );
                        record.reassurance =
                            unit(record.reassurance + config_float(config, "cooperation_reassurance_gain")? * trust_scale);
                        record.discontent = unit(
                            record.discontent - config_float(config, "cooperation_discontent_reduction")? * trust_scale,
                        );
                        self.store(agent_id, record);
                        transfers.push((actor.clone(), agent_id.clone(), name.clone(), Self::transfer_deltas(&before, &record)));
                    }
                    let distance = contact_distance(self.positions, actor, participant);
                    self.record_relationship(actor, participant, self.event.minute, &name, topic.as_ref(), *duration, distance)?;
                    let distance = contact_distance(self.positions, participant, actor);
                    self.record_relationship(participant, actor, self.event.minute, &name, topic.as_ref(), *duration, distance)?;
                }
                Interaction::Transfer { kind, include_uncertainty } => {
                    let Some(participant) = participant else {
                        return Ok(());
                    };
                    let actor_record = self.record(actor)?;
                    let mut target = self.record(participant)?;
                    let before = target;
                    let mut signal = unit(actor_record.positive_affect - actor_record.negative_affect);
                    if actor_record.positive_affect < actor_record.negative_affect {
                        signal = -unit(actor_record.negative_affect - actor_record.positive_affect);
                    }
                    let rate = config_float(&self.row.config, "social_action_affect_transfer")?;
                    let trust = self.relationship_trust(participant, actor)?;
                    let contribution = self.models.affect.social_action(signal, rate, trust);
                    Self::apply_contribution(&mut target, &contribution, *include_uncertainty);
                    self.store(participant, target);
                    transfers.push((actor.clone(), participant.clone(), kind.clone(), Self::transfer_deltas(&before, &target)));
                    let distance = contact_distance(self.positions, participant, actor);
                    self.record_relationship(participant, actor, self.event.minute, kind, topic.as_ref(), *duration, distance)?;
                }
                Interaction::ComplaintResponse => {
                    let before = self.record(actor)?;
                    let mut record = before;
                    let config = &self.row.config;
                    record.reassurance = unit(record.reassurance + config_float(config, "complaint_reassurance_gain")?);
                    record.discontent = unit(record.discontent - config_float(config, "complaint_discontent_reduction")?);
                    record.negative_affect = unit(record.negative_affect - 0.01);
                    self.store(actor, record);
                    transfers.push((actor.clone(), actor.clone(), Arc::from("complaint_response"), Self::transfer_deltas(&before, &record)));
                }
            },
            Action::Correction { actors } => {
                let name: Arc<str> = Arc::from("information_correction");
                for actor in actors {
                    let before = self.record(actor)?;
                    let mut record = before;
                    let config = &self.row.config;
                    record.uncertainty = unit(record.uncertainty - config_float(config, "correction_uncertainty_reduction")?);
                    record.reassurance = unit(record.reassurance + config_float(config, "correction_reassurance_gain")?);
                    record.negative_affect = unit(record.negative_affect - 0.01);
                    self.store(actor, record);
                    transfers.push((actor.clone(), actor.clone(), name.clone(), Self::transfer_deltas(&before, &record)));
                }
            }
        }
        for (actor, other, kind, mut deltas) in transfers {
            if actor != other {
                if let Some(distance) = contact_distance(self.positions, &actor, &other) {
                    deltas.push(("contact_distance_m", DeltaValue::Number(distance)));
                }
            }
            if let Some(duration) = self.event.transfer_duration {
                deltas.push(("contact_duration_minutes", DeltaValue::Number(duration)));
            }
            self.plan.items.push(PlannedItem {
                minute: self.event.minute,
                kind,
                actors: sorted_pair(&actor, &other),
                topic: self.event.transfer_topic.clone(),
                deltas,
            });
        }
        Ok(())
    }
}

// ---- Reading the world ----------------------------------------------------------------------------

/// `information_correction`'s people who took the correction: those in the zone, in ID order, and
/// with the evidence-aware model only those whose belief says so.
fn correction_actors(world: &World, event: &SocialEvent<'_>, selection: &Selection) -> Result<Vec<Arc<str>>> {
    let zone = Item::read(event, "zone_id")?;
    let mut actors: Vec<Arc<str>> = match &zone {
        Item::Text(zone) => {
            world.rows()?.agents.iter().filter(|(_, agent)| agent.zone_id == *zone).map(|(id, _)| id.clone()).collect()
        }
        Item::Nothing => Vec::new(),
        _ => return Err(Error::unsupported("a zone is not a str")),
    };
    actors.sort_unstable_by(|first, second| first.as_bytes().cmp(second.as_bytes()));
    let Some(information) = selection.get("information") else {
        return Err(Error::unsupported("no information model is selected"));
    };
    if &**information != EVIDENCE_AWARE_INFORMATION {
        return Ok(actors);
    }
    let message = Item::read(event, "message_id")?;
    let beliefs = if kit::namespace_absent(world, INFORMATION)? { None } else { Some(world.row::<InformationRow>(INFORMATION)?) };
    let Item::Text(message) = message else {
        return match message {
            Item::Nothing => Ok(Vec::new()),
            _ => Err(Error::unsupported("a message is not a str")),
        };
    };
    let Some(beliefs) = beliefs else {
        return Ok(Vec::new());
    };
    Ok(actors
        .into_iter()
        .filter(|actor| {
            beliefs.beliefs.get(&**actor).and_then(|held| held.get(&*message)).is_some_and(|belief| {
                *belief.last_event_id == *event.event_id && belief.last_correction_accepted
            })
        })
        .collect())
}

/// What the event asks of the rule, or Unsupported for what is Python's to decide.
fn action_of(world: &World, event: &SocialEvent<'_>, selection: &Selection, minute: i64) -> Result<(Action, EventFacts)> {
    let contact_minutes = Item::read(event, "contact_minutes")?;
    let (message, topic_id) = (Item::read(event, "message_id")?, Item::read(event, "topic_id")?);
    let transfer_topic = message.text().or(topic_id.text()).cloned();
    let interaction_duration = "interaction_duration_minutes";
    let transfer_duration = if holds(event, interaction_duration) {
        Item::read(event, interaction_duration)?.transfer_duration()
    } else {
        contact_minutes.transfer_duration()
    };
    let facts = EventFacts { minute, transfer_topic, transfer_duration };
    let people_are_known = |first: &Arc<str>, second: &Arc<str>| -> Result<bool> {
        let rows = world.rows()?;
        Ok(rows.agents.get(first).is_some() && rows.agents.get(second).is_some())
    };
    let action = match event.event_type {
        "proximity_contact_update" => {
            let first = Item::read(event, "first_agent_id")?.id()?;
            let second = Item::read(event, "second_agent_id")?.id()?;
            let observed = Item::read(event, "observed_minute")?;
            let distance = match Item::read(event, "distance_m")? {
                Item::Nothing => None,
                Item::Float(distance) => Some(distance),
                _ => return Err(Error::unsupported("a distance is not a float")),
            };
            match (first, second, observed) {
                (Some(first), Some(second), Item::Int(observed_minute))
                    if first != second && people_are_known(&first, &second)? =>
                {
                    Action::Proximity { first, second, observed_minute, distance, duration: contact_minutes.relationship_duration() }
                }
                _ => Action::Nothing,
            }
        }
        "information_contact_update" => {
            let sender = Item::read(event, "sender_agent_id")?.id()?;
            let recipient = Item::read(event, "recipient_agent_id")?.id()?;
            match (sender, recipient) {
                (Some(sender), Some(recipient)) if sender != recipient && people_are_known(&sender, &recipient)? => {
                    Action::Contact {
                        sender,
                        recipient,
                        topic: message.text().cloned(),
                        // `payload.get("contact_minutes", 1)`: both an absent key and None give 1.0.
                        duration: contact_minutes.relationship_duration(),
                    }
                }
                _ => Action::Nothing,
            }
        }
        "social_interaction" => {
            let interaction = Item::read(event, "interaction_type")?;
            let actor = Item::read(event, "agent_id")?.id()?;
            let participant = Item::read(event, "participant_agent_id")?.id()?;
            let Some(actor) = actor else {
                return Ok((Action::Nothing, facts));
            };
            let kind = match interaction.text().map(|text| &**text) {
                Some("cooperation") => Interaction::Cooperation,
                Some(name @ ("reassurance_exchange" | "family_care" | "voluntary_support")) => {
                    Interaction::Transfer { kind: Arc::from(name), include_uncertainty: name == "reassurance_exchange" }
                }
                Some("complaint_response") => Interaction::ComplaintResponse,
                _ => Interaction::None,
            };
            if matches!(kind, Interaction::Cooperation | Interaction::Transfer { .. }) && participant.as_ref() == Some(&actor) {
                return Err(Error::unsupported("a person interacts with themselves"));
            }
            let known = match (&kind, &participant) {
                (Interaction::ComplaintResponse, _) => people_are_known(&actor, &actor)?,
                (Interaction::None, _) | (_, None) => false,
                (_, Some(participant)) => people_are_known(&actor, participant)?,
            };
            if !known {
                Action::Nothing
            } else {
                let duration = Item::read(event, interaction_duration)?;
                Action::Social {
                    kind,
                    actor,
                    participant,
                    topic: topic_id.text().cloned(),
                    // `payload.get("interaction_duration_minutes", 1)`: None and absent give 1.0.
                    duration: duration.relationship_duration(),
                }
            }
        }
        "information_correction" => Action::Correction { actors: correction_actors(world, event, selection)? },
        _ => Action::Nothing,
    };
    Ok((action, facts))
}

/// The item's document, in the order of the Python rule's keys, as its canonical JSON. A value
/// canonical JSON refuses is refused here.
fn item_bytes(item: &PlannedItem, event: &SocialEvent<'_>) -> Result<Box<[u8]>> {
    let text = |value: &str| V::Str(Arc::from(value));
    let mut document = IndexMap::with_capacity(7);
    document.insert(Arc::from("event_id"), V::Str(Arc::from(event.event_id)));
    document.insert(Arc::from("minute"), V::Int(item.minute));
    document.insert(Arc::from("kind"), V::Str(item.kind.clone()));
    document.insert(Arc::from("actor_ids"), V::List(item.actors.iter().map(|actor| V::Str(actor.clone())).collect()));
    document.insert(Arc::from("topic_id"), item.topic.clone().map_or(V::Null, V::Str));
    document.insert(Arc::from("causation_id"), event.causation_id.map_or(V::Null, |id| V::Str(Arc::from(id))));
    let mut deltas = IndexMap::with_capacity(item.deltas.len());
    for (name, value) in &item.deltas {
        let value = match value {
            DeltaValue::Number(number) => V::Float(*number),
            DeltaValue::Name(name) => text(name),
        };
        deltas.insert(Arc::from(*name), value);
    }
    document.insert(Arc::from("deltas"), V::Map(deltas));
    Ok(V::Map(document).to_json()?.into_boxed_slice())
}

// ---- The kernel -----------------------------------------------------------------------------------

/// `record_social_event` of an applied event. `selection` and `models` are what the PyO3 crate read
/// of the configs (`selected_models`, the operators), after it found the namespace present.
pub fn record_social_event(
    world: &World,
    event: &SocialEvent<'_>,
    minute: i64,
    selection: &Selection,
    models: &BodyModels,
) -> Result<()> {
    if kit::namespace_absent(world, NAME)? || !records_event(event.event_type) {
        return Ok(());
    }
    let models = Models::of(models);
    let (action, facts) = action_of(world, event, selection, minute)?;
    if matches!(action, Action::Nothing) {
        return Ok(());
    }

    let plan = {
        let row = world.row::<BodyRow>(NAME)?;
        let movement =
            if kit::namespace_absent(world, MOVEMENT)? { None } else { Some(world.row::<MovementRow>(MOVEMENT)?) };
        let mut planner = Planner {
            relationship_count: row.relationships.map.len(),
            row: &row,
            positions: movement.as_ref().map(|movement| &movement.positions),
            models: &models,
            event: &facts,
            plan: Plan::default(),
        };
        planner.run(&action)?;
        planner.plan
    };

    // A value canonical JSON refuses is refused here, before any change.
    let mut items = Vec::with_capacity(plan.items.len());
    for item in &plan.items {
        items.push(item_bytes(item, event)?);
    }

    // From here on nothing may be refused.
    let mut row = world.row_mut::<BodyRow>(NAME)?;
    for (id, next) in &plan.records {
        let record = row.agents.get_mut(&**id).ok_or_else(|| Error::Runtime("a planned record left the row".to_owned()))?;
        *record = *next;
    }
    for planned in plan.relationships {
        row.relationships.written.push(planned.key.clone());
        if planned.new {
            row.relationships.map.insert(planned.key, planned.record);
        } else {
            let record = row
                .relationships
                .map
                .get_mut(&*planned.key)
                .ok_or_else(|| Error::Runtime("a planned relationship left the row".to_owned()))?;
            *record = planned.record;
        }
    }
    for bytes in items {
        row.history.items.push_back(bytes);
    }
    let dropped = row.history.items.len().saturating_sub(MAX_SOCIAL_HISTORY);
    row.history.items.drain(..dropped);
    Ok(())
}
