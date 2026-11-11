//! `_apply_effect` (`experiments/phases/propose.py`): what a decision does to the world before it is
//! recorded (Issue #152, lane P8 e1, the join of the four P8 lanes).
//!
//! One call plans with nothing changed and then writes. The step has no Python body any more
//! (segments.py, KERNEL_ONLY_SEGMENTS): what it doesn't take is refused before any write and becomes
//! KernelRefusal, anything that isn't plainly what the Python step accepted
//! (docs/rust-port/lanes/m4-service.md).
//!
//! Every rule the step reaches is a function of the core: the bed (`overlay`), the choice of a
//! facility and the trip (`travel`), the queues (`toilet`, `service`), the support request and its
//! care receipts (`support`, `care_receipts`), the information model's record of the request, and
//! the social action's event with its conversation context (`social_dialogue`). The PyO3 crate's
//! file of the same name reads what only Python holds (the proposal, the managers, the policies,
//! whether the world holds each namespace, the configs, the floor plan, the Run's router) into
//! `Inputs`, and makes the Python values of what comes back.
//!
//! A person who asks a queue at once (no trip to make, or an independent FIFO at their facility) goes
//! through the queue's own kernel, which plans and writes in one and refuses before its first write:
//! it runs last in the plan, after only the capacity waits the choice of the facility recorded. A
//! person the water or meal queue turns down is recorded with the text the queue gives.
//!
//! `seek_help` and `seek_information` plan and write on one borrow of the world (`seek_write`), then
//! call the LLM's hooks through the `Host` with nothing borrowed (`seek_after`, rule R6): the support
//! priority (`Host::prioritize_support_request`) and the social policy
//! (`Host::consider_support_request`). They may write the world; what the step reads after them (the
//! person's next decision, the metric of a refusal) is read again. A `ValueError` of anything after
//! the request's writes (the receipt of a request closed at once, the information record, the hooks)
//! is the step's `except ValueError`: `support_request_rejected_count` goes up by one.

use std::sync::{Arc, LazyLock};

use indexmap::IndexMap;

use crate::config::movement::Movement;
use crate::config::population::AccessibilityNeeds;
use crate::entities::Activity;
use crate::error::{Error, Result};
use crate::events::writer::{WriteError, WriteResult};
use crate::events::{EventKind, EventRow};
use crate::geometry::{Geo, Pt};
use crate::host::{ErrorClass, Host, HostFailure};
use crate::row::ValueRow;
use crate::rules::care_receipts::{self, source_ids_of, PlannedTransition, Transition};
use crate::rules::facility_queues;
use crate::rules::kit::{self, add_metric, check_metric_state, held_row, Events, Held, Kept};
use crate::rules::overlay::{self, Bed, Beds, PLAN_UNREAD};
use crate::rules::service::{self, Manager, Minute, Pending, Requested};
use crate::rules::social_dialogue::context_value;
use crate::rules::support::{self, support_kind_of, PlannedRequest, RequestService, NAMESPACE_IS_NONE};
use crate::rules::toilet;
use crate::rules::travel::{self, CapacityWait, Graph, PlannedTrip, Route, Routing, Selected, Trip, Walking};
use crate::state::information::{InformationRow, SupportRequestRow};
use crate::state::resource_service::{RequestRecord, ServiceRow, NAME as SERVICE};
use crate::value::V;
use crate::world::{World, WorldCell};

/// `information_spread_v1`.
pub const INFORMATION: &str = "information_spread_v1";
const OVERLAY_KEY: &str = "geometry_overlay_v1";
const REJECTED_METRIC: &str = "resource_service_request_rejected_count";
const SUPPORT_REJECTED_METRIC: &str = "support_request_rejected_count";
const TRAVEL_REJECTED_DETAIL: &str = "travel could not be scheduled within current limits";
/// The schedule's priority of a person's own social action.
const SOCIAL_PRIORITY: i64 = 35;
const SOCIAL_POLICY: &str = "event-triggered-social-v2";
const SOCIAL_SOURCE: &str = "ASM-EVENT-TRIGGERED-SOCIAL-INTERACTIONS-V1";

/// The refusal where the step reads the resource service's manager and reading it raised: the PyO3
/// crate reads it again there and raises what that raises.
pub const MANAGER_UNREAD: &str = "the resource service's manager raised when it was read";
/// The refusal where the step reads the information policy's `config.query_threshold` and reading
/// it raised: the PyO3 crate reads it again there and raises what that raises.
pub const THRESHOLD_UNREAD: &str = "the information policy's query threshold raised when it was read";
/// A `request` set on the manager itself (a test's patch) is not the queue's kernel.
pub const PATCHED: &str = "the manager's request is another function";
/// The refusal of `service_held` of a world whose resource service is None.
const OTHER_SERVICE: &str = "the world holds another service state";

fn refuse<T>(reason: &'static str) -> Result<T> {
    Err(Error::unsupported(reason))
}

/// The PyO3 crate did not read what the action reads: a bug of the caller.
fn missing(what: &str) -> Error {
    Error::Runtime(format!("the inputs of the step hold no {what}"))
}

/// The fixed strings of the event, each shared by all its rows.
struct Names {
    interaction_type: Arc<str>,
    agent_id: Arc<str>,
    participant_agent_id: Arc<str>,
    conversation_state_context: Arc<str>,
    zone_id: Arc<str>,
    topic_id: Arc<str>,
    action: Arc<str>,
    reason_short: Arc<str>,
    interaction_duration_minutes: Arc<str>,
    source_type: Arc<str>,
    source_id: Arc<str>,
    policy_version: Arc<str>,
    event_type: Arc<str>,
    autonomous_need_choice: Arc<str>,
    assumption: Arc<str>,
    social_source: Arc<str>,
    social_policy: Arc<str>,
}

