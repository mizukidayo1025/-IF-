//! The `WorldView` of the phases (`policies/world_view.py`), built from the rows of the core's
//! `World`: a detached snapshot in plain Rust values (`ViewData`), which the rules read directly
//! (lane P8 a1 of Issue #152). Its Python rules, `WorldView.from_world` and
//! `WorldView.after_state_update`, went with lane L10.
//!
//! `build` is `from_world` without its hash, `refresh` is `after_state_update` (the prior view with
//! what the update phase writes read again), and `state_hash` is the audit hash `_view_state_hash`
//! wrote, over the same canonical JSON. The PyO3 crate's `rules/world_view.rs` gathers the inputs
//! only Python has (the model selection, the configs while `model_configs` is a dict the world's
//! slot does not hold, `simulator_extensions.enabled`), calls these with the world borrowed, and
//! makes the Python `WorldView` from what they return.
//!
//! Every namespace is read from its row: `information_spread_v1`, `body_state_v1`,
//! `psychological_distress_v1` and `staff_operations_v1` as their typed rows, and
//! `operational_events_v1`, `infrastructure_v1`, `population_context_v1`, `population_dynamics_v1`,
//! `population_presence_v1` and `zone_environment_v1` as `V`. A tainted namespace (a value its row
//! cannot hold) is refused where it is read. Anything this module is not sure of says `Unsupported`
//! before anything has changed, and the phase's step raises KernelRefusal. A value of another type
//! than the Python view would hold is refused too: a typed field here could not keep it, and `1` and
//! `1.0` would hash differently.
//!
//! A person's context is parsed by `person_context.rs`, and a zone's conditions come from
//! `zone_environment.rs`; what they make is kept in a `ViewCache` (on the controller's `Run`) while
//! the stamps of the slots it was read from stay the same. The one coefficient in this file is in
//! `misinformation_of`, with `from_world`'s weight, threshold and cap; stressors sum their rates.

use std::ops::Deref;
use std::sync::Arc;

use indexmap::IndexMap;
use rustc_hash::FxHashMap;
use sha2::{Digest, Sha256};

use crate::cell::ShardRef;
use crate::config::{Fault, Selection};
use crate::entities::{Activity, Entities, Mobility, NeedRow, RequestStatus, TaskRow};
use crate::error::{Error, Result};
use crate::json::Encoder;
use crate::mathx::{py_fsum, py_min};
use crate::parallel::parts_in_order;
use crate::row::ValueRow;
use crate::stamp::Stamp;
use crate::state::body_state::{self, BodyRow};
use crate::state::information::{self, InformationRow};
use crate::state::psychological_distress::{self, DistressRow};
use crate::state::staff_operations::{self, StaffOperationsRow};
use crate::text::casefold;
use crate::value::V;
use crate::world::{World, MODEL_CONFIGS};

pub use super::person_context::PersonFields;
pub use super::zone_environment::DemandModifier;
use super::person_context;
use super::zone_environment::{self, ZoneConditions, ZoneInputs};

/// The keys of `model_state` read as `V`.
pub const OPERATIONAL_EVENTS: &str = "operational_events_v1";
pub const INFRASTRUCTURE: &str = "infrastructure_v1";
pub const POPULATION_CONTEXT: &str = "population_context_v1";
pub const POPULATION_DYNAMICS: &str = "population_dynamics_v1";
pub const POPULATION_PRESENCE: &str = "population_presence_v1";

#[track_caller]
fn unsupported<T>(reason: &'static str) -> Result<T> {
    Err(Error::unsupported(reason))
}

// ---- The snapshot ---------------------------------------------------------------------------------

/// What everyone in a zone shares of an `AgentView`.
#[derive(Clone, Debug, PartialEq)]
pub struct ViewZone {
    pub id: Arc<str>,
    pub environmental_stress_per_minute: f64,
    pub environmental_stressor_event_ids: Vec<Arc<str>>,
    pub environmental_assumption_source_id: Option<Arc<str>>,
    pub ambient_noise_level: f64,
    pub rest_efficiency_multiplier: f64,
    pub zone_environment_modifier_event_ids: Vec<Arc<str>>,
    pub zone_environment_assumption_ids: Vec<Arc<str>>,
    pub ventilation_effectiveness: f64,
    pub lighting_level: f64,
    pub privacy_level: f64,
    pub temperature_c: f64,
    pub ventilation_hygiene_growth_per_minute: f64,
    pub privacy_discontent_per_minute: f64,
    pub condition_modifier_event_ids: Vec<Arc<str>>,
    pub condition_assumption_ids: Vec<Arc<str>>,
    pub demand_modifiers: Vec<DemandModifier>,
    pub min_rest_decision_minutes: i64,
    pub max_rest_decision_minutes: i64,
    pub sanitation_level: f64,
}

/// One entry of `misinformation_effect_sources`.
#[derive(Clone, Debug, PartialEq)]
pub struct MisinformationSource {
    pub message_id: Arc<str>,
    pub origin_event_id: Arc<str>,
    pub last_event_id: Arc<str>,
    pub source_id: Arc<str>,
    pub time: i64,
}

/// A person's values from `body_state_v1`, with `from_world`'s defaults.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BodyFields {
    pub sleep_debt_minutes: f64,
    pub uncertainty: f64,
    pub reassurance: f64,
    pub discontent: f64,
    pub positive_affect: f64,
    pub negative_affect: f64,
    pub hygiene_need: f64,
}

/// A person without a record in `body_state_v1`.
pub const NO_BODY: BodyFields = BodyFields {
    sleep_debt_minutes: 0.0,
    uncertainty: 0.0,
    reassurance: 0.0,
    discontent: 0.0,
    positive_affect: 0.5,
    negative_affect: 0.0,
    hygiene_need: 0.0,
};

/// `AgentView`, except what `zone` and `person` hold.
#[derive(Clone)]
pub struct ViewAgent {
    pub id: Arc<str>,
    /// `household_id` of `population_presence_v1`, over `profile.group_id`.
    pub group_id: Option<Arc<str>>,
    /// The index in `ViewData::zones`.
    pub zone: usize,
    pub mobility: Mobility,
    pub support_need: f64,
    pub stress_sensitivity: f64,
    pub rng_key: Arc<str>,
    pub activity: Activity,
    pub target_id: Option<Arc<str>>,
    pub needs: NeedRow,
    pub stress: f64,
    pub chronic_distress: f64,
    pub trust: f64,
    pub next_decision_at: i64,
    pub queue_task_id: Option<Arc<str>>,
    pub queue_kind: Option<Arc<str>>,
    pub queue_target_id: Option<Arc<str>>,
    pub queue_wait_minutes: i64,
    pub support_request_id: Option<Arc<str>>,
    pub support_request_status: Option<RequestStatus>,
    pub support_assigned_staff_id: Option<Arc<str>>,
    pub support_wait_minutes: i64,
    pub support_cooldown_remaining_minutes: i64,
    pub information_query_pressure: f64,
    pub misinformation_belief_pressure: f64,
    pub misinformation_effect_sources: Vec<MisinformationSource>,
    pub body: BodyFields,
    pub person: Arc<PersonFields>,
}

