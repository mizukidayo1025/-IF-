//! The frame of a world (#264, stage 6 of #132): the map and the indicators of one minute, as the
//! API's `TimelineFrame` holds them (`src/sheltersim/contracts/api.py`), made from the core's World
//! alone. It is the frame `sheltersim.inspection.frames.world_frame` made before, byte for byte: the
//! same keys and values, an int where Python had an int and a float where it had a float, written
//! as canonical JSON (`V::write_json`: the keys sorted, the floats as Python writes them).
//!
//! `frame` reads the world, and through the host what Python parses (the floor plan's and the
//! spatial constraints' documents, as their contracts dump them) and what the waits of the support
//! requests read beside the world (as the metrics phase reads them: `rules/tick/metrics.rs`). It
//! changes nothing, draws nothing and calls no LLM: a frame is a read (AGENTS.md). Where it cannot
//! say what Python said (a tainted namespace, a value of another type than the rows hold), it
//! says `Unsupported` rather than guess.
//!
//! The frame's `state_hash` is the world's `state_digest` (`canonical::state_digest`) unless the
//! caller gives the one it holds (a Checkpoint's).

use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::sync::Arc;

use indexmap::IndexMap;

use crate::canonical;
use crate::config::movement::Movement;
use crate::config::{FacilityGeometry, Fault, Section, SectionId, SpatialConstraints};
use crate::error::{Error, Result};
use crate::events::packed::Unpacked;
use crate::events::{EventRow, EventStatus};
use crate::geometry::{point_in_polygon, Pt};
use crate::host::{Again, Holding, Host};
use crate::json::Encoder;
use crate::mathx::{hypot, py_min};
use crate::row::{Field, IdMap, ValueRow};
use crate::rules::common_operations::{json_value, Receipt};
use crate::rules::kit::{self, Kept};
use crate::rules::metrics::{self, MetricSample, Requests};
use crate::rules::overlay::{self, Places, PLAN_UNREAD};
use crate::rules::staff::{self as staff_rules, OpenWaits, WaitRoutes};
use crate::rules::tick::arbitrate::Staff;
use crate::rules::tick::read::is_refusal;
use crate::rules::zone_environment::{self, ZoneInputs};
use crate::state::agent_dynamics::AgentDynamicsRow;
use crate::state::body_state::BodyRow;
use crate::state::facility_queues::QueuesRow;
use crate::state::information::InformationRow;
use crate::state::movement::{MovementRow, Position};
use crate::state::shared_operations::SharedOperationsRow;
use crate::state::social_network::SocialNetworkRow;
use crate::state::staff_dynamics::StaffDynamicsRow;
use crate::state::staff_operations::StaffOperationsRow;
use crate::value::V;
use crate::world::{World, WorldCell, MODEL_CONFIGS};

const MOVEMENT: &str = "movement_v1";
const AGENT_DYNAMICS: &str = "agent_dynamics_v1";
const STAFF_DYNAMICS: &str = "staff_dynamics_v1";
const INFORMATION: &str = "information_spread_v1";
const SHARED: &str = "shared_operations_v1";
const BODY: &str = "body_state_v1";
const SOCIAL: &str = "social_network_v1";
const STAFF_OPERATIONS: &str = "staff_operations_v1";
const QUEUES: &str = "facility_queues_v1";
const DYNAMICS: &str = "population_dynamics_v1";
const PRESENCE: &str = "population_presence_v1";
const CONTEXT: &str = "population_context_v1";
const OVERLAY: &str = "geometry_overlay_v1";
const SPACES: &str = "temporary_spaces_v1";
const LEDGER: &str = "resource_ledger_v1";

/// The event types of the care records (`care_history_by_agent`).
const CARE_EVENT_TYPES: [&str; 2] = ["care_support_transition", "staff_support_continuity"];
/// The event types of a frame's interactions (`social_event_records`).
const SOCIAL_EVENT_TYPES: [&str; 3] = ["information_contact_update", "social_interaction", "information_correction"];
/// The payload keys whose people are an interaction's actors.
const ACTOR_KEYS: [&str; 4] = ["agent_id", "participant_agent_id", "sender_agent_id", "recipient_agent_id"];
/// The payload keys an interaction shows where they hold a str or an int.
const INTERACTION_FIELDS: [&str; 9] =
    ["interaction_type", "action", "topic_id", "message_id", "belief", "contact_basis", "contact_minutes", "reason_short", "zone_id"];
/// How many records back the interactions are looked for, and how many are kept.
const SOCIAL_SCAN_LIMIT: usize = 5000;
const SOCIAL_LIMIT: usize = 200;
/// The newest records a frame lists (`EventLog.recent(100)`).
const RECENT_EVENTS: usize = 100;
/// How many care records each person keeps (`care_history_by_agent`'s limit).
const CARE_HISTORY_LIMIT: usize = 20;
/// How many relationships a frame lists, of the sorted keys.
const EDGE_LIMIT: usize = 2000;
/// How many of the newest history items a frame lists.
const RECENT_HISTORY: usize = 200;
/// How many interactions and beliefs a person shows.
const DIALOGUE_LIMIT: usize = 20;
const BELIEF_LIMIT: usize = 16;
/// The squared distance within which a person lies in a bed (0.75 m).
const BED_DISTANCE_SQUARED: f64 = 0.75 * 0.75;

const POSITION_LIMITATIONS: [&str; 3] = [
    "Staff travel follows the installed zone route model; in-room obstacle avoidance and shared doorway arbitration remain approximations.",
    "Missing physical coordinates are not generated as walking trajectories.",
    "Arrival approach coordinates and walking speeds are explicit operator or source inputs, not inferred from a map.",
];
const OCCUPANCY_BASIS: &str = "occupied beds are coordinate observations within 0.75 m; reserved beds are exclusive sleep-action assignments persisted in the runtime overlay; room admissions are explicit capacity assignments";

/// What names a frame: the Run, the branch, how far the branch is computed, and the digest of the
/// world where the caller holds it (a Checkpoint's `state_hash`).
#[derive(Clone, Copy, Debug)]
pub struct Request<'a> {
    pub run_id: &'a str,
    pub branch_id: &'a str,
    pub computed_until: i64,
    pub state_hash: Option<&'a str>,
}

/// What the frame reads through the host before it reads the world.
struct Reads {
    holding: Arc<Holding>,
    /// The floor plan's document and the approach routes' lengths; None without a floor plan.
    geometry: Option<Arc<FacilityGeometry>>,
    spatial: Arc<SpatialConstraints>,
    schema: Kept<Option<i64>>,
}

/// The frame of the world now, as canonical JSON. `rng_state` is the world document's, which the
/// digest reads where `request` gives none. It changes nothing.
pub fn frame<C: WorldCell, H: Host>(cell: &C, host: &mut H, request: &Request<'_>, rng_state: &V) -> std::result::Result<Vec<u8>, H::Error> {
    let holding = kit::holding(cell, host)?;
    let staff = Staff::of(&holding)?;
    let staff_inputs = staff.inputs(None);
    // The waits of the requests read their routes only where a kind of care has free staff: first
    // without them, as the metrics phase does.
    let waits = match cell.with(|world| waits(world, &staff_inputs, None))?? {
        Some(found) => found,
        None => {
            host.read_queue_rows()?;
            let loads_staff = holding.held(STAFF_OPERATIONS, "the world holds another document than the row's");
            let plan = host.floor_plan();
            let movement = kit::kept_section_value(cell, host, &holding, SectionId::Movement, |configs| &configs.movement, Movement::parse)?;
            let support_minutes = kit::kept_section_value(
                cell,
                host,
                &holding,
                SectionId::SupportTaskMinutes,
                |configs| &configs.support_task_minutes,
                crate::config::staff::support_task_minutes,
            )?;
            let routes = WaitRoutes { loads_staff: &loads_staff, geo: plan.as_ref().ok(), movement: &movement, support_minutes: &support_minutes };
            let found = cell.with(|world| waits(world, &staff_inputs, Some(&routes)))?.map_err(H::Error::from);
            let found = match (found, plan) {
                (Err(error), Err(read)) if is_refusal(&error).as_deref() == Some(PLAN_UNREAD) => return Err(read),
                (found, _) => found?,
            };
            found.ok_or_else(|| Error::Runtime("the waits asked for their routes again".to_owned()))?
        }
    };
    let geometry = match kit::section(cell, host, &holding, SectionId::FacilityGeometry, |configs| &configs.facility_geometry)? {
        Section::Ok(geometry) => Some(geometry),
        Section::Absent => None,
        Section::Fault(fault) => return Err(raise(host, SectionId::FacilityGeometry, &fault)),
    };
    let spatial = match kit::section(cell, host, &holding, SectionId::SpatialConstraints, |configs| &configs.spatial_constraints)? {
        Section::Ok(spatial) => spatial,
        Section::Absent => return Err(Error::unsupported("the configs have no spatial constraints").into()),
        Section::Fault(fault) => return Err(raise(host, SectionId::SpatialConstraints, &fault)),
    };
    let schema = kit::kept_section_value(cell, host, &holding, SectionId::OverlaySchema, |configs| &configs.overlay_schema, crate::config::overlay::overlay_schema)?;
    let reads = Reads { holding, geometry, spatial, schema };
    let wanted = cell.with(wanted_records)??;
    let records = records(cell, host, &wanted.all())?;
    let state_hash = match request.state_hash {
        Some(state_hash) => Arc::from(state_hash),
        None => Arc::from(digest(cell, host, rng_state)?),
    };
    let made = cell.with(|world| -> Result<Vec<u8>> {
        let frame = build(world, &reads, &waits, &Records { by_position: records, wanted }, request, state_hash)?;
        V::Map(frame).to_json()
    })?;
    Ok(made?)
}

// ---- The log's records --------------------------------------------------------------------------