static NAMES: LazyLock<Names> = LazyLock::new(|| Names {
    interaction_type: Arc::from("interaction_type"),
    agent_id: Arc::from("agent_id"),
    participant_agent_id: Arc::from("participant_agent_id"),
    conversation_state_context: Arc::from("conversation_state_context"),
    zone_id: Arc::from("zone_id"),
    topic_id: Arc::from("topic_id"),
    action: Arc::from("action"),
    reason_short: Arc::from("reason_short"),
    interaction_duration_minutes: Arc::from("interaction_duration_minutes"),
    source_type: Arc::from("source_type"),
    source_id: Arc::from("source_id"),
    policy_version: Arc::from("policy_version"),
    event_type: Arc::from("social_interaction"),
    autonomous_need_choice: Arc::from("autonomous_need_choice"),
    assumption: Arc::from("assumption"),
    social_source: Arc::from(SOCIAL_SOURCE),
    social_policy: Arc::from(SOCIAL_POLICY),
});

// ---- The proposal and what the step reads beside the world -----------------------------------------

/// `ActionKind`, the proposal's action.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    Wait,
    Rest,
    Sleep,
    Socialize,
    CareFamily,
    HelpOthers,
    RequestToilet,
    FetchWater,
    CollectMeal,
    SeekHelp,
    SeekInformation,
}

impl Action {
    /// The action of an `ActionKind`'s value; None for one the step does not know.
    pub fn parse(value: &str) -> Option<Self> {
        Some(match value {
            "wait" => Action::Wait,
            "rest" => Action::Rest,
            "sleep" => Action::Sleep,
            "socialize" => Action::Socialize,
            "care_family" => Action::CareFamily,
            "help_others" => Action::HelpOthers,
            "request_toilet" => Action::RequestToilet,
            "fetch_water" => Action::FetchWater,
            "collect_meal" => Action::CollectMeal,
            "seek_help" => Action::SeekHelp,
            "seek_information" => Action::SeekInformation,
            _ => return None,
        })
    }

    /// `seek_help` and `seek_information`, which ask the staff for support.
    pub fn seeks(self) -> bool {
        matches!(self, Action::SeekHelp | Action::SeekInformation)
    }

    /// `socialize`, `care_family` and `help_others`, which schedule a social interaction.
    pub fn is_social(self) -> bool {
        matches!(self, Action::Socialize | Action::CareFamily | Action::HelpOthers)
    }

    /// The facility's kind and the action's name in the service's record, of `fetch_water` and
    /// `collect_meal`.
    pub fn resource(self) -> Option<(&'static str, &'static str)> {
        match self {
            Action::FetchWater => Some(("water_station", "fetch_water")),
            Action::CollectMeal => Some(("meal_distribution", "collect_meal")),
            _ => None,
        }
    }

    /// `request_toilet`, `fetch_water` and `collect_meal`, which choose a facility.
    pub fn trips(self) -> bool {
        self == Action::RequestToilet || self.resource().is_some()
    }
}

/// `interaction_data`: the interaction, its action and its topic.
struct Interaction {
    kind: &'static str,
    action: &'static str,
    topic: &'static str,
}

const SOCIALIZE: Interaction = Interaction { kind: "reassurance_exchange", action: "offer_reassurance", topic: "emotional_check_in" };
const CARE_FAMILY: Interaction = Interaction { kind: "family_care", action: "provide_family_support", topic: "family_care" };
const HELP_OTHERS: Interaction = Interaction { kind: "voluntary_support", action: "offer_practical_help", topic: "mutual_aid" };

/// What the step reads of the proposal.
#[derive(Clone, Debug)]
pub struct Proposal {
    pub action: Action,
    pub agent_id: Arc<str>,
    pub next_decision_at: i64,
    /// `target_id`, of a social action: the participant.
    pub target_id: Option<Arc<str>>,
    /// `observed_state_hash`, of a request where the Run has an LLM router.
    pub observed_state_hash: Option<Arc<str>>,
}

/// What a trip or a walk to a bed reads beside the world.
pub struct Walk<'a> {
    /// Whether the world holds `movement_v1`.
    pub movement: Held,
    /// The movement section of the configs.
    pub section: &'a Kept<Movement>,
    /// The floor plan; None where the PyO3 crate could not read it (`PLAN_UNREAD` where the rule
    /// reads it).
    pub geo: Option<&'a Arc<Geo>>,
}

impl Walk<'_> {
    fn walking(&self) -> Walking<'_> {
        Walking { movement: &self.movement, geo: self.geo }
    }
}

/// What `sleep` reads beside the world.
pub struct Sleep<'a> {
    pub walk: Walk<'a>,
    /// The overlay's schema (`overlay_schema`).
    pub schema: &'a Kept<Option<i64>>,
}

/// The service manager a trip action asks at once.
pub enum Asked<'a> {
    /// `controller.toilet_service`: whether its `__dict__` holds a `request` (a test's patch).
    Toilet { patched: bool },
    /// `controller.resource_service`, which is not None.
    Resource {
        patched: bool,
        /// The manager's `config`; None where reading it raised (`MANAGER_UNREAD`).
        config: Option<&'a IndexMap<Arc<str>, V>>,
        /// Whether the world holds `resource_service_v1` and `resource_ledger_v1`.
        service: Held,
        ledger: Held,
    },
}

/// What `request_toilet`, and `fetch_water` and `collect_meal` with a resource service, read beside
/// the world.
pub struct Trips<'a> {
    pub walk: Walk<'a>,
    /// Whether the world holds `staff_operations_v1`, whose trips load the edges.
    pub staff: Held,
    /// The people's graph of the controller's Run.
    pub graph: &'a Graph,
    /// The section `knowledge_is_limited`.
    pub knows: &'a Kept<bool>,
    /// Whether the world holds `facility_queues_v1`.
    pub queues: Held,
    /// The population's accessibility needs.
    pub needs: &'a Kept<AccessibilityNeeds>,
    pub asked: Asked<'a>,
}

/// `controller.information_policy`, which is not None.
pub struct Information {
    /// `config.query_threshold`; None where reading it raised (`THRESHOLD_UNREAD`).
    pub threshold: Option<f64>,
    /// Whether the world holds `information_spread_v1`.
    pub held: Held,
}