/// `FacilityView`.
#[derive(Clone, Debug, PartialEq)]
pub struct ViewFacility {
    pub id: Arc<str>,
    pub kind: Arc<str>,
    pub zone_id: Arc<str>,
    pub capacity: i64,
    pub working_units: i64,
    pub active_agent_ids: Vec<Arc<str>>,
    pub queue_agent_ids: Vec<Arc<str>>,
    pub resource_active_agent_ids: Vec<Arc<str>>,
    pub resource_queue_agent_ids: Vec<Arc<str>>,
    pub facility_event_context_ids: Vec<Arc<str>>,
}

/// All of a `WorldView`. Nothing here refers to Python, so a rule can read it on any thread.
#[derive(Clone)]
pub struct ViewData {
    pub minute_index: i64,
    pub resource_service_enabled: bool,
    pub autonomy_enabled: bool,
    pub has_population_context: bool,
    /// Read from a Python view: only what the update kernels read is filled, and a zone is one
    /// person's (the PyO3 crate's `from_python_view`). The behavior kernel reads more, and refuses it.
    pub partial: bool,
    /// In ID order.
    pub zones: Vec<Arc<ViewZone>>,
    /// In ID order, as `sorted(world.agents.items())`.
    pub agents: Vec<ViewAgent>,
    /// In ID order.
    pub facilities: Vec<Arc<ViewFacility>>,
}

impl ViewData {
    pub fn zone_of(&self, agent: &ViewAgent) -> &ViewZone {
        &self.zones[agent.zone]
    }

    /// The index of a person in `agents`.
    pub fn index_of(&self, id: &str) -> Option<usize> {
        self.agents.binary_search_by(|agent| (*agent.id).cmp(id)).ok()
    }

    /// `WorldView.zone_members`: each zone with people, in ID order, with their indexes in ID order.
    pub fn zone_members(&self) -> Vec<(&Arc<str>, Vec<usize>)> {
        let mut members: Vec<(&Arc<str>, Vec<usize>)> = self.zones.iter().map(|zone| (&zone.id, Vec::new())).collect();
        for (index, agent) in self.agents.iter().enumerate() {
            members[agent.zone].1.push(index);
        }
        members.retain(|(_, people)| !people.is_empty());
        members
    }

    /// `WorldView.group_members`: each group, in ID order, with its people's indexes in ID order.
    pub fn group_members(&self) -> Vec<(&Arc<str>, Vec<usize>)> {
        let mut positions: FxHashMap<&str, usize> = FxHashMap::default();
        let mut members: Vec<(&Arc<str>, Vec<usize>)> = Vec::new();
        for (index, agent) in self.agents.iter().enumerate() {
            if let Some(group) = &agent.group_id {
                let position = *positions.entry(group).or_insert_with(|| {
                    members.push((group, Vec::new()));
                    members.len() - 1
                });
                members[position].1.push(index);
            }
        }
        members.sort_unstable_by(|first, second| first.0.cmp(second.0));
        members
    }
}

// ---- Reading the world ----------------------------------------------------------------------------

/// What `model_state` holds under a namespace with a typed row.
#[derive(Clone, PartialEq, Eq, Debug)]
enum Held {
    /// No such key.
    Absent,
    /// The row: what the tree holds, or all there is of a detached namespace.
    Row,
    /// A value the row could not hold, and why.
    Tainted(String),
}

fn held(world: &World, name: &str) -> Result<Held> {
    let Some(slot) = world.slots.get(name) else {
        return Ok(Held::Absent);
    };
    let slot = slot.try_borrow()?;
    Ok(match (&slot.row, &slot.taint) {
        (Some(_), _) => Held::Row,
        (None, Some(taint)) => Held::Tainted(taint.clone()),
        (None, None) => Held::Absent,
    })
}

/// The typed row of a namespace, None without the key. A tainted namespace is refused, with why
/// its row could not hold it: the rules of the view read no Python tree.
#[track_caller]
fn row_of<'w, T: crate::row::Row>(world: &'w World, name: &str) -> Result<Option<ShardRef<'w, T>>> {
    match held(world, name)? {
        Held::Absent => Ok(None),
        Held::Row => world.row::<T>(name).map(Some),
        Held::Tainted(why) => Err(Error::unsupported(format!("{name} is tainted: {why}"))),
    }
}

/// `model_state.get(name)` of a key held as `V`: None without the key. A value that is not JSON
/// is refused.
fn state<'w>(world: &'w World, name: &str) -> Result<Option<ShardRef<'w, V>>> {
    world.slots.try_value(name)
}

/// `model_configs`, a map: the row of its slot, or the dict Python holds past it.
pub struct ModelConfigs<'w> {
    held: ConfigsHeld<'w>,
    /// The stamp of the slot whose row these are; None for configs given past the slot.
    pub stamp: Option<Stamp>,
}

enum ConfigsHeld<'w> {
    Row(ShardRef<'w, ValueRow>),
    Given(&'w V),
}

impl<'w> ModelConfigs<'w> {
    /// The configs of the world: `given` when the PyO3 crate read them from a dict the slot does not
    /// hold (`world.model_configs` assigned whole), else the slot's row.
    pub fn of(world: &'w World, given: Option<&'w V>) -> Result<Self> {
        let (held, stamp) = match given {
            Some(given) => (ConfigsHeld::Given(given), None),
            None => (ConfigsHeld::Row(world.slots.try_row::<ValueRow>(MODEL_CONFIGS)?), world.stamp_of(MODEL_CONFIGS)?),
        };
        let configs = Self { held, stamp };
        if !matches!(configs.value(), V::Map(_)) {
            return unsupported("the configs are not a plain dict");
        }
        Ok(configs)
    }

    fn value(&self) -> &V {
        match &self.held {
            ConfigsHeld::Row(row) => &row.0,
            ConfigsHeld::Given(given) => given,
        }
    }

    pub fn map(&self) -> &IndexMap<Arc<str>, V> {
        match self.value() {
            V::Map(map) => map,
            // `of` checked it.
            _ => unreachable!("the configs are a map"),
        }
    }

    pub fn get(&self, name: &str) -> Option<&V> {
        self.map().get(name)
    }
}

impl Deref for ModelConfigs<'_> {
    type Target = IndexMap<Arc<str>, V>;

    fn deref(&self) -> &Self::Target {
        self.map()
    }
}

/// `record[name]`; a missing one raises in Python.
#[track_caller]
fn field<'a>(record: &'a IndexMap<Arc<str>, V>, name: &str) -> Result<&'a V> {
    match record.get(name) {
        Some(value) => Ok(value),
        None => unsupported("a record's field is missing"),
    }
}