/// The positions of the records a frame reads.
struct Wanted {
    /// The newest records (`EventLog.recent(100)`).
    recent: std::ops::Range<usize>,
    /// The newest applied interactions (`social_event_records`), and whether records were left
    /// unscanned.
    social: Vec<usize>,
    social_truncated: bool,
    /// Every record of the care types, in append order (`care_history_by_agent`).
    care: Vec<usize>,
}

impl Wanted {
    fn all(&self) -> Vec<usize> {
        let mut all: Vec<usize> = self.recent.clone().chain(self.social.iter().copied()).chain(self.care.iter().copied()).collect();
        all.sort_unstable();
        all.dedup();
        all
    }
}

fn wanted_records(world: &mut World) -> Result<Wanted> {
    let log = world.log()?;
    let count = log.len();
    let social_types: HashSet<u32> = SOCIAL_EVENT_TYPES.iter().filter_map(|name| log.type_id(name)).collect();
    let (social, social_truncated) = log.recent_matching(&social_types, SOCIAL_LIMIT, SOCIAL_SCAN_LIMIT);
    let care_types: Vec<u32> = CARE_EVENT_TYPES.iter().filter_map(|name| log.type_id(name)).collect();
    Ok(Wanted { recent: count.saturating_sub(RECENT_EVENTS)..count, social, social_truncated, care: log.of_type_ids_from(&care_types, 0) })
}

/// The records at these positions: from the core's log, and from the host those whose bytes
/// Python took.
fn records<C: WorldCell, H: Host>(cell: &C, host: &mut H, positions: &[usize]) -> std::result::Result<BTreeMap<usize, Unpacked>, H::Error> {
    let (mut found, missing) = cell.with(|world| -> Result<_> {
        let log = world.log()?;
        let mut found = BTreeMap::new();
        let mut missing = Vec::new();
        for &index in positions {
            match log.record(index)? {
                Some(record) => {
                    found.insert(index, record);
                }
                None => missing.push(index),
            }
        }
        Ok((found, missing))
    })??;
    if !missing.is_empty() {
        host.read_records(&missing, &mut |index, raw| {
            found.insert(index, unpacked(raw)?);
            Ok(())
        })?;
    }
    if found.len() != positions.len() {
        return Err(Error::Runtime("the host did not give every record".to_owned()).into());
    }
    Ok(found)
}

fn unpacked(raw: &[u8]) -> Result<Unpacked> {
    let (event, realized_minute, status, detail) = EventRow::from_saved_record(raw).ok_or_else(|| Error::unsupported("bytes that are not a record"))?;
    Ok(Unpacked { event, realized_minute, status, detail })
}

/// How many records the digest reads from the world at a time.
const DIGEST_BATCH: usize = 4096;

/// `world.state_digest()` (`canonical::state_digest`), the log's records read a batch at a time,
/// those whose bytes Python took from the host.
fn digest<C: WorldCell, H: Host>(cell: &C, host: &mut H, rng_state: &V) -> std::result::Result<String, H::Error> {
    // The document holds the whole queue, Python's pending events too (H6).
    host.read_queue_rows()?;
    let count = cell.with(|world| world.log().map(|log| log.len()))??;
    let mut list = canonical::ListNode::new();
    let mut start = 0;
    while start < count {
        let end = (start + DIGEST_BATCH).min(count);
        let mut batch = cell.with(|world| -> Result<Vec<Option<Vec<u8>>>> {
            let log = world.log()?;
            let mut batch = Vec::with_capacity(end - start);
            for index in start..end {
                let mut raw = Vec::new();
                batch.push(log.write_record(index, &mut raw)?.then_some(raw));
            }
            Ok(batch)
        })??;
        let missing: Vec<usize> = (start..end).filter(|index| batch[index - start].is_none()).collect();
        if !missing.is_empty() {
            host.read_records(&missing, &mut |index, raw| {
                batch[index - start] = Some(raw.to_vec());
                Ok(())
            })?;
        }
        for raw in batch {
            list.push(&raw.ok_or_else(|| Error::Runtime("the host did not give every record".to_owned()))?);
        }
        start = end;
    }
    let log = list.node();
    Ok(cell.with(|world| canonical::tree_digest(world, rng_state, log))??)
}

/// The records a frame reads, by their position in the log.
struct Records {
    by_position: BTreeMap<usize, Unpacked>,
    wanted: Wanted,
}

impl Records {
    fn at(&self, index: usize) -> Result<&Unpacked> {
        self.by_position.get(&index).ok_or_else(|| Error::Runtime("a record the frame did not read".to_owned()))
    }
}

/// What a read site raises for a fault of a section Python parses (`Tick::raise`).
fn raise<H: Host>(host: &mut H, which: SectionId, fault: &Fault) -> H::Error {
    if fault.is_refusal() {
        return Error::unsupported(fault.text.to_string()).into();
    }
    match host.read_again(Again::Section(which)) {
        Err(error) => error,
        Ok(()) => Error::corrupt(MODEL_CONFIGS, "a config fault did not reproduce").into(),
    }
}

// ---- The waits ----------------------------------------------------------------------------------

/// What the frame reads of the waits: the minute's `MetricSnapshot` and each care request's wait.
struct Waits {
    sample: MetricSample,
    /// `support_waited_minutes` and `support_wait_reason_codes` of each care request with a
    /// support request, by its ID.
    care: BTreeMap<Arc<str>, (i64, Vec<Arc<str>>)>,
}

/// The waits, or None where they need their routes and `routes` is None.
fn waits(world: &World, staff: &staff_rules::Inputs<'_>, routes: Option<&WaitRoutes<'_>>) -> Result<Option<Waits>> {
    let minute = world.minute_index()?;
    let inputs = metrics::Inputs { sample: minute, requests: Requests::Unsettled(None), staff, routes };
    let Some(sample) = metrics::sample(world, &inputs)? else {
        return Ok(None);
    };
    let ids: Vec<Arc<str>> = {
        let Some(shared) = typed::<SharedOperationsRow>(world, SHARED)? else {
            return Ok(Some(Waits { sample, care: BTreeMap::new() }));
        };
        let rows = world.rows()?;
        shared.support_requests.map.keys().filter(|id| rows.support_requests.get(id).is_some()).cloned().collect()
    };
    let Some(open) = staff_rules::open_waits(world, staff, routes, &ids, minute, false)? else {
        return Ok(None);
    };
    Ok(Some(Waits { sample, care: care_waits(&open, &ids) }))
}

fn care_waits(open: &OpenWaits, ids: &[Arc<str>]) -> BTreeMap<Arc<str>, (i64, Vec<Arc<str>>)> {
    let mut care = BTreeMap::new();
    for request in &open.requests {
        let reasons = open.reasons_of(request).into_iter().map(Arc::from).collect();
        care.insert(ids[request.index].clone(), (request.waited, reasons));
    }
    care
}

// ---- Values -------------------------------------------------------------------------------------

type Map = IndexMap<Arc<str>, V>;

fn text(value: &str) -> Arc<str> {
    Arc::from(value)
}

fn string(value: &str) -> V {
    V::Str(Arc::from(value))
}

fn optional(value: &Option<Arc<str>>) -> V {
    value.clone().map_or(V::Null, V::Str)
}

fn int(value: usize) -> V {
    V::Int(value as i64)
}

/// The value of a typed field, as its canonical JSON reads back.
fn field<T: Field>(value: &T) -> Result<V> {
    let mut encoder = Encoder::with_capacity(256);
    value.write_json(&mut encoder)?;
    json_value(&encoder.out)
}

fn point(x: f64, y: f64) -> V {
    V::Map(IndexMap::from([(text("x"), V::Float(x)), (text("y"), V::Float(y))]))
}

/// A number as `Point2D` takes it: a float, or an int as its float.
fn coordinate(value: &V) -> Result<f64> {
    match value {
        V::Float(number) => Ok(*number),
        V::Int(number) => Ok(*number as f64),
        _ => Err(Error::unsupported("a coordinate is not a number")),
    }
}

/// `Point2D.model_validate(value)`.
fn point_of(value: &V) -> Result<Pt> {
    let V::Map(map) = value else {
        return Err(Error::unsupported("a point is not a dict"));
    };
    let x = coordinate(map.get("x").ok_or_else(|| Error::unsupported("a point without x"))?)?;
    let y = coordinate(map.get("y").ok_or_else(|| Error::unsupported("a point without y"))?)?;
    Ok(Pt { x, y })
}

fn points_of(value: &V) -> Result<Vec<Pt>> {
    match value {
        V::List(items) => items.iter().map(point_of).collect(),
        _ => Err(Error::unsupported("the points are not a list")),
    }
}

fn point_value(at: Pt) -> V {
    point(at.x, at.y)
}

/// `point_in_polygon`, which reads `polygon[-1]` first.
fn inside(at: Pt, polygon: &[Pt]) -> Result<bool> {
    if polygon.is_empty() {
        return Err(Error::unsupported("an empty polygon"));
    }
    Ok(point_in_polygon(at, polygon))
}

fn get<'v>(value: &'v V, key: &str) -> Option<&'v V> {
    match value {
        V::Map(map) => map.get(key),
        _ => None,
    }
}

/// `value[key]`: a missing key raises in Python.
fn item<'v>(value: &'v V, key: &str) -> Result<&'v V> {
    get(value, key).ok_or_else(|| Error::unsupported(format!("a record without {key}")))
}

fn as_int(value: &V) -> Result<i64> {
    match value {
        V::Int(number) => Ok(*number),
        _ => Err(Error::unsupported("a value is not an int")),
    }
}

fn as_list(value: &V) -> Result<&[V]> {
    match value {
        V::List(items) => Ok(items),
        _ => Err(Error::unsupported("a value is not a list")),
    }
}

fn as_map(value: &V) -> Result<&Map> {
    match value {
        V::Map(map) => Ok(map),
        _ => Err(Error::unsupported("a value is not a dict")),
    }
}