/// What `seek_help` and `seek_information` read beside the world.
pub struct Seek {
    /// What `StaffService.request` reads of `controller.staff_service`.
    pub service: RequestService,
    /// Whether the world holds `shared_operations_v1` (a None there is `NAMESPACE_IS_NONE`).
    pub shared: Held,
    /// None without an information policy.
    pub information: Option<Information>,
}

/// What the step reads beside the world, by the action.
pub struct Inputs<'a> {
    /// Whether the world holds `geometry_overlay_v1`, whose bed a person gives up unless they sleep.
    pub overlay: Held,
    /// Whether `world.metric_state` is its slot's tree (of a resource action and of a request).
    pub metric_state_held: bool,
    /// `body_dynamics.social_interaction_minutes`, of a social action.
    pub social_minutes: Option<i64>,
    pub sleep: Option<Sleep<'a>>,
    /// Of `request_toilet`, and of `fetch_water` and `collect_meal` with a resource service.
    pub trips: Option<Trips<'a>>,
    pub seek: Option<Seek>,
}

/// What the step returns: the target, the next decision minute, and whether the decision
/// committed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Applied {
    pub target: Option<Arc<str>>,
    pub next_decision_at: i64,
    pub committed: bool,
}

// ---- Shared pieces -------------------------------------------------------------------------------

/// The person's zone and profile ID.
fn agent_place(world: &World, agent_id: &str) -> Result<(Arc<str>, Arc<str>)> {
    let rows = world.rows()?;
    let Some(agent) = rows.agents.get(agent_id) else {
        return refuse("no such agent");
    };
    Ok((agent.zone_id.clone(), agent.profile.id.clone()))
}

/// `release_sleep_bed`, not done yet: whether the person holds a reservation to give up.
fn holds_bed(world: &World, overlay: &Held, agent_id: &str) -> Result<bool> {
    let Some(state) = held_row::<ValueRow>(world, OVERLAY_KEY, overlay)? else {
        return Ok(false);
    };
    let V::Map(fields) = &state.0 else {
        return refuse("the overlay is not a plain dict");
    };
    match fields.get("bed_reservations") {
        None => Ok(false),
        Some(V::Map(held)) => Ok(held.contains_key(agent_id)),
        Some(_) => refuse("the bed reservations are not a plain dict"),
    }
}

/// `release_sleep_bed`.
fn release_bed(world: &World, overlay: &Held, agent_id: &str) -> Result<()> {
    overlay::release(world, overlay, "bed_reservations", Some(agent_id)).map(drop)
}

/// The person's next decision, and the activity they take with no target.
fn set_person(world: &World, agent_id: &str, next: i64, still: Option<Activity>) -> Result<()> {
    let mut rows = world.rows_mut()?;
    let Some(row) = rows.agents.get_mut(agent_id) else {
        return refuse("the rows have no such agent");
    };
    row.next_decision_at = next;
    if let Some(activity) = still {
        row.activity = activity;
        row.target_id = None;
    }
    Ok(())
}

/// The floor plan where a rule reads it: `PLAN_UNREAD` where the PyO3 crate could not read it.
fn plan_of(geo: Option<&Arc<Geo>>) -> Result<&Geo> {
    match geo {
        Some(geo) => Ok(geo),
        None => refuse(PLAN_UNREAD),
    }
}

/// A trip planned within the facility's zone, or to it.
#[allow(clippy::too_many_arguments)]
fn plan_facility_trip(
    world: &World,
    events: &Events<'_>,
    walking: Walking<'_>,
    agent_id: &Arc<str>,
    route: Route,
    points: Vec<Pt>,
    purpose: &'static str,
    facility_id: &Arc<str>,
    minute: i64,
) -> Result<Option<PlannedTrip>> {
    travel::plan_travel(
        world,
        events,
        walking,
        Trip {
            agent_id: agent_id.clone(),
            route,
            points,
            purpose,
            facility_id: Some(facility_id.clone()),
            minute,
            causation_id: None,
            support_request_id: None,
            support_staff_id: None,
        },
    )
}

/// The `reservation_id` of each pending `resource_reserve` event, in the queue's order, as the
/// Python rule reads them from `pending()`. The PyO3 crate reads the rows of the events Python
/// scheduled before it calls.
fn pending_reserves(world: &World) -> Result<Vec<Pending>> {
    let queue = world.queue()?;
    let mut found = Vec::new();
    for (_, serial) in queue.ordered() {
        let Some(row) = &queue.slot(*serial).row else {
            return refuse("an event in the queue is not a plain one");
        };
        if &*row.event_type != "resource_reserve" {
            continue;
        }
        found.push(match kit::payload(row, "reservation_id") {
            Some(V::Str(id)) => Pending::Id(id.clone()),
            Some(_) => Pending::Other,
            None => Pending::Missing,
        });
    }
    Ok(found)
}

/// The service's record of a request is kept only where the world holds `resource_service_v1`.
fn check_service(world: &World, service: &Held) -> Result<()> {
    match held_row::<ServiceRow>(world, SERVICE, service)? {
        Some(_) => Ok(()),
        None => refuse(OTHER_SERVICE),
    }
}

// ---- wait, rest, sleep, the social actions and the trips --------------------------------------------

/// What a trip action does, planned.
struct TripEffect {
    /// What `_route` records while the facility is chosen.
    waits: Vec<CapacityWait>,
    trip: Option<PlannedTrip>,
    facility_id: Option<Arc<str>>,
    /// The person is at the facility and asks its queue, with no trip.
    at_once: bool,
    /// The request's record: `(outcome, reason, detail)`.
    outcome: Option<(&'static str, &'static str, Option<Arc<str>>)>,
    rejected: bool,
}

impl TripEffect {
    fn new() -> Self {
        Self { waits: Vec::new(), trip: None, facility_id: None, at_once: false, outcome: None, rejected: false }
    }
}