/// The dict a `get(key, {})` gave, where a non-dict raises in Python.
#[track_caller]
fn required_dict(value: &V) -> Result<&IndexMap<Arc<str>, V>> {
    match value {
        V::Map(map) => Ok(map),
        _ => unsupported("a state is not a plain dict"),
    }
}

#[track_caller]
fn exact_float(value: &V) -> Result<f64> {
    match value {
        V::Float(number) => Ok(*number),
        _ => unsupported("a view value is not a float"),
    }
}

#[track_caller]
fn exact_text(value: &V) -> Result<&Arc<str>> {
    match value {
        V::Str(text) => Ok(text),
        _ => unsupported("a view value is not a str"),
    }
}

#[track_caller]
fn optional_text(value: &V) -> Result<Option<Arc<str>>> {
    match value {
        V::Null => Ok(None),
        other => exact_text(other).cloned().map(Some),
    }
}

/// `type(value).__name__` of a JSON value.
fn type_name(value: &V) -> &'static str {
    match value {
        V::Null => "NoneType",
        V::Bool(_) => "bool",
        V::Int(_) | V::Big(_) => "int",
        V::Float(_) => "float",
        V::Str(_) => "str",
        V::List(_) => "list",
        V::Map(_) => "dict",
    }
}

/// `item in container`, as Python answers it for a JSON container and a str.
fn contains(container: &V, item: &str) -> Result<bool> {
    match container {
        V::List(items) => Ok(items.iter().any(|value| matches!(value, V::Str(text) if &**text == item))),
        V::Map(map) => Ok(map.contains_key(item)),
        V::Str(text) => Ok(text.contains(item)),
        other => Err(Error::Type(format!("argument of type '{}' is not iterable", type_name(other)))),
    }
}

// ---- The household -------------------------------------------------------------------------------

/// What `group_of` reads of a person's record in `population_presence_v1.people`.
#[derive(Clone, Debug, PartialEq)]
pub enum Household {
    /// No record, a record that is not a dict, or one without `household_id`: the profile's group.
    Profile,
    Is(Option<Arc<str>>),
    /// A `household_id` that is not a str or None, which the Python rule refuses when it reads it.
    Other,
}

/// `population_presence_v1.people`, for `group_of`; None for no such dict.
pub fn presence_people(world: &World) -> Result<Option<FxHashMap<Arc<str>, Household>>> {
    let presence = state(world, POPULATION_PRESENCE)?;
    let Some(V::Map(state)) = presence.as_deref() else {
        return Ok(None);
    };
    let Some(people) = state.get("people") else {
        return Ok(None);
    };
    let V::Map(people) = people else {
        return unsupported("the presence state's people are not a dict");
    };
    let mut found = FxHashMap::with_capacity_and_hasher(people.len(), Default::default());
    for (id, record) in people {
        let household = match record {
            V::Map(record) => match record.get("household_id") {
                None => Household::Profile,
                Some(V::Null) => Household::Is(None),
                Some(V::Str(text)) => Household::Is(Some(text.clone())),
                Some(_) => Household::Other,
            },
            _ => Household::Profile,
        };
        found.insert(id.clone(), household);
    }
    Ok(Some(found))
}

/// A person's `group_id` in a view: the household of the presence record, or the profile's.
#[track_caller]
pub fn group_of(
    people: Option<&FxHashMap<Arc<str>, Household>>,
    id: &str,
    profile_group: &Option<Arc<str>>,
) -> Result<Option<Arc<str>>> {
    match people.and_then(|people| people.get(id)) {
        None | Some(Household::Profile) => Ok(profile_group.clone()),
        Some(Household::Is(household)) => Ok(household.clone()),
        Some(Household::Other) => unsupported("a view value is not a str"),
    }
}

// ---- What is kept from one view to the next --------------------------------------------------------

/// What the view keeps for one controller, on its `Run`: not state. Person contexts are kept while
/// the stamps of `model_configs`, `population_context_v1` and `population_dynamics_v1` stay the
/// same, and each context while its document is equal to the copy kept with it; a zone's conditions
/// while the stamps of `model_configs` and `zone_environment_v1` stay the same. Configs given past
/// the slot have no stamp, so nothing read from them is kept whole. A stamp is never issued twice,
/// so a kept value is never one of another state.
#[derive(Default)]
pub struct ViewCache {
    people: KeptPeople,
    environment: Option<KeptEnvironment>,
    no_person: Arc<PersonFields>,
}

#[derive(Default)]
struct KeptPeople {
    stamps: Option<[Option<Stamp>; 3]>,
    found: Arc<FxHashMap<Arc<str>, Arc<PersonFields>>>,
    /// Each context parsed, with a copy of its document.
    by_id: FxHashMap<Arc<str>, (V, Arc<PersonFields>)>,
}

struct KeptEnvironment {
    stamps: (Stamp, Option<Stamp>),
    inputs: ZoneInputs,
    zones: FxHashMap<Arc<str>, Arc<ZoneConditions>>,
}

impl ViewCache {
    /// How many person contexts and zones' conditions are kept: for tests.
    pub fn kept(&self) -> (usize, usize) {
        (self.people.by_id.len(), self.environment.as_ref().map_or(0, |environment| environment.zones.len()))
    }
}

/// `state.get("people")` if both are dicts: one of the dicts `population_context_people` merges.
fn people_of(state: Option<&V>) -> Option<&IndexMap<Arc<str>, V>> {
    match state? {
        V::Map(state) => match state.get("people")? {
            V::Map(people) => Some(people),
            _ => None,
        },
        _ => None,
    }
}

/// `_parsed_person_contexts` and `_person_fields` of a world: who has a context, and its fields,
/// merged as `population_context_people` merges them, the later dict winning. Whether anyone has one.
fn person_contexts(
    world: &World,
    configs: &ModelConfigs<'_>,
    cache: &mut KeptPeople,
) -> Result<(Arc<FxHashMap<Arc<str>, Arc<PersonFields>>>, bool)> {
    let stamps = match configs.stamp {
        Some(stamp) => Some([Some(stamp), world.stamp_of(POPULATION_CONTEXT)?, world.stamp_of(POPULATION_DYNAMICS)?]),
        None => None,
    };
    if stamps.is_some() && cache.stamps == stamps {
        let any = !cache.found.is_empty();
        return Ok((cache.found.clone(), any));
    }
    let context = state(world, POPULATION_CONTEXT)?;
    let dynamics = state(world, POPULATION_DYNAMICS)?;
    // The dynamic contexts count only when they have entries, since an empty dict adds no one.
    let dynamic = match dynamics.as_deref() {
        Some(V::Map(state)) => match state.get("profile_context") {
            Some(V::Map(entries)) if !entries.is_empty() => Some(entries),
            _ => None,
        },
        _ => None,
    };
    let sources = [people_of(configs.get("population_context")), people_of(context.as_deref()), dynamic];
    let mut found = FxHashMap::default();
    for (place, source) in sources.iter().enumerate() {
        let Some(source) = source else { continue };
        for (id, raw) in source.iter() {
            if sources[place + 1..].iter().flatten().any(|later| later.contains_key(id)) {
                continue;
            }
            let fields = match cache.by_id.get(id) {
                // `entry[0] != raw`, as a NaN in a record parses it again.
                Some((kept, fields)) if kept == raw => fields.clone(),
                _ => {
                    let fields = Arc::new(person_context::parse(raw)?);
                    cache.by_id.insert(id.clone(), (raw.clone(), fields.clone()));
                    fields
                }
            };
            found.insert(id.clone(), fields);
        }
    }
    let found = Arc::new(found);
    if stamps.is_some() {
        cache.stamps = stamps;
        cache.found = found.clone();
    }
    let any = !found.is_empty();
    Ok((found, any))
}