/// The typed row of a namespace, None without the key; a tainted one is refused.
fn typed<'w, T: crate::row::Row>(world: &'w World, name: &str) -> Result<Option<crate::cell::ShardRef<'w, T>>> {
    let Some(slot) = world.slots.get(name) else {
        return Ok(None);
    };
    let held = {
        let slot = slot.try_borrow()?;
        match (&slot.row, &slot.taint) {
            (Some(_), _) => true,
            (None, Some(taint)) => return Err(Error::unsupported(format!("{name} is tainted: {taint}"))),
            (None, None) => false,
        }
    };
    if !held {
        return Ok(None);
    }
    world.row::<T>(name).map(Some)
}

/// `model_state.get(name)` of a namespace held as a `V`.
fn state(world: &World, name: &str) -> Result<Option<V>> {
    if let Some(slot) = world.slots.get(name) {
        let slot = slot.try_borrow()?;
        if slot.row.is_none() {
            if let Some(taint) = &slot.taint {
                return Err(Error::unsupported(format!("{name} is tainted: {taint}")));
            }
        }
    }
    Ok(world.slots.try_value(name)?.map(|value| value.clone()))
}

/// `read_only_part(world, name, key, default)`: the part of a dict namespace, else the default.
fn part(state: Option<&V>, key: &str) -> Option<V> {
    match state {
        Some(V::Map(map)) => map.get(key).cloned(),
        _ => None,
    }
}

// ---- The interactions and the care records -------------------------------------------------------

/// `social_event_records`: the newest applied interactions.
fn social_events(records: &Records) -> Result<Vec<V>> {
    let mut events = Vec::with_capacity(records.wanted.social.len());
    for &index in &records.wanted.social {
        let record = records.at(index)?;
        let event = &record.event;
        if !SOCIAL_EVENT_TYPES.contains(&&*event.event_type) || record.status != EventStatus::Applied {
            continue;
        }
        let actors: BTreeSet<Arc<str>> = ACTOR_KEYS
            .iter()
            .filter_map(|key| match get(&event.payload, key) {
                Some(V::Str(actor)) => Some(actor.clone()),
                _ => None,
            })
            .collect();
        let mut projected = Map::new();
        projected.insert(text("event_id"), V::Str(event.event_id.clone()));
        projected.insert(text("minute"), V::Int(record.realized_minute));
        projected.insert(text("event_type"), V::Str(event.event_type.clone()));
        projected.insert(text("actors"), V::List(actors.into_iter().map(V::Str).collect()));
        projected.insert(text("status"), string(record.status.as_str()));
        projected.insert(text("causation_id"), optional(&event.causation_id));
        for key in INTERACTION_FIELDS {
            if let Some(value @ (V::Str(_) | V::Int(_) | V::Big(_))) = get(&event.payload, key) {
                projected.insert(text(key), value.clone());
            }
        }
        events.push(V::Map(projected));
    }
    Ok(events)
}

/// `care_history_by_agent(world, limit=20)`: each person's applied care receipts, newest last.
fn care_history(records: &Records) -> Result<BTreeMap<Arc<str>, Vec<V>>> {
    let mut rows: BTreeMap<Arc<str>, Vec<V>> = BTreeMap::new();
    for &index in &records.wanted.care {
        let record = records.at(index)?;
        if record.status != EventStatus::Applied {
            continue;
        }
        let event = &record.event;
        let continuity = &*event.event_type == "staff_support_continuity";
        let receipt = Receipt::check(&event.payload).map_err(|_| Error::unsupported("a care receipt its contract does not take as it is"))?;
        let (agent_id, related, semantics) = match (&receipt, continuity) {
            (Receipt::Continuity(found), true) => (found.agent_id.clone(), Vec::new(), "applied_staff_support_continuity_after_interruption"),
            (Receipt::Care(found), false) => {
                (found.agent_id.clone(), found.state_changes.iter().map(|change| change.entity_id.clone()).collect(), "applied_typed_care_transition")
            }
            _ => return Err(Error::unsupported("a care receipt of another event type")),
        };
        if *receipt.event_id() != event.event_id || receipt.minute() != record.realized_minute || receipt.causation_id() != event.causation_id.as_ref() {
            continue;
        }
        let V::Map(mut row) = receipt.document() else {
            return Err(Error::Runtime("a receipt's document is not a dict".to_owned()));
        };
        row.insert(text("realized_minute"), V::Int(record.realized_minute));
        row.insert(text("source_id"), optional(&event.source_id));
        row.insert(text("time_semantics"), string("recorded_event_minute"));
        row.insert(text("evidence_semantics"), string(semantics));
        let mut affected: BTreeSet<Arc<str>> = related.into_iter().collect();
        affected.insert(agent_id.clone());
        for identity in affected {
            let mut person_row = row.clone();
            let role = if identity == agent_id { "request_person" } else { "explicit_related_state_change" };
            person_row.insert(text("person_role_in_record"), string(role));
            let history = rows.entry(identity).or_default();
            history.push(V::Map(person_row));
            if history.len() > CARE_HISTORY_LIMIT {
                history.remove(0);
            }
        }
    }
    Ok(rows)
}

// ---- The floor plan ------------------------------------------------------------------------------

/// `effective_geometry(world).model_dump(mode="json")`: the floor plan's document with the overlay's
/// facility moves, each facility's service points moved with it.
fn effective_geometry(document: &V, overrides: Option<&V>) -> Result<V> {
    let overrides = match overrides {
        Some(V::Map(overrides)) if !overrides.is_empty() => overrides,
        _ => return Ok(document.clone()),
    };
    let mut geometry = document.clone();
    let V::Map(map) = &mut geometry else {
        return Err(Error::unsupported("the floor plan's document is not a dict"));
    };
    let original: Map = as_map(map.get("facility_points").ok_or_else(|| Error::unsupported("no facility points"))?)?.clone();
    let mut points = original.clone();
    for (key, row) in overrides {
        points.insert(key.clone(), point_value(point_of(item(row, "point_m")?)?));
    }
    map.insert(text("facility_points"), V::Map(points));
    if let Some(V::List(service_points)) = map.get_mut("service_points") {
        for service_point in service_points.iter_mut() {
            let facility = match get(service_point, "facility_id") {
                Some(V::Str(facility)) => facility.clone(),
                _ => return Err(Error::unsupported("a service point without its facility")),
            };
            let Some(row) = overrides.get(&facility) else {
                continue;
            };
            let old = point_of(original.get(&facility).ok_or_else(|| Error::unsupported("a moved facility without its point"))?)?;
            let new = point_of(item(row, "point_m")?)?;
            let at = point_of(item(service_point, "point_m")?)?;
            if let V::Map(service_point) = service_point {
                service_point.insert(text("point_m"), point(at.x + new.x - old.x, at.y + new.y - old.y));
            }
        }
    }
    Ok(geometry)
}

/// `effective_spatial_constraints(world).model_dump(mode="json")`.
fn effective_spatial(document: &V, overlay: Option<&V>, configs: &Map) -> Result<V> {
    let mut constraints = document.clone();
    let V::Map(map) = &mut constraints else {
        return Err(Error::unsupported("the spatial constraints' document is not a dict"));
    };
    let empty = V::Map(Map::new());
    let overlay = match overlay {
        Some(overlay @ V::Map(_)) => overlay,
        _ => &empty,
    };
    if let Some(partitions) = get(overlay, "partitions") {
        let mut partitions: Vec<(&Arc<str>, &V)> = as_map(partitions)?.iter().collect();
        partitions.sort_by(|first, second| first.0.as_bytes().cmp(second.0.as_bytes()));
        let V::List(obstacles) = map.get_mut("obstacles").ok_or_else(|| Error::unsupported("no obstacles"))? else {
            return Err(Error::unsupported("the obstacles are not a list"));
        };
        for (key, row) in partitions {
            let polygon: Vec<V> = points_of(item(row, "polygon_m")?)?.into_iter().map(point_value).collect();
            let mut obstacle = Map::new();
            obstacle.insert(text("id"), V::Str(key.clone()));
            obstacle.insert(text("polygon"), V::List(polygon));
            obstacle.insert(text("source_ids"), V::List(as_list(item(row, "source_ids")?)?.to_vec()));
            obstacles.push(V::Map(obstacle));
        }
    }
    let moves = get(overlay, "facility_points");
    if let Some(V::List(footprints)) = map.get_mut("equipment_footprints") {
        for footprint in footprints.iter_mut() {
            let facility = match get(footprint, "facility_id") {
                Some(V::Str(facility)) => facility.clone(),
                _ => return Err(Error::unsupported("a footprint without its facility")),
            };
            let Some(row) = moves.and_then(|moves| get(moves, &facility)) else {
                continue;
            };
            let original = configs.get("facility_geometry").and_then(|geometry| get(geometry, "facility_points")).and_then(|points| get(points, &facility));
            let old = point_of(original.ok_or_else(|| Error::unsupported("a moved footprint without its point"))?)?;
            let new = point_of(item(row, "point_m")?)?;
            let polygon: Vec<V> =
                points_of(item(footprint, "polygon")?)?.into_iter().map(|at| point(at.x + new.x - old.x, at.y + new.y - old.y)).collect();
            if let V::Map(footprint) = footprint {
                footprint.insert(text("polygon"), V::List(polygon));
            }
        }
    }
    Ok(constraints)
}

/// The service points of a floor plan's document.
fn service_points(geometry: &V) -> Result<&[V]> {
    match get(geometry, "service_points") {
        Some(V::List(points)) => Ok(points),
        _ => Err(Error::unsupported("the floor plan has no service points")),
    }
}