impl Trips<'_> {
    fn routing(&self) -> Routing<'_> {
        Routing { graph: self.graph, geo: self.walk.geo, movement: self.walk.section, staff: &self.staff }
    }
}

/// `facility_has_independent_queues`; false for a world without `facility_queues_v1`.
fn has_independent(world: &World, queues: &Held, facility_id: &str) -> Result<bool> {
    Ok(facility_queues::queues(world, queues)?.is_some_and(|row| facility_queues::has_independent(&row, facility_id)))
}

/// The `request_toilet` branch, changing nothing.
fn plan_toilet(world: &World, events: &Events<'_>, trips: &Trips<'_>, agent_id: &Arc<str>, zone_id: &Arc<str>, minute: i64) -> Result<TripEffect> {
    let mut effect = TripEffect::new();
    let selected = travel::select(world, &trips.routing(), trips.knows, agent_id, None, &mut effect.waits)?;
    events.reserve(effect.waits.len());
    let Selected::Found(facility_id, route) = selected else {
        return Ok(effect);
    };
    let walking = trips.walk.walking();
    effect.trip = if route.destination == *zone_id && has_independent(world, &trips.queues, &facility_id)? {
        effect.at_once = true;
        None
    } else if route.destination == *zone_id {
        match travel::approach_route(world, walking, trips.walk.section, agent_id, &facility_id)? {
            None => None,
            Some((local, _)) if local.minutes <= 0 => {
                effect.at_once = true;
                None
            }
            Some((local, points)) => plan_facility_trip(world, events, walking, agent_id, local, points, "toilet_request", &facility_id, minute)?,
        }
    } else {
        plan_facility_trip(world, events, walking, agent_id, route, Vec::new(), "toilet_request", &facility_id, minute)?
    };
    effect.facility_id = Some(facility_id);
    Ok(effect)
}

/// The `fetch_water` and `collect_meal` branch, changing nothing.
fn plan_resource(world: &World, events: &Events<'_>, trips: &Trips<'_>, agent_id: &Arc<str>, zone_id: &Arc<str>, kind: &str, minute: i64) -> Result<TripEffect> {
    let mut effect = TripEffect::new();
    let selected = travel::select(world, &trips.routing(), trips.knows, agent_id, Some(kind), &mut effect.waits)?;
    events.reserve(effect.waits.len());
    let walking = trips.walk.walking();
    match selected {
        Selected::NoFacility => {
            effect.outcome = Some(("rejected", "no_facility", None));
            effect.rejected = true;
        }
        Selected::NoRoute(facility_id) => {
            effect.facility_id = Some(facility_id);
            effect.outcome = Some(("rejected", "no_route", None));
            effect.rejected = true;
        }
        Selected::Found(facility_id, route) => {
            if route.destination == *zone_id && has_independent(world, &trips.queues, &facility_id)? {
                effect.at_once = true;
            } else if route.destination == *zone_id {
                match travel::approach_route(world, walking, trips.walk.section, agent_id, &facility_id)? {
                    None => effect.outcome = Some(("rejected", "room_route_blocked", None)),
                    Some((local, _)) if local.minutes <= 0 => effect.at_once = true,
                    Some((local, points)) => {
                        effect.trip = plan_facility_trip(world, events, walking, agent_id, local, points, "resource_request", &facility_id, minute)?;
                        if effect.trip.is_some() {
                            effect.outcome = Some(("travel_scheduled", "in_room_route_available", None));
                        }
                    }
                }
            } else {
                effect.trip = plan_facility_trip(world, events, walking, agent_id, route, Vec::new(), "resource_request", &facility_id, minute)?;
                if effect.trip.is_some() {
                    effect.outcome = Some(("travel_scheduled", "route_available", None));
                } else {
                    effect.outcome = Some(("rejected", "travel_rejected", Some(Arc::from(TRAVEL_REJECTED_DETAIL))));
                    effect.rejected = true;
                }
            }
            effect.facility_id = Some(facility_id);
        }
    }
    Ok(effect)
}

/// The request of a person at the facility, through the queue's own kernel: None once queued, or
/// the text of the `ValueError` the water or meal queue turns them down with, having written what
/// it writes before that.
fn queue_at_once(world: &World, inputs: &Inputs<'_>, trips: &Trips<'_>, agent_id: &str, facility_id: &str) -> WriteResult<Option<String>> {
    match &trips.asked {
        Asked::Toilet { patched } => {
            if *patched {
                return Err(Error::unsupported(PATCHED).into());
            }
            let geo = plan_of(trips.walk.geo)?;
            let request = toilet::Request { agent_id, facility_id, queue_id: None, queues: &trips.queues, geo, needs: trips.needs };
            toilet::queue_up(world, &request)?;
            Ok(None)
        }
        Asked::Resource { patched, config, service: held, ledger } => {
            if *patched {
                return Err(Error::unsupported(PATCHED).into());
            }
            // Read before the request, which may write the task counter and then turn the person
            // down.
            check_service(world, held)?;
            check_metric_state(world, inputs.metric_state_held)?;
            let Some(config) = config else {
                return Err(Error::unsupported(MANAGER_UNREAD).into());
            };
            let geo = plan_of(trips.walk.geo)?;
            let manager = Manager { config, service: held.clone(), ledger: ledger.clone(), queues: trips.queues.clone(), geo, metric_state_held: inputs.metric_state_held };
            let pending = pending_reserves(world)?;
            let request = service::Request { agent_id, facility_id, minute: Minute::Absent, queue_id: None, needs: trips.needs };
            match service::queue_up(world, &manager, &request, &pending)? {
                Requested::Queued(_) => Ok(None),
                Requested::Refused(text) => Ok(Some(text)),
            }
        }
    }
}