/// An active stressor of a zone: `active_environmental_stress`.
struct Stressor {
    id: Arc<str>,
    stress_per_minute: f64,
    start_event_id: Arc<str>,
    source_id: Option<Arc<str>>,
}

/// The active stressors of each zone, by ID.
fn active_stressors(infrastructure: Option<&V>, zone_ids: &[Arc<str>]) -> Result<FxHashMap<Arc<str>, Vec<Stressor>>> {
    let mut found: FxHashMap<Arc<str>, Vec<Stressor>> = FxHashMap::default();
    let Some(V::Map(state)) = infrastructure else {
        return Ok(found);
    };
    let Some(stressors) = state.get("environmental_stressors") else {
        return unsupported("the infrastructure state has no stressors");
    };
    for (id, record) in required_dict(stressors)? {
        let record = required_dict(record)?;
        if !field(record, "active")?.is_truthy() {
            continue;
        }
        let zone = exact_text(field(record, "zone_id")?)?;
        if zone_ids.binary_search(zone).is_err() {
            continue;
        }
        let stressor = Stressor {
            id: id.clone(),
            stress_per_minute: exact_float(field(record, "stress_per_minute")?)?,
            start_event_id: exact_text(field(record, "start_event_id")?)?.clone(),
            source_id: optional_text(field(record, "source_id")?)?,
        };
        found.entry(zone.clone()).or_default().push(stressor);
    }
    #[allow(clippy::iter_over_hash_type, reason = "sorts each list in place")]
    for entries in found.values_mut() {
        entries.sort_unstable_by(|first, second| first.id.cmp(&second.id));
    }
    Ok(found)
}

fn sorted_unique(mut items: Vec<Arc<str>>) -> Vec<Arc<str>> {
    items.sort_unstable();
    items.dedup();
    items
}

/// The inputs of the zones' conditions: kept while the stamps hold, else read now.
fn kept_environment<'c>(
    world: &World,
    configs: &ModelConfigs<'_>,
    cache: &'c mut Option<KeptEnvironment>,
) -> Result<&'c mut KeptEnvironment> {
    let zone_stamp = world.stamp_of(zone_environment::STATE)?;
    let stamps = configs.stamp.map(|stamp| (stamp, zone_stamp));
    let current = match (stamps, cache.as_ref()) {
        (Some(stamps), Some(kept)) => kept.stamps == stamps,
        _ => false,
    };
    if !current {
        let environment = state(world, zone_environment::STATE)?;
        let inputs = ZoneInputs::read(configs, environment.as_deref())?;
        // Read past the slot, the configs have no stamp: what is read serves this view.
        let stamps = stamps.unwrap_or((Stamp::next(), None));
        *cache = Some(KeptEnvironment { stamps, inputs, zones: FxHashMap::default() });
    }
    match cache.as_mut() {
        Some(kept) => Ok(kept),
        None => unsupported("no zone environment"),
    }
}

/// The zones of a view: the head of `from_world`.
fn read_zones(
    world: &World,
    rows: &Entities,
    configs: &ModelConfigs<'_>,
    cache: &mut Option<KeptEnvironment>,
) -> Result<Vec<Arc<ViewZone>>> {
    let mut zone_ids: Vec<Arc<str>> = rows.zones.iter().map(|(id, _)| id.clone()).collect();
    zone_ids.sort_unstable();

    let environment = kept_environment(world, configs, cache)?;
    let infrastructure = state(world, INFRASTRUCTURE)?;
    let stressors = active_stressors(infrastructure.as_deref(), &zone_ids)?;
    // The sanitation of each zone, from the row.
    let operations = row_of::<StaffOperationsRow>(world, staff_operations::NAME)?;
    let mut result = Vec::with_capacity(zone_ids.len());
    for id in zone_ids {
        let conditions = match environment.zones.get(&id) {
            Some(conditions) => conditions.clone(),
            None => {
                let conditions = Arc::new(environment.inputs.conditions(&id)?);
                environment.zones.insert(id.clone(), conditions.clone());
                conditions
            }
        };
        let entries = stressors.get(&id).map_or(&[][..], Vec::as_slice);
        let rates: Vec<f64> = entries.iter().map(|entry| entry.stress_per_minute).collect();
        let Some(base_stress) = py_fsum(&rates) else {
            return unsupported("a sum of stress rates overflows");
        };
        let sanitation = operations.as_ref().and_then(|row| row.zones.get(&id)).map_or(1.0, |record| record.sanitation);
        let mut event_ids: Vec<Arc<str>> = entries.iter().map(|entry| entry.start_event_id.clone()).collect();
        event_ids.extend(conditions.condition_modifier_event_ids.iter().cloned());
        result.push(Arc::new(ViewZone {
            environmental_stress_per_minute: base_stress + conditions.temperature_stress_per_minute,
            environmental_stressor_event_ids: sorted_unique(event_ids),
            environmental_assumption_source_id: match conditions.condition_assumption_ids.first() {
                Some(first) if conditions.temperature_stress_per_minute != 0.0 => Some(first.clone()),
                _ => entries.first().and_then(|entry| entry.source_id.clone()),
            },
            ambient_noise_level: conditions.ambient_noise_level,
            rest_efficiency_multiplier: conditions.rest_efficiency_multiplier,
            zone_environment_modifier_event_ids: conditions.noise_modifier_event_ids.clone(),
            zone_environment_assumption_ids: conditions.noise_assumption_ids.clone(),
            ventilation_effectiveness: conditions.ventilation_effectiveness,
            lighting_level: conditions.lighting_level,
            privacy_level: conditions.privacy_level,
            temperature_c: conditions.temperature_c,
            ventilation_hygiene_growth_per_minute: conditions.ventilation_hygiene_growth_per_minute,
            privacy_discontent_per_minute: conditions.privacy_discontent_per_minute,
            condition_modifier_event_ids: conditions.condition_modifier_event_ids.clone(),
            condition_assumption_ids: conditions.condition_assumption_ids.clone(),
            demand_modifiers: conditions.demand_modifiers.clone(),
            min_rest_decision_minutes: conditions.min_rest_decision_minutes,
            max_rest_decision_minutes: conditions.max_rest_decision_minutes,
            sanitation_level: sanitation,
            id,
        }));
    }
    Ok(result)
}