/// `legacy_service_points(geometry, [facility])`'s point of a facility at `at`.
fn legacy_service_point(facility_id: &str, capacity: i64, at: &V) -> Result<V> {
    use sha2::{Digest, Sha256};
    let digest = crate::checkpoint::hex(&Sha256::digest(facility_id.as_bytes()));
    let mut access = Map::new();
    access.insert(text("accessibility"), string("unknown"));
    access.insert(text("required_access_tags"), V::List(Vec::new()));
    access.insert(text("required_staff_skills"), V::List(Vec::new()));
    let point_m = point_of(at)?;
    let mut found = Map::new();
    found.insert(text("schema_version"), V::Int(1));
    found.insert(text("id"), V::Str(Arc::from(format!("legacy-service:{}", &digest[..24]))));
    found.insert(text("facility_id"), string(facility_id));
    found.insert(text("point_m"), point_value(point_m));
    found.insert(text("access"), V::Map(access));
    found.insert(text("service_capacity"), V::Int(capacity));
    found.insert(text("queue_id"), string(facility_id));
    Ok(V::Map(found))
}

// ---- The frame ----------------------------------------------------------------------------------

/// `MetricSnapshot.from_world(world).csv_row()` without `source_ids_json`.
fn kpis(sample: &MetricSample, minute: i64) -> Map {
    let mut kpis = Map::new();
    let mut put = |name: &str, value: V| {
        kpis.insert(text(name), value);
    };
    put("minute_index", V::Int(minute));
    put("toilet_queue_length", int(sample.toilet_queue_length));
    put("toilet_active_users", int(sample.toilet_active_users));
    put("toilet_working_units", V::Int(sample.toilet_working_units));
    put("toilet_wait_count", V::Float(sample.toilet_wait_count));
    put("toilet_wait_total_minutes", V::Float(sample.toilet_wait_total_minutes));
    put("toilet_wait_mean_minutes", V::Float(sample.toilet_wait_mean_minutes));
    put("toilet_wait_max_minutes", V::Float(sample.toilet_wait_max_minutes));
    put("stress_mean", V::Float(sample.stress_mean));
    put("stress_high_count", int(sample.stress_high_count));
    put("staff_assigned_task_count", int(sample.staff_assigned_task_count));
    put("staff_busy_count", int(sample.staff_busy_count));
    put("staff_load_ratio", V::Float(sample.staff_load_ratio));
    put("unresolved_support_request_count", int(sample.unresolved_support_request_count));
    put("support_no_qualified_worker_count", int(sample.support_no_qualified_worker_count));
    put("support_no_qualified_worker_request_minutes", V::Float(sample.support_no_qualified_worker_request_minutes));
    put("support_fairness_promoted_assignment_count", V::Float(sample.support_fairness_promoted_assignment_count));
    put("support_wait_staff_busy_request_minutes", V::Float(sample.support_wait_staff_busy_request_minutes));
    put("support_wait_staff_off_shift_request_minutes", V::Float(sample.support_wait_staff_off_shift_request_minutes));
    kpis
}

/// The resource ledger's KPI columns (`resource_metric_values`), in their order.
const RESOURCE_COLUMNS: [[&str; 6]; 2] = [
    ["water_ml_initial", "water_ml_delivered", "water_ml_served", "water_ml_losses", "water_ml_remaining", "water_ml_reserved"],
    [
        "food_meal_units_initial",
        "food_meal_units_delivered",
        "food_meal_units_served",
        "food_meal_units_losses",
        "food_meal_units_remaining",
        "food_meal_units_reserved",
    ],
];

/// `occupancy_snapshot(world)["zones"]`.
fn occupancy_zones(world: &World, reads: &Reads, configs: &Map) -> Result<Vec<V>> {
    let rows = world.rows()?;
    let mut counts: IndexMap<Arc<str>, i64> = rows.zones.iter().map(|(id, _)| (id.clone(), 0)).collect();
    for (_, person) in rows.agents.iter() {
        *counts.get_mut(&person.zone_id).ok_or_else(|| Error::unsupported("a person in a zone the world does not have"))? += 1;
    }
    let mut zones: Vec<(Arc<str>, Option<i64>)> = rows.zones.iter().map(|(id, zone)| (id.clone(), zone.capacity)).collect();
    drop(rows);
    zones.sort_by(|first, second| first.0.as_bytes().cmp(second.0.as_bytes()));
    let planning = configs.get("spatial_constraints").and_then(|constraints| get(constraints, "planning_capacity_by_zone"));
    let spaces = reads.holding.held(SPACES, "the world holds another document than the row's");
    let overlay_held = reads.holding.held(OVERLAY, "the world holds another document than the row's");
    let places = Places { spaces: &spaces, overlay: &overlay_held, schema: &reads.schema };
    let mut found = Vec::with_capacity(zones.len());
    for (zone_id, capacity) in zones {
        let occupants = counts[&zone_id];
        let metrics = overlay::zone_capacity(world, &places, &zone_id, Some(occupants))?;
        let planning_capacity = match planning.and_then(|planning| get(planning, &zone_id)) {
            Some(V::Int(value)) => Some(*value),
            Some(V::Null) => None,
            Some(_) => return Err(Error::unsupported("a planning capacity is not an int")),
            None => capacity,
        };
        let mut zone = Map::new();
        let mut put = |name: &str, value: V| {
            zone.insert(text(name), value);
        };
        let or_null = |value: Option<i64>| value.map_or(V::Null, V::Int);
        put("zone_id", V::Str(zone_id.clone()));
        put("occupants", V::Int(occupants));
        put("hard_capacity", or_null(capacity));
        let (physical, people, retained) = match &metrics {
            Some((nominal, metrics)) => {
                put("nominal_capacity", V::Int(*nominal));
                put("effective_capacity", V::Int(metrics.effective));
                put("temporary_people_capacity", V::Int(metrics.active_people));
                put("retained_expired_capacity", V::Int(metrics.retained));
                put("temporary_sleeping_capacity", V::Int(metrics.sleeping));
                put("physical_room_capacity", V::Int(metrics.room_capacity));
                put("active_room_admissions", V::Int(metrics.active_admissions));
                put("retained_expired_room_admissions", V::Int(metrics.retained_rooms));
                put("effective_total_capacity", V::Int(metrics.total()?));
                (metrics.room_capacity, metrics.active_people, metrics.retained)
            }
            None => {
                put("nominal_capacity", V::Null);
                put("effective_capacity", V::Null);
                for name in [
                    "temporary_people_capacity",
                    "retained_expired_capacity",
                    "temporary_sleeping_capacity",
                    "physical_room_capacity",
                    "active_room_admissions",
                    "retained_expired_room_admissions",
                ] {
                    put(name, V::Int(0));
                }
                put("effective_total_capacity", V::Null);
                (0, 0, 0)
            }
        };
        put("occupied_above_nominal", V::Int(capacity.map_or(0, |capacity| (occupants - capacity).max(0))));
        put("planning_capacity", or_null(planning_capacity));
        put("planning_excess", planning_capacity.map_or(V::Null, |planning| V::Int((occupants - planning).max(0))));
        let status = if physical != 0 {
            "physical_room_and_zone_capacity"
        } else if people != 0 || retained != 0 {
            "zone_only_capacity"
        } else {
            "no_temporary_space"
        };
        put("geometry_status", string(status));
        found.push(V::Map(zone));
    }
    Ok(found)
}