/// Whether the world holds `resource_service_v1`, as a resource action read it. Without a resource
/// service, the step's `held_mirror` would refuse: it never reads it then.
fn service_held<'a>(inputs: &'a Inputs<'_>) -> Result<&'a Held> {
    static NO_SERVICE: Held = Held::Other(OTHER_SERVICE);
    match inputs.trips.as_ref().map(|trips| &trips.asked) {
        Some(Asked::Resource { service, .. }) => Ok(service),
        None => Ok(&NO_SERVICE),
        Some(Asked::Toilet { .. }) => Err(missing("resource service")),
    }
}

/// What `sleep` does beside the activity, planned.
enum SleepPlan<'w> {
    /// A walk to the bed, with the reservation the person takes.
    Walk(Bed, Box<PlannedTrip>, Events<'w>),
    /// The person is at the bed.
    Stay(Bed),
    /// No trip could be scheduled: whether the person held a reservation to give up.
    NoRoom(bool),
}

/// The `sleep` branch, changing nothing but the empty reservations of an older overlay
/// (`overlay::plan_bed`). None for no bed.
fn plan_sleep<'w>(world: &'w World, sleep: &Sleep<'_>, overlay: &Held, agent_id: &Arc<str>, zone_id: &Arc<str>) -> Result<Option<SleepPlan<'w>>> {
    let beds = Beds { overlay, movement: &sleep.walk.movement, schema: sleep.schema, walking: sleep.walk.section, geo: sleep.walk.geo.map(|geo| &**geo) };
    let Some(bed) = overlay::plan_bed(world, &beds, agent_id)? else {
        return Ok(None);
    };
    if bed.travel_minutes <= 0 {
        return Ok(Some(SleepPlan::Stay(bed)));
    }
    let events = Events::of(world)?;
    let minute = world.minute_index()?;
    let route = Route {
        origin: zone_id.clone(),
        destination: zone_id.clone(),
        zone_ids: vec![zone_id.clone(), zone_id.clone()],
        edge_ids: Vec::new(),
        minutes: bed.travel_minutes,
    };
    let trip = travel::plan_travel(
        world,
        &events,
        sleep.walk.walking(),
        Trip {
            agent_id: agent_id.clone(),
            route,
            points: bed.path.to_vec(),
            purpose: "sleep_bed",
            facility_id: None,
            minute,
            causation_id: None,
            support_request_id: None,
            support_staff_id: None,
        },
    )?;
    Ok(Some(match trip {
        Some(trip) => SleepPlan::Walk(bed, Box::new(trip), events),
        None => SleepPlan::NoRoom(holds_bed(world, overlay, agent_id)?),
    }))
}

/// `position.update(...)` of a person already at their bed, on the row of `movement_v1` (an
/// attached tree follows it): nothing where the world has no movement state or the row no record
/// of them.
fn place_in_bed(world: &World, movement: &Held, agent_id: &str, bed: &Bed, zone_id: &Arc<str>) -> Result<()> {
    match movement {
        Held::Absent => Ok(()),
        Held::Other(reason) => Err(Error::unsupported(*reason)),
        Held::Held => crate::rules::movement::place_at_point(world, agent_id, zone_id, bed.point).map(drop),
    }
}

/// The `social_interaction` event a social action schedules, planned: the event, its writer, the
/// participant, and the minute the next decision waits until. None where no one co-located is met
/// before the Run's end.
fn plan_social<'w>(
    world: &'w World,
    interaction: &Interaction,
    minutes: i64,
    participant: Option<&Arc<str>>,
    own_id: &Arc<str>,
    zone_id: &Arc<str>,
) -> Result<Option<(EventRow, Events<'w>, Arc<str>, i64)>> {
    let minute = world.minute_index()?;
    let duration = world.duration_minutes()?;
    let Some(due) = minute.checked_add(minutes) else {
        return refuse("a due minute out of range");
    };
    let meets = match participant {
        None => None,
        Some(id) => world.rows()?.agents.get(&**id).map(|row| (row.profile.id.clone(), row.zone_id.clone())),
    };
    let Some((participant_id, participant_zone)) = meets else {
        return Ok(None);
    };
    if participant_id == *own_id || participant_zone != *zone_id || due >= duration {
        return Ok(None);
    }
    let events = Events::of(world)?;
    let sequence = events.next_sequence()?;
    let event_id: Arc<str> = Arc::from(format!("autonomous-social-action-{sequence}"));
    if events.is_taken(&event_id, Some(sequence))? {
        return refuse("the event's identity is taken");
    }
    let names = &*NAMES;
    let mut payload = IndexMap::with_capacity(9);
    payload.insert(names.interaction_type.clone(), V::Str(Arc::from(interaction.kind)));
    payload.insert(names.agent_id.clone(), V::Str(own_id.clone()));
    payload.insert(names.participant_agent_id.clone(), V::Str(participant_id.clone()));
    payload.insert(names.conversation_state_context.clone(), context_value(world, own_id, &participant_id)?);
    payload.insert(names.zone_id.clone(), V::Str(zone_id.clone()));
    payload.insert(names.topic_id.clone(), V::Str(Arc::from(interaction.topic)));
    payload.insert(names.action.clone(), V::Str(Arc::from(interaction.action)));
    payload.insert(names.reason_short.clone(), V::Str(names.autonomous_need_choice.clone()));
    payload.insert(names.interaction_duration_minutes.clone(), V::Int(minutes));
    let mut provenance = IndexMap::with_capacity(3);
    provenance.insert(names.source_type.clone(), V::Str(names.assumption.clone()));
    provenance.insert(names.source_id.clone(), V::Str(names.social_source.clone()));
    provenance.insert(names.policy_version.clone(), V::Str(names.social_policy.clone()));
    let Some(event) = EventRow::new(
        event_id,
        EventKind::Endogenous,
        due,
        SOCIAL_PRIORITY,
        sequence,
        names.event_type.clone(),
        payload,
        Some(names.social_policy.clone()),
        None,
        provenance,
    ) else {
        return refuse("the event's values are not what an event takes");
    };
    Ok(Some((event, events, participant_id, due)))
}