/// What the rows say of the people, in ID order, and of the facilities.
fn read_rows(
    rows: &Entities,
    zones: &[Arc<ViewZone>],
    no_person: &Arc<PersonFields>,
    minute: i64,
) -> Result<(Vec<ViewAgent>, Vec<ViewFacility>)> {
    let mut first_tasks: FxHashMap<&str, &TaskRow> = FxHashMap::default();
    for (_, task) in rows.active_tasks.iter() {
        if let Some(agent) = &task.agent_id {
            if matches!(&*task.kind, "toilet_queue" | "resource_service_queue") {
                first_tasks.entry(agent).or_insert(task);
            }
        }
    }
    let waited = |since: i64| -> Result<i64> {
        match minute.checked_sub(since) {
            Some(wait) => Ok(wait.max(0)),
            None => unsupported("a minute is out of range"),
        }
    };
    let mut people: Vec<_> = rows.agents.iter().collect();
    people.sort_unstable_by(|first, second| first.0.cmp(second.0));
    let mut agents = Vec::with_capacity(people.len());
    for (id, agent) in people {
        if **id != *agent.profile.id {
            return unsupported("a person is under another ID");
        }
        let Ok(zone) = zones.binary_search_by(|zone| zone.id.cmp(&agent.zone_id)) else {
            return unsupported("a person is in an unknown zone");
        };
        let task = first_tasks.get(&**id).copied();
        let request = agent.support_request_id.as_ref().and_then(|request| rows.support_requests.get(request));
        let Some(cooldown) = agent.support_cooldown_until.checked_sub(minute) else {
            return unsupported("a minute is out of range");
        };
        agents.push(ViewAgent {
            id: id.clone(),
            group_id: agent.profile.group_id.clone(),
            zone,
            mobility: agent.profile.mobility,
            support_need: agent.profile.support_need,
            stress_sensitivity: agent.profile.stress_sensitivity,
            rng_key: agent.profile.rng_key.clone(),
            activity: agent.activity,
            target_id: agent.target_id.clone(),
            needs: agent.needs.clone(),
            stress: agent.stress,
            chronic_distress: 0.0,
            trust: agent.trust,
            next_decision_at: agent.next_decision_at,
            queue_task_id: task.map(|task| task.id.clone()),
            queue_kind: task.map(|task| task.kind.clone()),
            queue_target_id: task.and_then(|task| task.resource_id.clone()),
            queue_wait_minutes: match task {
                Some(task) => waited(task.start_minute)?,
                None => 0,
            },
            support_request_id: agent.support_request_id.clone(),
            support_request_status: request.map(|request| request.status),
            support_assigned_staff_id: request.and_then(|request| request.assigned_staff_id.clone()),
            support_wait_minutes: match request {
                Some(request) => waited(request.requested_minute)?,
                None => 0,
            },
            support_cooldown_remaining_minutes: cooldown.max(0),
            information_query_pressure: 0.0,
            misinformation_belief_pressure: 0.0,
            misinformation_effect_sources: Vec::new(),
            body: NO_BODY,
            person: no_person.clone(),
        });
    }
    let mut places: Vec<_> = rows.facilities.iter().collect();
    places.sort_unstable_by(|first, second| first.0.cmp(second.0));
    let mut facilities = Vec::with_capacity(places.len());
    for (id, facility) in places {
        if **id != *facility.id {
            return unsupported("a facility is under another ID");
        }
        facilities.push(ViewFacility {
            id: id.clone(),
            kind: facility.kind.clone(),
            zone_id: facility.zone_id.clone(),
            capacity: facility.capacity,
            working_units: facility.working_units,
            active_agent_ids: facility.active_agent_ids.clone(),
            queue_agent_ids: facility.queue_agent_ids.clone(),
            resource_active_agent_ids: facility.resource_active_agent_ids.clone(),
            resource_queue_agent_ids: facility.resource_queue_agent_ids.clone(),
            facility_event_context_ids: Vec::new(),
        });
    }
    Ok((agents, facilities))
}

/// `min(1.0, pressure)` and the sources of the false claims a person believes, in `from_world`'s
/// order. `beliefs_parse` holds, so the claims and beliefs would parse.
pub fn misinformation_of(row: &InformationRow, id: &str) -> (f64, Vec<MisinformationSource>) {
    let mut pressure = 0.0;
    let mut sources = Vec::new();
    let Some(records) = row.beliefs.get(id) else {
        return (0.0, sources);
    };
    let mut entries: Vec<_> = records.iter().collect();
    entries.sort_unstable_by(|first, second| first.0.cmp(second.0));
    for (message_id, belief) in entries {
        let Some(message) = row.messages.get(message_id) else {
            continue;
        };
        if &*message.truth_status == "false" && &*belief.belief != "unknown" && belief.belief_strength > 0.5 {
            pressure += 2.0 * (belief.belief_strength - 0.5);
            sources.push(MisinformationSource {
                message_id: message_id.clone(),
                origin_event_id: message.origin_event_id.clone(),
                last_event_id: belief.last_event_id.clone(),
                source_id: message.source_id.clone(),
                time: message.time,
            });
        }
    }
    (py_min(1.0, pressure), sources)
}

/// The query pressure of each person, and with `applies` the beliefs in false claims.
fn read_information(world: &World, applies: bool, agents: &mut [ViewAgent]) -> Result<()> {
    let Some(row) = row_of::<InformationRow>(world, information::NAME)? else {
        return Ok(());
    };
    if applies && !row.beliefs_parse() {
        return unsupported("the claims or beliefs do not parse");
    }
    for agent in agents.iter_mut() {
        let Some(pressure) = row.query_pressure.get(&*agent.id) else {
            return unsupported("a person has no query pressure");
        };
        agent.information_query_pressure = *pressure;
        if applies {
            let (pressure, sources) = misinformation_of(&row, &agent.id);
            agent.misinformation_belief_pressure = pressure;
            agent.misinformation_effect_sources = sources;
        }
    }
    Ok(())
}

/// The long-term distress and body values of each person: what the update phase writes outside
/// the entities, and so what `refresh` reads again.
fn read_body_and_distress(world: &World, agents: &mut [ViewAgent]) -> Result<()> {
    let distress = row_of::<DistressRow>(world, psychological_distress::NAME)?;
    for agent in agents.iter_mut() {
        agent.chronic_distress =
            distress.as_ref().and_then(|row| row.agents.get(&*agent.id)).map_or(0.0, |record| record.long_term_distress);
    }
    drop(distress);
    let Some(row) = row_of::<BodyRow>(world, body_state::NAME)? else {
        for agent in agents.iter_mut() {
            agent.body = NO_BODY;
        }
        return Ok(());
    };
    for agent in agents.iter_mut() {
        agent.body = match row.agents.get(&*agent.id) {
            Some(record) => BodyFields {
                sleep_debt_minutes: record.sleep_debt_minutes,
                uncertainty: record.uncertainty,
                reassurance: record.reassurance,
                discontent: record.discontent,
                positive_affect: record.positive_affect,
                negative_affect: record.negative_affect,
                hygiene_need: record.hygiene_need,
            },
            None => NO_BODY,
        };
    }
    Ok(())
}