/// The frame's fields, before `state_hash` where the request gives none.
fn build(world: &World, reads: &Reads, waits: &Waits, records: &Records, request: &Request<'_>, state_hash: Arc<str>) -> Result<Map> {
    let minute = world.minute_index()?;
    let duration = world.duration_minutes()?;
    let configs_row = world.slots.try_row::<ValueRow>(MODEL_CONFIGS)?;
    let configs = as_map(&configs_row.0)?;

    let geometry_document = match &reads.geometry {
        Some(geometry) => Some(geometry.document.as_ref().map_err(|fault| Error::unsupported(fault.text.to_string()))?.clone()),
        None => None,
    };
    let overlay = state(world, OVERLAY)?;
    let geometry = match &geometry_document {
        Some(document) => Some(effective_geometry(document, part(overlay.as_ref(), "facility_points").as_ref())?),
        None => None,
    };

    let movement = typed::<MovementRow>(world, MOVEMENT)?;
    let dynamics = state(world, DYNAMICS)?;
    let traits = match part(dynamics.as_ref(), "profile_traits") {
        Some(traits) => traits,
        None => match configs.get("population") {
            None => V::Map(Map::new()),
            Some(population) => get(population, "people").cloned().unwrap_or_else(|| V::Map(Map::new())),
        },
    };
    let agent_dynamics = typed::<AgentDynamicsRow>(world, AGENT_DYNAMICS)?;
    let staff_dynamics = typed::<StaffDynamicsRow>(world, STAFF_DYNAMICS)?;
    let information = typed::<InformationRow>(world, INFORMATION)?;
    let shared = typed::<SharedOperationsRow>(world, SHARED)?;
    let care_histories = care_history(records)?;
    let context_people = population_context(world, configs, dynamics.as_ref())?;
    let social_events = social_events(records)?;
    let social_truncated = records.wanted.social_truncated;
    let presence = state(world, PRESENCE)?;

    let rows = world.rows()?;
    let mut household_of: BTreeMap<Arc<str>, Option<Arc<str>>> = BTreeMap::new();
    let mut group_zones: BTreeMap<Arc<str>, BTreeSet<Arc<str>>> = BTreeMap::new();
    for (id, person) in rows.agents.iter() {
        let household = kit::household_of(presence.as_ref(), &rows, id)?;
        if let Some(household) = &household {
            group_zones.entry(household.clone()).or_default().insert(person.zone_id.clone());
        }
        household_of.insert(id.clone(), household);
    }

    let mut ids: Vec<&Arc<str>> = rows.agents.iter().map(|(id, _)| id).collect();
    ids.sort_by(|first, second| first.as_bytes().cmp(second.as_bytes()));
    let mut agents = Vec::with_capacity(ids.len());
    let mut people = Vec::with_capacity(ids.len());
    for id in &ids {
        let agent = rows.agents.get(id).ok_or_else(|| Error::Runtime("a person left the table".to_owned()))?;
        let record = movement.as_ref().and_then(|movement| movement.positions.get(*id));
        let zone = rows.zones.get(&agent.zone_id).ok_or_else(|| Error::unsupported("a person in a zone the world does not have"))?;
        let mut at = record.and_then(|record| record.point).map(|found| Pt { x: found.x, y: found.y });
        let mut kind: Arc<str> = record.map_or_else(|| text("unavailable"), |record| record.position_kind.clone());
        if at.is_none() && geometry.is_none() {
            if let (Some(x), Some(y)) = (zone.display_x, zone.display_y) {
                at = Some(Pt { x, y });
                kind = text("display_only");
            }
        }
        let mut position = Map::new();
        position.insert(text("agent_id"), V::Str((*id).clone()));
        position.insert(text("zone_id"), V::Str(record.map_or_else(|| agent.zone_id.clone(), |record| record.zone_id.clone())));
        position.insert(text("point"), at.map_or(V::Null, point_value));
        position.insert(text("edge_id"), record.map_or(V::Null, |record| optional(&record.edge_id)));
        position.insert(text("progress"), V::Float(record.map_or(0.0, |record| record.progress)));
        position.insert(text("position_kind"), V::Str(kind));
        agents.push(V::Map(position));

        let mut dialogue: Vec<V> = social_events
            .iter()
            .filter(|event| {
                let acts = matches!(get(event, "actors"), Some(V::List(actors)) if actors.iter().any(|actor| matches!(actor, V::Str(actor) if actor == *id)));
                acts || (matches!(get(event, "event_type"), Some(V::Str(kind)) if &**kind == "information_correction")
                    && matches!(get(event, "zone_id"), Some(V::Str(zone)) if *zone == agent.zone_id))
            })
            .cloned()
            .collect();
        if dialogue.len() > DIALOGUE_LIMIT {
            dialogue.drain(..dialogue.len() - DIALOGUE_LIMIT);
        }
        let household = household_of.get(*id).cloned().flatten();
        let separated = group_zones.get(household.as_deref().unwrap_or("")).is_some_and(|zones| zones.iter().any(|zone| *zone != agent.zone_id));
        let mut care_requests = Vec::new();
        if let Some(shared) = &shared {
            for (request_id, head) in &shared.support_requests.map {
                if head.agent_id != **id {
                    continue;
                }
                let V::Map(mut projected) = field(head)? else {
                    return Err(Error::Runtime("a care request is not a dict".to_owned()));
                };
                let support = rows.support_requests.get(request_id);
                projected.insert(text("assigned_staff_id"), support.map_or(V::Null, |support| optional(&support.assigned_staff_id)));
                projected.insert(text("runtime_status"), support.map_or(V::Null, |support| string(support.status.text())));
                if support.is_some() {
                    let (waited, reasons) = waits.care.get(request_id).ok_or_else(|| Error::Runtime("a care request without its wait".to_owned()))?;
                    projected.insert(text("waited_minutes"), V::Int(*waited));
                    projected.insert(text("wait_reason_codes"), V::List(reasons.iter().cloned().map(V::Str).collect()));
                }
                care_requests.push(V::Map(projected));
            }
        }
        let beliefs = match information.as_ref().and_then(|information| information.beliefs.get(*id)) {
            Some(beliefs) => {
                let mut kept = Map::new();
                for (key, belief) in beliefs.iter().take(BELIEF_LIMIT) {
                    kept.insert(key.clone(), field(belief)?);
                }
                V::Map(kept)
            }
            None => V::Map(Map::new()),
        };
        let mut needs = Map::new();
        needs.insert(text("toilet_urgency"), V::Float(agent.needs.toilet_urgency));
        needs.insert(text("hunger"), V::Float(agent.needs.hunger));
        needs.insert(text("fatigue"), V::Float(agent.needs.fatigue));
        needs.insert(text("hydration_need"), V::Float(agent.needs.hydration_need));
        let dialogue_status = if dialogue.is_empty() { "no_retained_events" } else { "structured_events_only" };
        let mut person = Map::new();
        let mut put = |name: &str, value: V| {
            person.insert(text(name), value);
        };
        put("id", V::Str((*id).clone()));
        put("group_id", optional(&agent.profile.group_id));
        put("zone_id", V::Str(agent.zone_id.clone()));
        put("family_separated", V::Bool(separated));
        put("activity", string(agent.activity.text()));
        put("target_id", optional(&agent.target_id));
        put("stress", V::Float(agent.stress));
        put("trust", V::Float(agent.trust));
        put("needs", V::Map(needs));
        put("mobility", string(agent.profile.mobility.text()));
        put("support_need", V::Float(agent.profile.support_need));
        put("traits", match &traits {
            V::Map(traits) => traits.get(*id).cloned().unwrap_or(V::Null),
            _ => return Err(Error::unsupported("the traits are not a dict")),
        });
        put("context", context_people.get(*id).cloned().unwrap_or(V::Null));
        put("care_requests", V::List(care_requests));
        put("care_history", V::List(care_histories.get(*id).cloned().unwrap_or_default()));
        put("decision", match agent_dynamics.as_ref().and_then(|row| row.agents.get(*id)) {
            Some(memory) => field(memory)?,
            None => V::Null,
        });
        put("beliefs", beliefs);
        put("dialogue", V::List(dialogue));
        put("dialogue_status", string(dialogue_status));
        people.push(person);
    }

    let mut kpis = kpis(&waits.sample, minute);
    if !reads.holding.is_absent(LEDGER).map_err(Error::unsupported)? {
        let balances = crate::rules::resources::metric_values(world)?;
        for (names, values) in RESOURCE_COLUMNS.iter().zip(&balances) {
            for (name, value) in names.iter().zip(values) {
                kpis.insert(text(name), V::Int(*value));
            }
        }
    }
    let pending: Vec<V> = match part(dynamics.as_ref(), "pending_arrivals") {
        Some(V::List(pending)) => pending,
        Some(_) => return Err(Error::unsupported("the pending arrivals are not a list")),
        None => Vec::new(),
    };
    let incoming = incoming_arrivals(reads, geometry.as_ref(), &pending, minute, duration)?;
    let capacity: i64 = rows
        .zones
        .iter()
        .filter(|(id, _)| &***id == "sleeping" || id.starts_with("sleeping_"))
        .map(|(_, zone)| zone.capacity.unwrap_or(0))
        .sum();
    let population = rows.agents.len();
    kpis.insert(text("population"), int(population));
    kpis.insert(text("pending_arrivals"), int(pending.len()));
    kpis.insert(text("sleeping_capacity"), V::Int(capacity));
    kpis.insert(text("occupancy_ratio"), if capacity != 0 { V::Float(population as f64 / capacity as f64) } else { V::Null });

    let events = recent_events(records)?;

    let zone_polygons: BTreeMap<Arc<str>, Vec<Pt>> = match &geometry {
        Some(geometry) => {
            let mut polygons = BTreeMap::new();
            for zone in as_list(item(geometry, "zones")?)? {
                let V::Str(zone_id) = item(zone, "zone_id")? else {
                    return Err(Error::unsupported("a zone's ID is not a str"));
                };
                polygons.insert(zone_id.clone(), points_of(item(zone, "polygon")?)?);
            }
            polygons
        }
        None => BTreeMap::new(),
    };
    let queues = typed::<QueuesRow>(world, QUEUES)?;
    let mut facility_ids: Vec<&Arc<str>> = rows.facilities.iter().map(|(id, _)| id).collect();
    facility_ids.sort_by(|first, second| first.as_bytes().cmp(second.as_bytes()));
    let mut facilities = Vec::with_capacity(facility_ids.len());
    for id in &facility_ids {
        let facility = rows.facilities.get(id).ok_or_else(|| Error::Runtime("a facility left the table".to_owned()))?;
        let mut at = match &geometry {
            Some(geometry) => match get(item(geometry, "facility_points")?, id) {
                Some(found) => Some(point_of(found)?),
                None => None,
            },
            None => None,
        };
        if let Some(found) = at {
            let within = match zone_polygons.get(&facility.zone_id) {
                Some(polygon) => inside(found, polygon)?,
                None => false,
            };
            if !within {
                at = None;
            }
        }
        let mut service_queues = Vec::new();
        if let Some(geometry) = &geometry {
            for service_point in service_points(geometry)? {
                if !matches!(get(service_point, "facility_id"), Some(V::Str(facility_id)) if facility_id == *id) {
                    continue;
                }
                let V::Str(queue_id) = item(service_point, "queue_id")? else {
                    return Err(Error::unsupported("a queue ID is not a str"));
                };
                let queue = queues.as_ref().and_then(|queues| queues.queues.get(queue_id));
                let agents_of = |requests: &[Arc<str>]| -> V {
                    V::List(
                        requests
                            .iter()
                            .filter_map(|request| queues.as_ref().and_then(|queues| queues.requests.get(request)))
                            .map(|request| V::Str(request.agent_id.clone()))
                            .collect(),
                    )
                };
                let ids_of = |requests: &[Arc<str>]| V::List(requests.iter().cloned().map(V::Str).collect());
                let queued: &[Arc<str>] = queue.map_or(&[], |queue| &queue.queued_request_ids);
                let active: &[Arc<str>] = queue.map_or(&[], |queue| &queue.active_request_ids);
                let access = item(service_point, "access")?;
                let mut found = Map::new();
                found.insert(text("service_point_id"), item(service_point, "id")?.clone());
                found.insert(text("queue_id"), V::Str(queue_id.clone()));
                found.insert(text("point"), point_value(point_of(item(service_point, "point_m")?)?));
                found.insert(text("queue_agent_ids"), agents_of(queued));
                found.insert(text("active_agent_ids"), agents_of(active));
                found.insert(text("queued_request_ids"), ids_of(queued));
                found.insert(text("active_request_ids"), ids_of(active));
                found.insert(text("service_capacity_concurrent"), item(service_point, "service_capacity")?.clone());
                found.insert(text("accessibility"), item(access, "accessibility")?.clone());
                found.insert(text("required_access_tags"), item(access, "required_access_tags")?.clone());
                found.insert(text("required_staff_skills"), item(access, "required_staff_skills")?.clone());
                found.insert(text("last_stall"), match queue.and_then(|queue| queue.last_stall.as_ref()) {
                    Some(stall) => field(stall)?,
                    None => V::Null,
                });
                service_queues.push(V::Map(found));
            }
        }
        let ids_of = |ids: &[Arc<str>]| V::List(ids.iter().cloned().map(V::Str).collect());
        let mut found = Map::new();
        found.insert(text("id"), V::Str(facility.id.clone()));
        found.insert(text("kind"), V::Str(facility.kind.clone()));
        found.insert(text("zone_id"), V::Str(facility.zone_id.clone()));
        found.insert(text("capacity"), V::Int(facility.capacity));
        found.insert(text("working_units"), V::Int(facility.working_units));
        found.insert(text("point"), at.map_or(V::Null, point_value));
        found.insert(text("queue_agent_ids"), ids_of(&facility.queue_agent_ids));
        found.insert(text("active_agent_ids"), ids_of(&facility.active_agent_ids));
        found.insert(text("resource_queue_agent_ids"), ids_of(&facility.resource_queue_agent_ids));
        found.insert(text("resource_active_agent_ids"), ids_of(&facility.resource_active_agent_ids));
        found.insert(text("service_queues"), V::List(service_queues));
        facilities.push(V::Map(found));
    }

    let social_network = typed::<SocialNetworkRow>(world, SOCIAL)?;
    let staff_operations = typed::<StaffOperationsRow>(world, STAFF_OPERATIONS)?;
    let mut staff_ids: Vec<&Arc<str>> = rows.staff.iter().map(|(id, _)| id).collect();
    staff_ids.sort_by(|first, second| first.as_bytes().cmp(second.as_bytes()));
    let mut staff = Vec::with_capacity(staff_ids.len());
    for id in &staff_ids {
        let member = rows.staff.get(id).ok_or_else(|| Error::Runtime("a staff member left the table".to_owned()))?;
        let mut found = Map::new();
        found.insert(text("id"), V::Str(member.id.clone()));
        found.insert(text("zone_id"), V::Str(member.zone_id.clone()));
        found.insert(text("skills"), V::List(member.skills.0.iter().cloned().map(V::Str).collect()));
        found.insert(text("active_task_id"), optional(&member.active_task_id));
        found.insert(text("position"), match movement.as_ref().and_then(|movement| movement.staff_positions.get(*id)) {
            Some(position) => field(position)?,
            None => V::Null,
        });
        found.insert(text("dynamics"), match staff_dynamics.as_ref().and_then(|row| row.staff.get(*id)) {
            Some(record) => field(record)?,
            None => V::Null,
        });
        found.insert(text("operation"), match staff_operations.as_ref().and_then(|row| row.staff.get(*id)) {
            Some(record) => field(record)?,
            None => V::Null,
        });
        staff.push(found);
    }

    let mut edges = Vec::new();
    let mut total_edges = 0;
    if let Some(network) = &social_network {
        let relationships = &network.relationships.map;
        total_edges = relationships.len();
        let mut keys: Vec<&Arc<str>> = relationships.keys().collect();
        keys.sort_by(|first, second| first.as_bytes().cmp(second.as_bytes()));
        for key in keys.into_iter().take(EDGE_LIMIT) {
            let V::Map(mut edge) = field(&relationships[key])? else {
                return Err(Error::Runtime("a relationship is not a dict".to_owned()));
            };
            edge.insert(text("pair"), json_value(key.as_bytes())?);
            edges.push(V::Map(edge));
        }
    }

    let mut movement_view = Map::new();
    {
        let mut usage = Map::new();
        let mut journeys = Vec::new();
        let mut journey_count = 0;
        if let Some(movement) = &movement {
            for (edge, minutes) in &movement.edge_usage_minutes {
                usage.insert(edge.clone(), V::Int(*minutes));
            }
            journey_count = movement.journeys.len();
            let mut keys: Vec<&Arc<str>> = movement.journeys.keys().collect();
            keys.sort_by(|first, second| first.as_bytes().cmp(second.as_bytes()));
            for key in keys {
                let V::Map(journey) = field(&movement.journeys[key])? else {
                    return Err(Error::Runtime("a journey is not a dict".to_owned()));
                };
                let mut found = Map::new();
                found.insert(text("agent_id"), V::Str(key.clone()));
                found.extend(journey);
                journeys.push(V::Map(found));
            }
        }
        movement_view.insert(text("edge_usage_agent_minutes"), V::Map(usage));
        movement_view.insert(text("journey_count"), int(journey_count));
        movement_view.insert(text("active_journeys"), V::List(journeys));
    }

    // The zones in the order of the world's dict, with their conditions.
    let environment = state(world, zone_environment::STATE)?;
    let zone_inputs = ZoneInputs::read(configs, environment.as_ref())?;
    let mut zones = Vec::with_capacity(rows.zones.len());
    for (id, zone) in rows.zones.iter() {
        let found = zone_inputs.conditions(id)?;
        let mut conditions = Map::new();
        conditions.insert(text("ventilation_effectiveness"), V::Float(found.ventilation_effectiveness));
        conditions.insert(text("lighting_level"), V::Float(found.lighting_level));
        conditions.insert(text("privacy_level"), V::Float(found.privacy_level));
        conditions.insert(text("temperature_c"), V::Float(found.temperature_c));
        conditions.insert(text("condition_modifier_event_ids"), V::List(found.condition_modifier_event_ids.into_iter().map(V::Str).collect()));
        conditions.insert(text("condition_assumption_ids"), V::List(found.condition_assumption_ids.into_iter().map(V::Str).collect()));
        let mut entry = Map::new();
        entry.insert(text("id"), V::Str(zone.id.clone()));
        entry.insert(text("name"), V::Str(zone.name.clone()));
        entry.insert(text("capacity"), zone.capacity.map_or(V::Null, V::Int));
        entry.insert(text("conditions"), V::Map(conditions));
        zones.push(V::Map(entry));
    }

    // What the namespaces of the body and of the staff's operations keep.
    let body = typed::<BodyRow>(world, BODY)?;
    for person in people.iter_mut() {
        let Some(V::Str(id)) = person.get("id").cloned() else { continue };
        let found = match body.as_ref().and_then(|body| body.agents.get(&id)) {
            Some(record) => field(record)?,
            None => V::Null,
        };
        person.insert(text("body"), found);
    }
    let presence_view: Map = match &presence {
        Some(V::Map(presence)) => presence.iter().filter(|(key, _)| &***key != "history").map(|(key, value)| (key.clone(), value.clone())).collect(),
        Some(_) => return Err(Error::unsupported("the presence state is not a dict")),
        None => Map::new(),
    };
    let sanitation = match &staff_operations {
        Some(row) => field(&row.zones)?,
        None => V::Map(Map::new()),
    };
    let (body_count, body_recent) = recent_items(body.as_ref().map(|body| &body.history as &dyn crate::row::Items))?;
    let (operations_count, operations_recent) = match &staff_operations {
        Some(row) => {
            let count = row.history.len();
            let mut recent = Vec::with_capacity(count.min(RECENT_HISTORY));
            for finished in &row.history[count.saturating_sub(RECENT_HISTORY)..] {
                recent.push(field(finished)?);
            }
            (count, recent)
        }
        None => (0, Vec::new()),
    };
    let shared_count = shared.as_ref().map_or(0, |shared| shared.history.items.len());

    let occupancy_zones = occupancy_zones(world, reads, configs)?;
    let mut occupancy = Map::new();
    occupancy.insert(text("schema_version"), V::Int(2));
    occupancy.insert(text("minute"), V::Int(minute));
    occupancy.insert(text("time_semantics"), string("before_tick"));
    occupancy.insert(text("admitted_count"), int(population));
    occupancy.insert(text("waiting_outside_count"), int(pending.len()));
    occupancy.insert(text("zones"), V::List(occupancy_zones.clone()));

    let demand = operational_demand(world, reads, configs, minute, population, pending.len(), occupancy_zones, shared.as_deref())?;
    let spatial = effective_spatial(
        reads.spatial.document.as_deref().ok_or_else(|| Error::unsupported("the host kept no spatial constraints' document"))?,
        overlay.as_ref(),
        configs,
    )?;
    let no_positions = IdMap::default();
    let geometry_overlay = overlay_view(world, overlay.as_ref(), movement.as_ref().map_or(&no_positions, |movement| &movement.positions))?;

    // The projections of the declared namespaces (`entity_projections`): only the presence ledger
    // has one.
    let presence_declared = configs
        .get("state_contracts")
        .and_then(|declaration| get(declaration, "namespaces"))
        .is_some_and(|namespaces| get(namespaces, PRESENCE).is_some());
    let presence_people = get(presence.as_ref().unwrap_or(&V::Null), "people").cloned();
    for person in people.iter_mut() {
        let Some(V::Str(id)) = person.get("id").cloned() else { continue };
        let context = context_people.get(&id).cloned();
        if let Some(context) = &context {
            person.insert(text("context"), context.clone());
        }
        let group = person.get("group_id").cloned().unwrap_or(V::Null);
        let mut household = match person.get("context") {
            Some(V::Map(context)) => context.get("household_id").cloned().unwrap_or(group),
            _ => group,
        };
        if let Some(record) = presence_people.as_ref().and_then(|people| get(people, &id)) {
            household = item(record, "household_id")?.clone();
        }
        person.insert(text("household_id"), household);
        if presence_declared {
            let state = presence.as_ref().ok_or_else(|| Error::unsupported("a declared namespace the world does not have"))?;
            let row = get(item(state, "people")?, &id).cloned().unwrap_or(V::Map(Map::new()));
            let mut projected = Map::new();
            for key in ["status", "household_id", "transition_zone_id", "expected_return_minute"] {
                projected.insert(text(key), get(&row, key).cloned().unwrap_or(V::Null));
            }
            let mut extensions = Map::new();
            extensions.insert(text(PRESENCE), V::Map(projected));
            person.insert(text("extensions"), V::Map(extensions));
        }
    }

    let mut frame = Map::new();
    let mut put = |name: &str, value: V| {
        frame.insert(text(name), value);
    };
    put("schema_version", V::Int(1));
    put("run_id", string(request.run_id));
    put("branch_id", string(request.branch_id));
    put("minute", V::Int(minute));
    put("computed_until", V::Int(request.computed_until));
    put("state_hash", V::Str(state_hash));
    put("exact", V::Bool(true));
    put("agents", V::List(agents));
    if !incoming.is_empty() {
        put("incoming_arrivals", V::List(incoming));
    }
    put("kpis", V::Map(kpis));
    put("events", V::List(events));
    put("geometry", geometry.unwrap_or(V::Null));
    put("people", V::List(people.into_iter().map(V::Map).collect()));
    put("facilities", V::List(facilities));
    put("staff", V::List(staff.into_iter().map(V::Map).collect()));
    put("zones", V::List(zones));
    let mut social = Map::new();
    social.insert(text("edges"), V::List(edges));
    social.insert(text("total_edges"), int(total_edges));
    social.insert(text("truncated"), V::Bool(total_edges > EDGE_LIMIT));
    social.insert(text("interactions"), V::List(social_events));
    put("social", V::Map(social));
    put("movement", V::Map(movement_view));
    put("occupancy", V::Map(occupancy));
    put("operational_demand", demand);
    put("spatial_constraints", spatial);
    put("presence", V::Map(presence_view));
    put("geometry_overlay", geometry_overlay);
    put("sanitation", sanitation);
    let mut recorded = Map::new();
    recorded.insert(
        text("decisions"),
        V::Map(Map::from([
            (text("source"), string("people[].decision")),
            (text("retention"), string("latest selected action per active agent")),
        ])),
    );
    recorded.insert(
        text("social"),
        V::Map(Map::from([
            (text("retained_records"), int(body_count)),
            (text("event_window_truncated"), V::Bool(social_truncated)),
            (text("frame_event_scan_limit"), int(SOCIAL_SCAN_LIMIT)),
            (text("retention_limit"), V::Int(20000)),
            (text("recent_effects"), V::List(body_recent)),
        ])),
    );
    recorded.insert(
        text("staff"),
        V::Map(Map::from([
            (text("retained_records"), int(operations_count)),
            (text("retention_limit"), V::Int(20000)),
            (text("recent_operations"), V::List(operations_recent)),
        ])),
    );
    recorded.insert(text("status"), string("bounded retained evidence; absence does not prove no earlier event"));
    recorded.insert(
        text("shared_operations"),
        V::Map(Map::from([
            (text("contract_enabled"), V::Bool(reads.holding.contains(SHARED))),
            (text("retained_records"), int(shared_count)),
            (text("retention_limit"), V::Int(20000)),
            (text("read_route"), string("branch/history?kind=care or kind=operations")),
            (
                text("claim_boundary"),
                string("only event-linked applied transitions are indexed; this does not imply an unrecorded outcome"),
            ),
        ])),
    );
    put("recorded_history", V::Map(recorded));
    put("pending_arrivals", int(pending.len()));
    put("kpi_time_semantics", string("state at boundary before this tick"));
    put("position_limitations", V::List(POSITION_LIMITATIONS.iter().map(|line| string(line)).collect()));
    Ok(frame)
}