/// `_apply_effect` of every action but `seek_help` and `seek_information` (`seek_write`): the
/// target, the next decision, and whether the decision committed. Refused before any write, but
/// for the queue's kernel of a request at once, which runs after the capacity waits are written.
pub fn apply(world: &World, inputs: &Inputs<'_>, proposal: &Proposal) -> WriteResult<Applied> {
    let action = proposal.action;
    let agent_id = &proposal.agent_id;
    let interaction = match action {
        Action::Socialize => Some(&SOCIALIZE),
        Action::CareFamily => Some(&CARE_FAMILY),
        Action::HelpOthers => Some(&HELP_OTHERS),
        Action::SeekHelp | Action::SeekInformation => return Err(Error::Runtime("a request is the step's seek_write".to_owned()).into()),
        _ => None,
    };
    let (zone_id, own_id) = agent_place(world, agent_id)?;

    // Plan.
    let sleeps = action == Action::Sleep;
    let bed = !sleeps && holds_bed(world, &inputs.overlay, agent_id)?;
    let sleep = if sleeps {
        let sleep = inputs.sleep.as_ref().ok_or_else(|| missing("sleep"))?;
        plan_sleep(world, sleep, &inputs.overlay, agent_id, &zone_id)?
    } else {
        None
    };
    let social = match interaction {
        Some(interaction) => {
            let minutes = inputs.social_minutes.ok_or_else(|| missing("social minutes"))?;
            plan_social(world, interaction, minutes, proposal.target_id.as_ref(), &own_id, &zone_id)?
        }
        None => None,
    };
    let resource = action.resource();
    let mut trip_effect = None;
    if action.trips() {
        let events = Events::of(world)?;
        let minute = world.minute_index()?;
        let planned = match (&inputs.trips, resource) {
            (Some(trips), None) => plan_toilet(world, &events, trips, agent_id, &zone_id, minute)?,
            (Some(trips), Some((kind, _))) => plan_resource(world, &events, trips, agent_id, &zone_id, kind, minute)?,
            // No resource service.
            (None, Some(_)) => TripEffect { rejected: true, ..TripEffect::new() },
            (None, None) => return Err(missing("trips").into()),
        };
        if planned.outcome.is_some() {
            check_service(world, service_held(inputs)?)?;
        }
        if planned.rejected {
            check_metric_state(world, inputs.metric_state_held)?;
        }
        trip_effect = Some((planned, events, minute));
    }
    // A capacity wait comes before the queue's own events: `_route` recorded it while the facility
    // was chosen, and its sequence is only reserved. Before a request at once it is written here,
    // so the queue's kernel takes the next sequences. That kernel's refusal then leaves the wait
    // written, which no Python step could undo; the step has none (segments.py), so it fails.
    if let Some((planned, events, _)) = trip_effect.as_mut() {
        if planned.at_once {
            for wait in planned.waits.drain(..) {
                travel::write_capacity_wait(world, events, wait)?;
            }
        }
    }
    // The queue's kernel, which writes all it does or refuses having written nothing. It reads
    // nothing that the writes below change.
    if let (Some((planned, ..)), Some(trips)) = (trip_effect.as_mut(), &inputs.trips) {
        if let (true, Some(facility_id)) = (planned.at_once, planned.facility_id.clone()) {
            if let Some(text) = queue_at_once(world, inputs, trips, agent_id, &facility_id)? {
                // `except ValueError as exc`: the record says why, with `str(exc)`, and the person
                // is not at the facility's queue.
                planned.at_once = false;
                planned.outcome = Some(("rejected", "request_rejected", Some(Arc::from(text))));
                planned.rejected = true;
            }
        }
    }

    // Write. Nothing below refuses.
    if bed {
        release_bed(world, &inputs.overlay, agent_id)?;
    }
    let next = social.as_ref().map_or(proposal.next_decision_at, |(.., due)| *due);
    let walks_to_bed = matches!(&sleep, Some(SleepPlan::Walk(..)));
    let still = match action {
        Action::Wait => Some(Activity::Idle),
        Action::Rest => Some(Activity::Resting),
        Action::Sleep if !walks_to_bed => Some(Activity::Sleeping),
        _ => None,
    };
    set_person(world, agent_id, next, still)?;
    let mut target = None;
    if let Some((event, events, participant_id, _)) = social {
        events.allocate_sequence()?;
        events.schedule(event)?;
        target = Some(participant_id);
    }
    match sleep {
        None | Some(SleepPlan::NoRoom(false)) => {}
        Some(SleepPlan::Walk(bed, trip, events)) => {
            overlay::write_bed(world, &inputs.overlay, agent_id, &bed)?;
            travel::write_travel(world, &events, *trip)?;
            target = Some(Arc::from(bed.target_id.as_str()));
        }
        Some(SleepPlan::Stay(bed)) => {
            overlay::write_bed(world, &inputs.overlay, agent_id, &bed)?;
            let movement = &inputs.sleep.as_ref().ok_or_else(|| missing("sleep"))?.walk.movement;
            place_in_bed(world, movement, agent_id, &bed, &zone_id)?;
            target = Some(Arc::from(bed.target_id.as_str()));
        }
        Some(SleepPlan::NoRoom(true)) => release_bed(world, &inputs.overlay, agent_id)?,
    }
    if let Some((planned, events, minute)) = trip_effect {
        // What `_route` recorded while the facility was chosen, before the trip's sequence.
        for wait in planned.waits {
            travel::write_capacity_wait(world, &events, wait)?;
        }
        let facility_id = planned.facility_id;
        if planned.at_once {
            if let Some(facility_id) = &facility_id {
                target = Some(facility_id.clone());
            }
        }
        if let Some(trip) = planned.trip {
            travel::write_travel(world, &events, trip)?;
            if let Some(facility_id) = &facility_id {
                target = Some(facility_id.clone());
            }
        }
        if let (Some((outcome, reason, detail)), Some((_, action_name))) = (planned.outcome, resource) {
            service::append_outcome(
                world,
                service_held(inputs)?,
                RequestRecord {
                    minute,
                    agent_id: agent_id.clone(),
                    action: Arc::from(action_name),
                    outcome: Arc::from(outcome),
                    reason: Arc::from(reason),
                    facility_id,
                    origin_zone_id: zone_id.clone(),
                    detail,
                },
            )?;
        }
        if planned.rejected {
            add_metric(world, REJECTED_METRIC)?;
        }
    }
    let committed = still.is_some() || target.is_some();
    Ok(Applied { target, next_decision_at: next, committed })
}