/// `"water_station" in kind.casefold()`.
pub fn is_water_station(kind: &str) -> bool {
    casefold(kind).contains("water_station")
}

/// The values of a dict of records, as `dict.values()`.
fn records_of(value: Option<&V>) -> Result<Vec<&V>> {
    match value {
        Some(records) => Ok(required_dict(records)?.values().collect()),
        None => Ok(Vec::new()),
    }
}

/// `facility_event_context_ids` of the facilities.
fn read_facility_contexts(world: &World, facilities: &mut [ViewFacility]) -> Result<()> {
    let operational = state(world, OPERATIONAL_EVENTS)?;
    let water_outages = match operational.as_deref() {
        None => Vec::new(),
        // `get(name, {})` of a stored None raises in Python.
        Some(V::Null) => return unsupported("the operational state is None"),
        Some(state) => records_of(required_dict(state)?.get("water_outages"))?,
    };
    let infrastructure = state(world, INFRASTRUCTURE)?;
    let outages = match infrastructure.as_deref() {
        Some(V::Map(state)) => match state.get("outages") {
            Some(outages) => records_of(Some(outages))?,
            None => return unsupported("the infrastructure state has no outages"),
        },
        _ => Vec::new(),
    };
    for facility in facilities.iter_mut() {
        let mut ids: Vec<Arc<str>> = Vec::new();
        if facility.working_units < facility.capacity {
            let log = world.log()?;
            ids.extend(log.applied_facility_event_ids(&facility.id).iter().map(|id| Arc::from(id.as_str())));
        }
        if !water_outages.is_empty() && is_water_station(&facility.kind) {
            for outage in &water_outages {
                let outage = required_dict(outage)?;
                if !field(outage, "active")?.is_truthy() {
                    continue;
                }
                let kind = exact_text(field(outage, "target_kind")?)?;
                let target = exact_text(field(outage, "target_id")?)?;
                let hit = (&**kind == "zone" && *target == facility.zone_id) || (&**kind == "facility" && *target == facility.id);
                if hit {
                    ids.push(exact_text(field(outage, "outage_event_id")?)?.clone());
                }
            }
        }
        for outage in &outages {
            let outage = required_dict(outage)?;
            if field(outage, "active")?.is_truthy() && contains(field(outage, "affected_facility_ids")?, &facility.id)? {
                ids.push(exact_text(field(outage, "start_event_id")?)?.clone());
            }
        }
        facility.facility_event_context_ids = sorted_unique(ids);
    }
    Ok(())
}

/// `_misinformation_applies`: whether the selected psychology reads beliefs in false claims. These
/// are `LIFE_STATE_PSYCHOLOGY_POLICIES` of `domain/psychological_state.py`. `selection` is
/// `selected_models(model_configs)` as the PyO3 crate read it (a fault for what it raised).
pub fn misinformation_applies(selection: &std::result::Result<Arc<Selection>, Fault>) -> Result<bool> {
    let selection = match selection {
        Ok(selection) => selection,
        Err(fault) if fault.is_refusal() => return unsupported("a model ID is not a str"),
        // A failure of the Python code is Python's to raise: `from_world` ran it again.
        Err(_) => return unsupported("the Python rule raises"),
    };
    let Some(selected) = selection.get("psychology") else {
        return Err(Error::Key("psychology".to_owned()));
    };
    Ok(matches!(&**selected, "life-state-feedback-v1" | "mean-reverting-life-state-v1"))
}

/// What only Python knows, gathered by the PyO3 crate before the world is borrowed.
pub struct Inputs<'a> {
    /// `selected_models(model_configs)`, or its fault.
    pub selection: &'a std::result::Result<Arc<Selection>, Fault>,
    /// `world.model_configs` as a `V`, while it is a dict the world's slot does not hold (assigned
    /// whole); None while the slot holds it.
    pub configs: Option<&'a V>,
}

/// A view without `simulator_extensions.enabled`, which Python reads after the rest, as
/// `from_world` read it last.
pub struct Draft {
    data: ViewData,
    body_present: bool,
}

impl Draft {
    /// The view, with `autonomy_enabled` of the extensions' flag.
    pub fn finish(mut self, extensions_enabled: bool) -> ViewData {
        self.data.autonomy_enabled = extensions_enabled || self.data.has_population_context || self.body_present;
        self.data
    }
}

/// `from_world`, without its hash and the extensions' flag.
pub fn build(world: &World, inputs: &Inputs<'_>, cache: &mut ViewCache) -> Result<Draft> {
    let minute = world.minute_index()?;
    let schema_version = world.schema_version()?;
    let configs = ModelConfigs::of(world, inputs.configs)?;
    let applies = misinformation_applies(inputs.selection)?;
    let information = row_of::<InformationRow>(world, information::NAME)?.is_some();
    let (persons, has_population_context) = person_contexts(world, &configs, &mut cache.people)?;
    let rows = world.rows()?;
    let zones = read_zones(world, &rows, &configs, &mut cache.environment)?;
    let (mut agents, mut facilities) = read_rows(&rows, &zones, &cache.no_person, minute)?;
    drop(rows);

    let presence = presence_people(world)?;
    for agent in agents.iter_mut() {
        agent.group_id = group_of(presence.as_ref(), &agent.id, &agent.group_id)?;
        if let Some(person) = persons.get(&agent.id) {
            agent.person = person.clone();
        }
    }
    if information {
        read_information(world, applies, &mut agents)?;
    }
    read_body_and_distress(world, &mut agents)?;
    read_facility_contexts(world, &mut facilities)?;
    let body_present = held(world, body_state::NAME)? != Held::Absent;
    Ok(Draft {
        data: ViewData {
            minute_index: minute,
            resource_service_enabled: schema_version == 6,
            autonomy_enabled: false,
            has_population_context,
            partial: false,
            zones,
            agents,
            facilities: facilities.into_iter().map(Arc::new).collect(),
        },
        body_present,
    })
}

/// The prior view with what the update phase writes read again (the Python `after_state_update`
/// of before lane L10), without its hash.
pub fn refresh(prior: &ViewData, world: &World) -> Result<ViewData> {
    let mut agents = prior.agents.clone();
    {
        let rows = world.rows()?;
        for agent in agents.iter_mut() {
            let Some(row) = rows.agents.get(&agent.id) else {
                return unsupported("a person of the prior view has left");
            };
            agent.needs = row.needs.clone();
            agent.stress = row.stress;
            agent.trust = row.trust;
        }
    }
    read_body_and_distress(world, &mut agents)?;
    Ok(ViewData { agents, partial: false, ..prior.clone_without_agents() })
}