/// The count of a history list and its newest items.
fn recent_items(items: Option<&dyn crate::row::Items>) -> Result<(usize, Vec<V>)> {
    let Some(items) = items else {
        return Ok((0, Vec::new()));
    };
    let count = items.count();
    let mut recent = Vec::with_capacity(count.min(RECENT_HISTORY));
    let mut scratch = Encoder::with_capacity(512);
    for index in count.saturating_sub(RECENT_HISTORY)..count {
        scratch.out.clear();
        recent.push(json_value(items.item_json(index, &mut scratch)?)?);
    }
    Ok((count, recent))
}

/// `population_context_people(world)`: the Scenario's context of each person, then what was saved
/// for the people who arrived since.
fn population_context(world: &World, configs: &Map, dynamics: Option<&V>) -> Result<Map> {
    let mut merged = match configs.get("population_context") {
        Some(V::Map(configured)) => match configured.get("people") {
            Some(V::Map(people)) => people.clone(),
            _ => Map::new(),
        },
        _ => Map::new(),
    };
    if let Some(V::Map(saved)) = state(world, CONTEXT)? {
        if let Some(V::Map(people)) = saved.get("people") {
            merged.extend(people.iter().map(|(key, value)| (key.clone(), value.clone())));
        }
    }
    if let Some(V::Map(people)) = part(dynamics, "profile_context") {
        merged.extend(people);
    }
    Ok(merged)
}