// ---- seek_help and seek_information --------------------------------------------------------------

/// What `seek_write` did: the request rejected, or asked, with what the step does after the
/// request's writes.
pub enum Sought {
    /// `StaffService.request` raised its `ValueError`: the step's answer is the metric, written.
    Rejected { next: i64 },
    Asked {
        request_id: Arc<str>,
        /// Whether the request is pending; false for one closed at once in the Run's last minute.
        is_open: bool,
        minute: i64,
        /// The receipt of a request closed at once (`interrupted`), still to append.
        interrupted: Option<Transition>,
        /// The information model's record of the request is due.
        pressure_due: bool,
    },
}

/// `model_state[INFORMATION]["query_pressure"][agent] >= policy.config.query_threshold`.
fn query_pressure_is_high(world: &World, information: &Information, agent_id: &str) -> Result<bool> {
    let Some(threshold) = information.threshold else {
        return refuse(THRESHOLD_UNREAD);
    };
    let Some(row) = held_row::<InformationRow>(world, INFORMATION, &information.held)? else {
        return refuse("no information state");
    };
    match row.query_pressure.get(agent_id) {
        Some(pressure) => Ok(*pressure >= threshold),
        None => refuse("the person has no query pressure"),
    }
}

/// `InformationSpreadPolicy.record_support_request(state, agent_id=, request_id=, minute=)` on the
/// row of `information_spread_v1` (an attached tree follows it): the person's heard claims, sorted,
/// tied to the request, the count, and their query pressure back to 0.0. Nothing is written where
/// they hold no heard claim. A person with no beliefs is the Python rule's `KeyError`.
pub fn record_support_request(world: &World, held: &Held, agent_id: &str, request_id: &str, minute: i64) -> Result<()> {
    match held {
        Held::Absent => return Err(Error::Runtime("no information state".to_owned())),
        Held::Other(reason) => return Err(Error::unsupported(*reason)),
        Held::Held => {}
    }
    let (agent_key, message_ids) = {
        let row = world.row::<InformationRow>(INFORMATION)?;
        let Some((agent_key, beliefs)) = row.beliefs.get_key_value(agent_id) else {
            // `runtime_state["beliefs"][agent_id]`.
            return Err(Error::Key(agent_id.to_owned()));
        };
        // `sorted` of str: bytes of UTF-8 sort as code points do.
        let mut message_ids: Vec<Arc<str>> = beliefs.iter().filter(|(_, belief)| &*belief.belief == "heard").map(|(id, _)| id.clone()).collect();
        message_ids.sort_unstable_by(|first, second| first.as_bytes().cmp(second.as_bytes()));
        (agent_key.clone(), message_ids)
    };
    if message_ids.is_empty() {
        return Ok(());
    }
    let mut row = world.row_mut::<InformationRow>(INFORMATION)?;
    row.support_requests.push(SupportRequestRow { request_id: Arc::from(request_id), agent_id: agent_key.clone(), minute, message_ids });
    row.counters.support_requests += 1;
    match row.query_pressure.get_mut(&agent_key) {
        Some(pressure) => *pressure = 0.0,
        None => {
            row.query_pressure.insert(agent_key, 0.0);
        }
    }
    Ok(())
}

/// A write's error where only the core's errors go on: a duplicate event as the `RuntimeError` it
/// never is in a world the checks let through.
fn plain(error: WriteError) -> Error {
    match error {
        WriteError::Error(error) => error,
        WriteError::Duplicate(text) => Error::Runtime(text),
    }
}

/// `append_applied_care_transition` of the receipt of a request closed at once, as its entry does
/// it: nothing without the namespace, and a refusal is that entry's KernelRefusal.
fn append_interrupted(world: &mut World, shared: &Held, transition: Transition) -> Result<()> {
    let appended = match shared {
        Held::Absent => Ok(()),
        Held::Other(reason) => Err(Error::unsupported(*reason)),
        Held::Held => care_receipts::append(world, transition).map_err(plain),
    };
    appended.map_err(|error| Error::refused("care_receipts.append_transition", error))
}

/// The care receipt of a request: `transition(status)` of the step.
fn transition_of(world: &World, request_id: &Arc<str>, agent_id: &Arc<str>, status: &'static str) -> Result<Transition> {
    Ok(Transition {
        request_id: request_id.clone(),
        agent_id: agent_id.clone(),
        household_id: kit::household_in(world, agent_id)?,
        support_kind: support_kind_of(world, agent_id)?,
        status,
        staff_id: None,
        causation_id: None,
        source_ids: source_ids_of(world, agent_id)?,
        outcome: None,
    })
}