impl ViewData {
    fn clone_without_agents(&self) -> ViewData {
        ViewData {
            minute_index: self.minute_index,
            resource_service_enabled: self.resource_service_enabled,
            autonomy_enabled: self.autonomy_enabled,
            has_population_context: self.has_population_context,
            partial: self.partial,
            zones: self.zones.clone(),
            agents: Vec::new(),
            facilities: self.facilities.clone(),
        }
    }
}

// ---- The audit hash -----------------------------------------------------------------------------

fn key(out: &mut Vec<u8>, first: bool, name: &str) {
    if !first {
        out.push(b',');
    }
    out.push(b'"');
    out.extend_from_slice(name.as_bytes());
    out.extend_from_slice(b"\":");
}

fn write_text(out: &mut Vec<u8>, text: &str) {
    crate::json::write_json_string(out, text);
}

fn write_option(out: &mut Vec<u8>, text: &Option<Arc<str>>) {
    match text {
        Some(text) => write_text(out, text),
        None => out.extend_from_slice(b"null"),
    }
}

fn write_int(out: &mut Vec<u8>, value: i64) {
    let mut buffer = itoa::Buffer::new();
    out.extend_from_slice(buffer.format(value).as_bytes());
}

fn write_optional_int(out: &mut Vec<u8>, value: Option<i64>) {
    match value {
        Some(value) => write_int(out, value),
        None => out.extend_from_slice(b"null"),
    }
}

fn write_texts(out: &mut Vec<u8>, items: &[Arc<str>]) {
    out.push(b'[');
    for (index, item) in items.iter().enumerate() {
        if index > 0 {
            out.push(b',');
        }
        write_text(out, item);
    }
    out.push(b']');
}

/// `model_configs[name]`; a None is refused, as `_view_state_hash` raised on it.
#[track_caller]
fn hashed_config<'a>(configs: &'a ModelConfigs<'_>, name: &str) -> Result<&'a V> {
    match configs.get(name) {
        None => unsupported("a config is missing"),
        Some(V::Null) => unsupported("a config is None"),
        Some(value) => Ok(value),
    }
}

/// The people of the view hash go to the threads in runs of this many.
const AGENTS_PER_PART: usize = 64;

/// A person's entry of `agent_life_context`, after a comma if it is not the first.
fn write_life(encoder: &mut Encoder, index: usize, agent: &ViewAgent) -> Result<()> {
    if index > 0 {
        encoder.out.push(b',');
    }
    let person = &agent.person;
    let out = &mut encoder.out;
    out.extend_from_slice(b"{\"agent_id\":");
    write_text(out, &agent.id);
    key(out, false, "care_for_ids");
    write_texts(out, &person.care_for_ids);
    key(out, false, "caregiver_ids");
    write_texts(out, &person.caregiver_ids);
    key(out, false, "discontent");
    encoder.try_float(agent.body.discontent)?;
    key(&mut encoder.out, false, "hygiene_need");
    encoder.try_float(agent.body.hygiene_need)?;
    key(&mut encoder.out, false, "negative_affect");
    encoder.try_float(agent.body.negative_affect)?;
    let out = &mut encoder.out;
    key(out, false, "parent_ids");
    write_texts(out, &person.parent_ids);
    key(out, false, "personal_context_present");
    out.extend_from_slice(if person.personal_context_present { b"true" } else { b"false" });
    key(out, false, "positive_affect");
    encoder.try_float(agent.body.positive_affect)?;
    key(&mut encoder.out, false, "reassurance");
    encoder.try_float(agent.body.reassurance)?;
    let out = &mut encoder.out;
    key(out, false, "required_sleep_minutes");
    write_optional_int(out, person.required_sleep_minutes);
    key(out, false, "roles");
    write_texts(out, &person.roles);
    key(out, false, "sleep_debt_minutes");
    encoder.try_float(agent.body.sleep_debt_minutes)?;
    key(&mut encoder.out, false, "uncertainty");
    encoder.try_float(agent.body.uncertainty)?;
    let out = &mut encoder.out;
    key(out, false, "usual_sleep_minute");
    write_optional_int(out, person.usual_sleep_minute);
    key(out, false, "usual_wake_minute");
    write_optional_int(out, person.usual_wake_minute);
    out.push(b'}');
    Ok(())
}

/// A person's entry of `agents`, after a comma if it is not the first.
fn write_agent(encoder: &mut Encoder, index: usize, agent: &ViewAgent, data: &ViewData, information: bool, resource: bool) -> Result<()> {
    if index > 0 {
        encoder.out.push(b',');
    }
    let out = &mut encoder.out;
    out.push(b'{');
    key(out, true, "activity");
    write_text(out, activity_text(agent.activity));
    key(out, false, "group_id");
    write_option(out, &agent.group_id);
    key(out, false, "id");
    write_text(out, &agent.id);
    if information {
        key(out, false, "information_query_pressure");
        encoder.try_float(agent.information_query_pressure)?;
    }
    let out = &mut encoder.out;
    key(out, false, "mobility");
    write_text(out, mobility_text(agent.mobility));
    key(out, false, "needs");
    out.extend_from_slice(b"{\"fatigue\":");
    encoder.try_float(agent.needs.fatigue)?;
    encoder.out.extend_from_slice(b",\"hunger\":");
    encoder.try_float(agent.needs.hunger)?;
    if resource {
        encoder.out.extend_from_slice(b",\"hydration_need\":");
        encoder.try_float(agent.needs.hydration_need)?;
    }
    encoder.out.extend_from_slice(b",\"toilet_urgency\":");
    encoder.try_float(agent.needs.toilet_urgency)?;
    let out = &mut encoder.out;
    out.push(b'}');
    key(out, false, "next_decision_at");
    write_int(out, agent.next_decision_at);
    key(out, false, "queue_kind");
    write_option(out, &agent.queue_kind);
    key(out, false, "queue_target_id");
    write_option(out, &agent.queue_target_id);
    key(out, false, "queue_task_id");
    write_option(out, &agent.queue_task_id);
    key(out, false, "queue_wait_minutes");
    write_int(out, agent.queue_wait_minutes);
    key(out, false, "rng_key");
    write_text(out, &agent.rng_key);
    key(out, false, "stress");
    encoder.try_float(agent.stress)?;
    key(&mut encoder.out, false, "stress_sensitivity");
    encoder.try_float(agent.stress_sensitivity)?;
    let out = &mut encoder.out;
    key(out, false, "support_assigned_staff_id");
    write_option(out, &agent.support_assigned_staff_id);
    key(out, false, "support_cooldown_remaining_minutes");
    write_int(out, agent.support_cooldown_remaining_minutes);
    key(out, false, "support_need");
    encoder.try_float(agent.support_need)?;
    let out = &mut encoder.out;
    key(out, false, "support_request_id");
    write_option(out, &agent.support_request_id);
    key(out, false, "support_request_status");
    match agent.support_request_status {
        Some(status) => write_text(out, status_text(status)),
        None => out.extend_from_slice(b"null"),
    }
    key(out, false, "support_wait_minutes");
    write_int(out, agent.support_wait_minutes);
    key(out, false, "target_id");
    write_option(out, &agent.target_id);
    key(out, false, "trust");
    encoder.try_float(agent.trust)?;
    let out = &mut encoder.out;
    key(out, false, "zone_id");
    write_text(out, &data.zones[agent.zone].id);
    out.push(b'}');
    Ok(())
}