/// `world.event_log.recent(100)`, as the frame lists them.
fn recent_events(records: &Records) -> Result<Vec<V>> {
    let mut events = Vec::with_capacity(records.wanted.recent.len());
    for index in records.wanted.recent.clone() {
        let record = records.at(index)?;
        let event = &record.event;
        let mut found = Map::new();
        found.insert(text("event_id"), V::Str(event.event_id.clone()));
        found.insert(text("minute"), V::Int(record.realized_minute));
        found.insert(text("event_type"), V::Str(event.event_type.clone()));
        found.insert(text("kind"), string(event.kind.as_str()));
        found.insert(text("status"), string(record.status.as_str()));
        found.insert(text("source_id"), optional(&event.source_id));
        found.insert(text("causation_id"), optional(&event.causation_id));
        events.push(V::Map(found));
    }
    Ok(events)
}

/// The people walking in on an approach route, where they are on it.
fn incoming_arrivals(reads: &Reads, geometry: Option<&V>, pending: &[V], minute: i64, duration: i64) -> Result<Vec<V>> {
    let Some(geometry) = geometry else {
        // Without a floor plan there are no approach routes: nobody is shown.
        return Ok(Vec::new());
    };
    let documents = match get(geometry, "arrival_approach_routes") {
        Some(V::List(routes)) => routes.as_slice(),
        _ => &[],
    };
    if documents.is_empty() {
        return Ok(Vec::new());
    }
    let parsed = match &reads.geometry {
        Some(found) => found.approach_routes.as_ref().map_err(|fault| Error::unsupported(fault.text.to_string()))?.clone(),
        None => return Ok(Vec::new()),
    };
    if parsed.len() != documents.len() {
        return Err(Error::Runtime("the approach routes and their documents differ".to_owned()));
    }
    // `{route.route_id: route}`: a later route of the same ID wins.
    let mut by_id: BTreeMap<Arc<str>, usize> = BTreeMap::new();
    for (index, route) in parsed.iter().enumerate() {
        by_id.insert(route.route_id.clone(), index);
    }
    let mut incoming = Vec::new();
    for arrival in pending {
        let Some(V::Str(route_id)) = get(arrival, "approach_route_id") else {
            continue;
        };
        let Some(&index) = by_id.get(route_id) else {
            continue;
        };
        let route = &parsed[index];
        let points = points_of(item(&documents[index], "points_m")?)?;
        let started = as_int(item(arrival, "approach_started_minute")?)?;
        let elapsed = (minute - started).max(0);
        let mut remaining = py_min(route.distance, elapsed as f64 * route.walking_speed);
        let progress = remaining / route.distance;
        let mut position = *points.last().ok_or_else(|| Error::unsupported("a route without points"))?;
        for index in 0..points.len().saturating_sub(1) {
            let (left, right) = (points[index], points[index + 1]);
            let segment = hypot(&[right.x - left.x, right.y - left.y]);
            if remaining <= segment || index == points.len() - 2 {
                let fraction = if segment <= 0.0 { 1.0 } else { py_min(1.0, remaining / segment) };
                position = Pt { x: left.x + (right.x - left.x) * fraction, y: left.y + (right.y - left.y) * fraction };
                break;
            }
            remaining -= segment;
        }
        let due = as_int(item(arrival, "approach_due_minute")?)?;
        let completed = !matches!(get(arrival, "approach_completed_minute"), None | Some(V::Null));
        let status = if completed {
            "awaiting_admission"
        } else if minute >= duration && due >= duration {
            "censored"
        } else {
            "approaching"
        };
        let profile = item(arrival, "profile")?;
        let mut found = Map::new();
        found.insert(text("agent_id"), item(profile, "id")?.clone());
        found.insert(text("route_id"), V::Str(route.route_id.clone()));
        found.insert(text("destination_zone_id"), V::Str(route.destination_zone_id.clone()));
        found.insert(text("position"), point_value(position));
        found.insert(text("progress"), V::Float(progress));
        found.insert(text("status"), string(status));
        found.insert(text("arrival_minute"), V::Int(due));
        incoming.push(V::Map(found));
    }
    Ok(incoming)
}