/// The `seek_help` and `seek_information` branch of `_apply_effect`, up to the request's writes:
/// `StaffService.request`, the care receipt of a `seek_help`, the bed given up and the person's next
/// decision. Refused before any write.
pub fn seek_write(world: &mut World, inputs: &Inputs<'_>, proposal: &Proposal) -> WriteResult<Sought> {
    let seek_help = match proposal.action {
        Action::SeekHelp => true,
        Action::SeekInformation => false,
        _ => return Err(Error::Runtime("not a request".to_owned()).into()),
    };
    let seek = inputs.seek.as_ref().ok_or_else(|| missing("request"))?;
    let agent_id = &proposal.agent_id;
    let next = proposal.next_decision_at;
    agent_place(world, agent_id)?;
    let minute = world.minute_index()?;
    let (planned, opened, bed) = {
        let events = Events::of(world)?;
        check_metric_state(world, inputs.metric_state_held)?;
        let bed = holds_bed(world, &inputs.overlay, agent_id)?;
        let planned = support::plan_request(world, &seek.service, events.reserved(), agent_id, minute)?;
        let opened = match &planned {
            PlannedRequest::Opens { request_id, event, .. } => Some((request_id.clone(), event.sequence + 1)),
            PlannedRequest::Closes { request_id, .. } => Some((request_id.clone(), events.next_sequence()?)),
            PlannedRequest::Rejected => None,
        };
        (planned, opened, bed)
    };
    let mut care: Option<PlannedTransition> = None;
    let mut interrupted = None;
    let mut pressure_due = false;
    if let Some((request_id, sequence)) = &opened {
        if seek_help {
            match &seek.shared {
                Held::Absent => {}
                Held::Other(NAMESPACE_IS_NONE) => return Err(Error::unsupported(NAMESPACE_IS_NONE).into()),
                held => {
                    let transition = transition_of(world, request_id, agent_id, "requested")?;
                    if let Held::Other(reason) = held {
                        return Err(Error::unsupported(*reason).into());
                    }
                    care = Some(care_receipts::plan_transition(world, transition, *sequence)?);
                    if matches!(planned, PlannedRequest::Closes { .. }) {
                        interrupted = Some(transition_of(world, request_id, agent_id, "interrupted")?);
                    }
                }
            }
        }
        if let Some(information) = &seek.information {
            pressure_due = !seek_help || query_pressure_is_high(world, information, agent_id)?;
        }
    }

    // Write. Nothing below refuses.
    if bed {
        release_bed(world, &inputs.overlay, agent_id)?;
    }
    set_person(world, agent_id, next, None)?;
    let (request_id, is_open) = match planned {
        PlannedRequest::Rejected => {
            add_metric(world, SUPPORT_REJECTED_METRIC)?;
            return Ok(Sought::Rejected { next });
        }
        PlannedRequest::Opens { request_id, timeout_minute, event } => {
            support::write_request(world, agent_id, minute, &request_id, timeout_minute, event)?;
            (request_id, true)
        }
        PlannedRequest::Closes { request_id, until } => {
            support::write_closed_request(world, inputs.metric_state_held, agent_id, minute, &request_id, until)?;
            (request_id, false)
        }
    };
    if let Some(care) = care {
        care_receipts::write_transition(world, care)?;
    }
    Ok(Sought::Asked { request_id, is_open, minute, interrupted, pressure_due })
}

/// The rest of the `seek_help` and `seek_information` branch, after `seek_write`: the receipt of a
/// request closed at once, the information model's record, and for a pending request the LLM's
/// hooks through the host, each with nothing borrowed (rule R6). A `ValueError` of any of them is
/// the step's `except ValueError`: the metric of a refused request. The person's next decision is
/// read again after the host, which may have changed it.
pub fn seek_after<C: WorldCell, H: Host>(cell: &C, host: &mut H, inputs: &Inputs<'_>, proposal: &Proposal, sought: Sought) -> std::result::Result<Applied, H::Error> {
    let (request_id, is_open, minute, interrupted, pressure_due) = match sought {
        Sought::Rejected { next } => return Ok(Applied { target: None, next_decision_at: next, committed: false }),
        Sought::Asked { request_id, is_open, minute, interrupted, pressure_due } => (request_id, is_open, minute, interrupted, pressure_due),
    };
    let seek = inputs.seek.as_ref().ok_or_else(|| missing("request"))?;
    let agent_id = &proposal.agent_id;
    let after = (|| -> std::result::Result<(), H::Error> {
        if let Some(interrupted) = interrupted {
            cell.with(|world| append_interrupted(world, &seek.shared, interrupted))??;
        }
        if pressure_due {
            let held = seek.information.as_ref().map_or(&Held::Absent, |information| &information.held);
            cell.with(|world| record_support_request(world, held, agent_id, &request_id, minute))??;
        }
        if !is_open {
            return Ok(());
        }
        let llm = host.llm()?;
        if llm.router {
            let hash = proposal.observed_state_hash.as_ref().ok_or_else(|| missing("observed state hash"))?;
            host.prioritize_support_request(&request_id, agent_id, hash)?;
        }
        if llm.social_policy {
            host.consider_support_request(agent_id, &request_id)?;
        }
        Ok(())
    })();
    match after {
        Ok(()) => {}
        // `except ValueError` of the step, around all of the above: read the metric's row again.
        Err(error) if error.class() == ErrorClass::Value => {
            cell.with(|world| check_metric_state(world, inputs.metric_state_held).and_then(|()| add_metric(world, SUPPORT_REJECTED_METRIC)))??;
        }
        Err(error) => return Err(error),
    }
    // The person now seeks help: their activity has changed unless they were already, which the
    // service does not let them. The host may have moved their next decision.
    let next_now = cell.with(|world| -> Result<Option<i64>> { Ok(world.rows()?.agents.get(&**agent_id).map(|row| row.next_decision_at)) })??;
    let next_now = next_now.ok_or_else(|| Error::Runtime("the person has gone".to_owned()))?;
    // A request closed at once did not commit the decision.
    Ok(Applied { target: Some(request_id), next_decision_at: next_now, committed: is_open })
}

/// `_apply_effect`, whole, for a build that holds its world in Rust: `apply` on the world, or
/// `seek_write` and `seek_after` of a request. A duplicate event is a `RuntimeError` here.
pub fn apply_effect<C: WorldCell, H: Host>(cell: &C, host: &mut H, inputs: &Inputs<'_>, proposal: &Proposal) -> std::result::Result<Applied, H::Error> {
    if proposal.action.seeks() {
        let sought = cell.with(|world| seek_write(world, inputs, proposal))?.map_err(plain)?;
        return seek_after(cell, host, inputs, proposal, sought);
    }
    Ok(cell.with(|world| apply(world, inputs, proposal))?.map_err(plain)?)
}