/// The entries of `agent_life_context` (if `life`) and of `agents`, in runs, in the people's order,
/// written by the threads together. The first error is the one the sections written one after the
/// other would stop at: the life context's first, then the agents'.
fn agent_parts(data: &ViewData, life: bool, information: bool, resource: bool) -> Result<(Vec<Vec<u8>>, Vec<Vec<u8>>)> {
    let parts = parts_in_order(data.agents.len(), AGENTS_PER_PART, |run| {
        let mut life_part = Encoder { out: Vec::new() };
        let mut agents_part = Encoder { out: Vec::with_capacity(run.len() * 640) };
        let life_written = if life {
            life_part.out.reserve(run.len() * 512);
            run.clone().try_for_each(|index| write_life(&mut life_part, index, &data.agents[index]))
        } else {
            Ok(())
        };
        let agents_written =
            run.into_iter().try_for_each(|index| write_agent(&mut agents_part, index, &data.agents[index], data, information, resource));
        (life_written.map(|()| life_part.out), agents_written.map(|()| agents_part.out))
    });
    let mut life_parts = Vec::with_capacity(parts.len());
    let mut agents_parts = Vec::with_capacity(parts.len());
    for (life_part, agents_part) in parts {
        life_parts.push(life_part?);
        agents_parts.push(agents_part);
    }
    let agents_parts = agents_parts.into_iter().collect::<Result<Vec<_>>>()?;
    Ok((if life { life_parts } else { Vec::new() }, agents_parts))
}

/// The canonical JSON of the document `_view_state_hash` builds, handed to `sink` in pieces whose
/// bytes, one after the other, are the document. Keys are written sorted. A float the canonical
/// form does not hold says Unsupported, as the Python rule raised its own text on it.
fn write_document(data: &ViewData, world: &World, configs: Option<&V>, mut sink: impl FnMut(&[u8])) -> Result<()> {
    let configs = ModelConfigs::of(world, configs)?;
    let resource = data.resource_service_enabled;
    let information = held(world, information::NAME)? != Held::Absent;
    let resource_config = if resource { Some(hashed_config(&configs, "resource_service")?) } else { None };
    let information_config = if information { Some(hashed_config(&configs, "information_spread")?) } else { None };
    // A row holds a namespace's dict of fields, which is never empty.
    let life_context = data.has_population_context || held(world, body_state::NAME)? != Held::Absent;

    // Each run of people is written by the threads (parallel.rs); the bytes are the same as in one piece.
    let (life_parts, agent_parts) = agent_parts(data, life_context, information, resource)?;
    let mut encoder = Encoder { out: Vec::with_capacity(32 * 1024) };
    encoder.out.push(b'{');
    let mut first = true;
    if life_context {
        key(&mut encoder.out, true, "agent_life_context");
        first = false;
        encoder.out.push(b'[');
        sink(&encoder.out);
        encoder.out.clear();
        for part in &life_parts {
            sink(part);
        }
        encoder.out.push(b']');
    }
    key(&mut encoder.out, first, "agents");
    encoder.out.push(b'[');
    sink(&encoder.out);
    encoder.out.clear();
    for part in &agent_parts {
        sink(part);
    }
    encoder.out.push(b']');
    let out = &mut encoder.out;
    key(out, false, "facilities");
    out.push(b'[');
    for (index, facility) in data.facilities.iter().enumerate() {
        if index > 0 {
            out.push(b',');
        }
        out.push(b'{');
        key(out, true, "active_agent_ids");
        write_texts(out, &facility.active_agent_ids);
        key(out, false, "capacity");
        write_int(out, facility.capacity);
        key(out, false, "id");
        write_text(out, &facility.id);
        key(out, false, "kind");
        write_text(out, &facility.kind);
        key(out, false, "queue_agent_ids");
        write_texts(out, &facility.queue_agent_ids);
        if resource {
            key(out, false, "resource_active_agent_ids");
            write_texts(out, &facility.resource_active_agent_ids);
            key(out, false, "resource_queue_agent_ids");
            write_texts(out, &facility.resource_queue_agent_ids);
        }
        key(out, false, "working_units");
        write_int(out, facility.working_units);
        key(out, false, "zone_id");
        write_text(out, &facility.zone_id);
        out.push(b'}');
    }
    out.push(b']');
    if let Some(config) = information_config {
        key(&mut encoder.out, false, "information_spread_config");
        config.write_json(&mut encoder)?;
    }
    let out = &mut encoder.out;
    key(out, false, "minute_index");
    write_int(out, data.minute_index);
    if let Some(config) = resource_config {
        key(&mut encoder.out, false, "resource_service_config");
        config.write_json(&mut encoder)?;
    }
    encoder.out.push(b'}');
    sink(&encoder.out);
    Ok(())
}

/// `canonical_sha256` of the document `_view_state_hash` builds, as lowercase hex. `configs` is
/// `world.model_configs` while the world's slot does not hold it (`Inputs::configs`).
pub fn state_hash(data: &ViewData, world: &World, configs: Option<&V>) -> Result<String> {
    let mut hasher = Sha256::new();
    write_document(data, world, configs, |piece| hasher.update(piece))?;
    let digest = hasher.finalize();
    Ok(digest.iter().map(|byte| format!("{byte:02x}")).collect())
}

/// The document `state_hash` hashes, in one piece: for tests.
pub fn document(data: &ViewData, world: &World, configs: Option<&V>) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    write_document(data, world, configs, |piece| out.extend_from_slice(piece))?;
    Ok(out)
}

pub fn activity_text(activity: Activity) -> &'static str {
    match activity {
        Activity::Idle => "idle",
        Activity::Moving => "moving",
        Activity::Queued => "queued",
        Activity::InService => "in_service",
        Activity::SeekingHelp => "seeking_help",
        Activity::Sleeping => "sleeping",
        Activity::Resting => "resting",
    }
}

pub fn mobility_text(mobility: Mobility) -> &'static str {
    match mobility {
        Mobility::Independent => "independent",
        Mobility::Assisted => "assisted",
        Mobility::Wheelchair => "wheelchair",
    }
}

pub fn status_text(status: RequestStatus) -> &'static str {
    match status {
        RequestStatus::Pending => "pending",
        RequestStatus::Assigned => "assigned",
        RequestStatus::Resolved => "resolved",
        RequestStatus::Unresolved => "unresolved",
    }
}

// A view stays shareable: a rule reads it on any thread.
#[allow(dead_code)]
fn assert_views_are_shareable() {
    fn check<T: Send + Sync>() {}
    check::<ViewData>();
    check::<ViewCache>();
}