/// `operational_demand_snapshot(world)`, as its contract dumps it.
#[allow(clippy::too_many_arguments)]
fn operational_demand(
    world: &World,
    reads: &Reads,
    configs: &Map,
    minute: i64,
    population: usize,
    pending: usize,
    zones: Vec<V>,
    shared: Option<&SharedOperationsRow>,
) -> Result<V> {
    // The Scenario's floor plan, without the overlay's moves, where the configs hold a truthy one.
    let raw_geometry = configs.get("facility_geometry").is_some_and(V::is_truthy);
    let geometry = match (&reads.geometry, raw_geometry) {
        (Some(geometry), true) => Some(geometry.document.as_ref().map_err(|fault| Error::unsupported(fault.text.to_string()))?.clone()),
        _ => None,
    };
    let queues = typed::<QueuesRow>(world, QUEUES)?;
    let rows = world.rows()?;
    let mut facility_ids: Vec<&Arc<str>> = rows.facilities.iter().map(|(id, _)| id).collect();
    facility_ids.sort_by(|first, second| first.as_bytes().cmp(second.as_bytes()));
    let mut services = Vec::new();
    for id in facility_ids {
        let facility = rows.facilities.get(id).ok_or_else(|| Error::Runtime("a facility left the table".to_owned()))?;
        let mut points: Vec<V> = match &geometry {
            Some(geometry) => service_points(geometry)?
                .iter()
                .filter(|point| matches!(get(point, "facility_id"), Some(V::Str(facility_id)) if facility_id == id))
                .cloned()
                .collect(),
            None => Vec::new(),
        };
        if points.is_empty() {
            if let Some(geometry) = &geometry {
                if let Some(at) = get(item(geometry, "facility_points")?, id) {
                    points.push(legacy_service_point(id, facility.working_units, at)?);
                }
            }
        }
        for service_point in points {
            let V::Str(queue_id) = item(&service_point, "queue_id")? else {
                return Err(Error::unsupported("a queue ID is not a str"));
            };
            let queue = queues.as_ref().and_then(|queues| queues.queues.get(queue_id));
            let bound = **queue_id == **id && queue.is_none();
            let ids_of = |ids: &[Arc<str>]| V::List(ids.iter().cloned().map(V::Str).collect());
            let access = item(&service_point, "access")?;
            let mut found = Map::new();
            found.insert(text("service_point_id"), item(&service_point, "id")?.clone());
            found.insert(text("facility_id"), V::Str(facility.id.clone()));
            found.insert(text("zone_id"), V::Str(facility.zone_id.clone()));
            found.insert(text("point_m"), point_value(point_of(item(&service_point, "point_m")?)?));
            found.insert(text("queue_id"), V::Str(queue_id.clone()));
            let (length, active, queued_ids, active_ids, stall, binding) = match queue {
                Some(queue) => (
                    int(queue.queued_request_ids.len()),
                    int(queue.active_request_ids.len()),
                    ids_of(&queue.queued_request_ids),
                    ids_of(&queue.active_request_ids),
                    match &queue.last_stall {
                        Some(stall) => field(stall)?,
                        None => V::Null,
                    },
                    "independent_queue_state",
                ),
                None if bound => (
                    int(facility.queue_agent_ids.len() + facility.resource_queue_agent_ids.len()),
                    int(facility.active_agent_ids.len() + facility.resource_active_agent_ids.len()),
                    V::Null,
                    V::Null,
                    V::Null,
                    "legacy_facility_fifo",
                ),
                None => (V::Null, V::Null, V::Null, V::Null, V::Null, "not_connected"),
            };
            found.insert(text("queue_length"), length);
            found.insert(text("active_services"), active);
            found.insert(text("queued_request_ids"), queued_ids);
            found.insert(text("active_request_ids"), active_ids);
            found.insert(text("last_stall"), stall);
            found.insert(text("service_capacity_concurrent"), item(&service_point, "service_capacity")?.clone());
            found.insert(text("reservation_binding"), string(binding));
            found.insert(text("accessibility"), item(access, "accessibility")?.clone());
            found.insert(text("required_access_tags"), item(access, "required_access_tags")?.clone());
            found.insert(text("required_staff_skills"), item(access, "required_staff_skills")?.clone());
            services.push(V::Map(found));
        }
    }
    let mut request_counts: BTreeMap<&str, i64> = BTreeMap::new();
    if let Some(shared) = shared {
        for head in shared.support_requests.map.values() {
            *request_counts.entry(head.status.text()).or_default() += 1;
        }
    }
    let assigned = rows.staff.iter().filter(|(_, member)| member.active_task_id.is_some()).count();
    let mut staff = Map::new();
    staff.insert(text("count"), int(rows.staff.len()));
    staff.insert(text("assigned"), int(assigned));
    drop(rows);
    let resources: Map = world.resources()?.iter().map(|(key, value)| (key.clone(), V::Int(*value))).collect();
    let mut snapshot = Map::new();
    let mut put = |name: &str, value: V| {
        snapshot.insert(text(name), value);
    };
    put("schema_version", V::Int(1));
    put("minute", V::Int(minute));
    put("time_semantics", string("before_tick"));
    put("coverage", string("observed_state_only"));
    put("population_count", int(population));
    put("pending_arrivals", int(pending));
    put("zones", V::List(zones));
    put("services", V::List(services));
    put("staff", V::Map(staff));
    put("resources", V::Map(resources));
    put("support_requests", V::Map(request_counts.into_iter().map(|(status, count)| (text(status), V::Int(count))).collect()));
    put(
        "unestimated",
        V::List(["future_resource_demand", "required_bed_count", "unmet_service_need", "required_staff_coverage"].iter().map(|name| string(name)).collect()),
    );
    Ok(V::Map(snapshot))
}

/// `geometry_overlay_v1` without its history, each temporary room with who is in it, its beds and
/// its admissions.
fn overlay_view(world: &World, overlay: Option<&V>, positions: &IdMap<Position>) -> Result<V> {
    let mut view: Map = match overlay {
        Some(V::Map(overlay)) => overlay.iter().filter(|(key, _)| &***key != "history").map(|(key, value)| (key.clone(), value.clone())).collect(),
        Some(_) => return Err(Error::unsupported("the overlay is not a dict")),
        None => Map::new(),
    };
    let reservations = view.get("bed_reservations").cloned().unwrap_or(V::Map(Map::new()));
    let admissions = view.get("room_admissions").cloned().unwrap_or(V::Map(Map::new()));
    let Some(V::Map(rooms)) = view.get_mut("temporary_rooms") else {
        return Ok(V::Map(view));
    };
    let rows = world.rows()?;
    for room in rooms.values_mut() {
        let room_id = item(room, "room_id")?.clone();
        let zone_id = item(room, "zone_id")?.clone();
        let polygon = points_of(item(room, "polygon_m")?)?;
        let mut occupants: Vec<Arc<str>> = Vec::new();
        for (agent_id, agent) in rows.agents.iter() {
            if !matches!(&zone_id, V::Str(zone) if *zone == agent.zone_id) {
                continue;
            }
            let Some(at) = positions.get(agent_id).and_then(|position| position.point) else {
                continue;
            };
            if inside(Pt { x: at.x, y: at.y }, &polygon)? {
                occupants.push(agent_id.clone());
            }
        }
        occupants.sort_by(|first, second| first.as_bytes().cmp(second.as_bytes()));
        let mut occupied = Vec::new();
        let mut claimed: BTreeSet<Arc<str>> = BTreeSet::new();
        for (index, bed) in as_list(item(room, "bed_points_m")?)?.iter().enumerate() {
            let bed = point_of(bed)?;
            let mut nearby: Vec<&Arc<str>> = Vec::new();
            for agent_id in &occupants {
                let Some(at) = positions.get(agent_id).and_then(|position| position.point) else {
                    return Err(Error::unsupported("an occupant without a point"));
                };
                let (dx, dy) = (at.x - bed.x, at.y - bed.y);
                if dx * dx + dy * dy <= BED_DISTANCE_SQUARED && !claimed.contains(agent_id) {
                    nearby.push(agent_id);
                }
            }
            if let Some(agent_id) = nearby.into_iter().min_by(|first, second| first.as_bytes().cmp(second.as_bytes())) {
                claimed.insert(agent_id.clone());
                occupied.push(V::Map(Map::from([(text("bed_index"), int(index)), (text("agent_id"), V::Str(agent_id.clone()))])));
            }
        }
        let mut reserved: Vec<(&V, &V, &V)> = Vec::new();
        for reservation in as_map(&reservations)?.values() {
            reserved.push((item(reservation, "room_id")?, item(reservation, "bed_index")?, item(reservation, "agent_id")?));
        }
        reserved.sort_by(|first, second| order(first.0, second.0).then(order(first.1, second.1)).then(order(first.2, second.2)));
        let reserved: Vec<V> = reserved
            .into_iter()
            .filter(|(room, _, _)| **room == room_id)
            .map(|(_, bed, agent)| V::Map(Map::from([(text("bed_index"), bed.clone()), (text("agent_id"), agent.clone())])))
            .collect();
        let mut admitted: Vec<V> = Vec::new();
        for admission in as_map(&admissions)?.values() {
            if *item(admission, "room_id")? == room_id {
                admitted.push(item(admission, "agent_id")?.clone());
            }
        }
        admitted.sort_by(order);
        let V::Map(room) = room else {
            return Err(Error::unsupported("a room is not a dict"));
        };
        room.insert(text("observed_occupant_agent_ids"), V::List(occupants.into_iter().map(V::Str).collect()));
        room.insert(text("observed_occupied_beds"), V::List(occupied));
        room.insert(text("reserved_beds"), V::List(reserved));
        room.insert(text("admitted_agent_ids"), V::List(admitted));
        room.insert(text("occupancy_basis"), string(OCCUPANCY_BASIS));
    }
    Ok(V::Map(view))
}

/// Python's `<` of two IDs or two ints, the keys a room's beds sort by.
fn order(first: &V, second: &V) -> std::cmp::Ordering {
    match (first, second) {
        (V::Str(first), V::Str(second)) => first.as_bytes().cmp(second.as_bytes()),
        (V::Int(first), V::Int(second)) => first.cmp(second),
        _ => std::cmp::Ordering::Equal,
    }
}
