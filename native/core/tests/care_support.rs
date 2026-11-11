//! The support requests and the end of a staff member's support (`rules::support`, lane P8 b4) over
//! a `World` built here, with no Python: a request's ID, plan and writes; a timeout; the care of a
//! completion with and without the body state, the selected formula asked for where the care needs
//! it; the receipts around a request; the support kind; the built-in care formulas.

use std::sync::Arc;

use indexmap::IndexMap;
use sheltersim_core::config::body::CareOutcome;
use sheltersim_core::entities::{Activity, AgentRow, Entities, Mobility, NeedRow, ProfileRow, RequestRow, RequestStatus, Skills, StaffRow, TaskRow};
use sheltersim_core::error::{Error, Outcome};
use sheltersim_core::events::log::EventLog;
use sheltersim_core::events::queue::Queue;
use sheltersim_core::row::{IdMap, Row, ValueRow};
use sheltersim_core::rules::care_receipts::{self, Transition};
use sheltersim_core::rules::kit::Held;
use sheltersim_core::rules::support::{
    self, Completion, Formula, Handler, Inputs, PlannedRequest, RequestService, SupportEvent, AFTER_UNREAD, COOLDOWN_UNREAD, NAMESPACE_IS_NONE,
    NO_LONGER_PENDING, OTHER_DOCUMENT, POLICY_UNREAD,
};
use sheltersim_core::state::body_state::{BodyRecord, BodyRow, NAME as BODY};
use sheltersim_core::state::metric_state::MetricRow;
use sheltersim_core::state::shared_operations::{CareStatus, Heads, Receipts, SharedOperationsRow, NAME as SHARED, POLICY};
use sheltersim_core::value::V;
use sheltersim_core::world::{Fit, Place, World, ID_COUNTERS, METRIC_STATE, MODEL_CONFIGS};

fn text(value: &str) -> Arc<str> {
    Arc::from(value)
}

fn map(entries: &[(&str, V)]) -> V {
    V::Map(entries.iter().map(|(key, value)| (text(key), value.clone())).collect())
}

fn unsupported(reason: &str) -> Error {
    Error::Unsupported(reason.to_owned().into())
}

fn needs() -> NeedRow {
    NeedRow { toilet_urgency: 0.0, hunger: 0.0, fatigue: 0.0, hydration_need: 0.0 }
}

fn agent(id: &str, activity: Activity) -> AgentRow {
    AgentRow {
        profile: ProfileRow {
            id: text(id),
            group_id: Some(text("h1")),
            initial_zone_id: text("z0"),
            mobility: Mobility::Independent,
            support_need: 0.0,
            stress_sensitivity: 1.0,
            rng_key: text(id),
            initial_needs: needs(),
            provenance: V::Map(IndexMap::new()),
        },
        zone_id: text("z0"),
        activity,
        target_id: None,
        needs: needs(),
        stress: 0.5,
        trust: 0.5,
        next_decision_at: 0,
        support_request_id: None,
        support_cooldown_until: 0,
    }
}

fn slot(world: &mut World, name: &str, place: Place, row: Box<dyn Row>) {
    let index = world.slots.add(text(name), place).unwrap();
    world.slots.at(index).unwrap().try_borrow_mut().unwrap().row = Some(row);
}

/// A world at minute 7 of 100, holding its queue and log, with `id_counters` at `event: 10`,
/// `metric_state`, two idle people and a staff member.
fn world() -> World {
    let mut world = World::default();
    {
        let scalars = world.scalars.get_mut();
        scalars.duration_minutes = Fit::Ok(100);
        scalars.minute_index = Fit::Ok(7);
    }
    let mut entities = Entities::default();
    entities.agents.insert(text("a1"), agent("a1", Activity::Idle));
    entities.agents.insert(text("a2"), agent("a2", Activity::Idle));
    entities.staff.insert(
        text("s1"),
        StaffRow { id: text("s1"), zone_id: text("z0"), skills: Skills(vec![text("support")]), work_queue: Vec::new(), busy_until: 0, active_task_id: None },
    );
    *world.entities.get_mut() = entities;
    slot(&mut world, ID_COUNTERS, Place::Field, Box::new(ValueRow(map(&[("event", V::Int(10))]))));
    slot(&mut world, METRIC_STATE, Place::Field, Box::new(MetricRow::default()));
    slot(&mut world, MODEL_CONFIGS, Place::Field, Box::new(ValueRow(V::Map(IndexMap::new()))));
    world.hold_queue(Arc::new(Queue::new()));
    world.hold_log(Arc::new(EventLog::new()));
    world
}

fn shared() -> SharedOperationsRow {
    SharedOperationsRow { schema_version: 1, policy_version: text(POLICY), support_requests: Heads::default(), history: Receipts::default(), history_truncated: false }
}

fn record(reassurance: f64, hygiene_need: f64) -> BodyRecord {
    BodyRecord {
        sleep_debt_minutes: 0.0,
        uncertainty: 0.25,
        reassurance,
        discontent: 0.25,
        positive_affect: 0.5,
        negative_affect: 0.5,
        hygiene_need,
    }
}

fn body(config: V) -> BodyRow {
    let mut agents = IdMap::default();
    agents.insert(text("a1"), record(0.25, 0.75));
    BodyRow {
        schema_version: 1,
        policy_version: text("body"),
        source_id: text("ASM-BODY"),
        config,
        agents,
        relationships: Default::default(),
        history: Default::default(),
    }
}

fn inputs(shared: Held, body_present: bool) -> Inputs {
    Inputs { shared, body_present, metric_state_held: true }
}

fn metric(world: &World, name: &str) -> f64 {
    world.metric_state().unwrap().get(name).copied().unwrap_or(0.0)
}

fn service() -> RequestService {
    RequestService { unresolved_after_minutes: Some(30), cooldown_minutes: Some(15), policy_version: Some(text("staff-support-v1")) }
}

/// A request of a1, opened at minute 2, its receipt `requested` where the world has the namespace.
fn opened(world: &mut World, status: RequestStatus) {
    let request = RequestRow {
        id: text("q-1"),
        agent_id: text("a1"),
        requested_minute: 2,
        timeout_minute: 32,
        status,
        assigned_staff_id: None,
        task_id: None,
        resolved_minute: None,
        cooldown_until: 0,
        last_assignment_attempt_minute: None,
        last_assignment_wait_reason_code: None,
    };
    let mut rows = world.rows_mut().unwrap();
    rows.support_requests.insert(text("q-1"), request);
    rows.agents.get_mut("a1").unwrap().support_request_id = Some(text("q-1"));
}

fn requested(world: &mut World, kind: &str) {
    transition(world, kind, "requested");
}

fn transition(world: &mut World, kind: &str, status: &'static str) {
    let transition = Transition {
        request_id: text("q-1"),
        agent_id: text("a1"),
        household_id: Some(text("h1")),
        support_kind: text(kind),
        status,
        staff_id: None,
        causation_id: None,
        source_ids: Vec::new(),
        outcome: None,
    };
    care_receipts::append(world, transition).unwrap();
}

fn event(ids: [Option<&str>; 4], due_minute: i64) -> SupportEvent {
    SupportEvent {
        event_id: text("e-9"),
        due_minute,
        request_id: ids[0].map(text),
        agent_id: ids[1].map(text),
        staff_id: ids[2].map(text),
        task_id: ids[3].map(text),
    }
}

// ---- A request ---------------------------------------------------------------------------------------

#[test]
fn a_request_s_id_is_python_s_digest() {
    // `"support-" + sha256(b"a1\0" + b"7").hexdigest()[:24]` of CPython.
    assert_eq!(&*support::request_id_of("a1", 7), "support-078c841c0b1ca5fe2acb62cb");
}

#[test]
fn a_request_opens_with_its_timeout_event_and_is_written_after() {
    let world = world();
    let PlannedRequest::Opens { request_id, timeout_minute, event } = support::plan_request(&world, &service(), 0, "a1", 7).unwrap() else {
        panic!("a request that opens");
    };
    assert_eq!((&*request_id, timeout_minute), ("support-078c841c0b1ca5fe2acb62cb", 37));
    assert_eq!((&*event.event_id, event.sequence, event.priority, event.due_minute), ("support-timeout-10", 10, 20, 37));
    assert_eq!(event.source_id.as_deref(), Some(&*request_id));
    assert_eq!(
        event.provenance,
        map(&[
            ("source_type", V::Str(text("assumption"))),
            ("source_id", V::Str(text("ASM-STAFF-SUPPORT-LIFECYCLE-V1"))),
            ("policy_version", V::Str(text("staff-support-v1"))),
            ("timeout_minutes", V::Int(30)),
        ])
    );
    // Past two sequences the caller's plan reserved.
    let PlannedRequest::Opens { event: later, .. } = support::plan_request(&world, &service(), 2, "a1", 7).unwrap() else { panic!() };
    assert_eq!(later.sequence, 12);
    // Nothing changed.
    assert_eq!(world.id_counter("event").unwrap(), Some(10));
    assert!(world.rows().unwrap().support_requests.is_empty());

    support::write_request(&world, "a1", 7, &request_id, timeout_minute, event).unwrap();
    let rows = world.rows().unwrap();
    let request = rows.support_requests.get(&request_id).unwrap();
    assert_eq!((request.status, request.requested_minute, request.timeout_minute), (RequestStatus::Pending, 7, 37));
    let person = rows.agents.get("a1").unwrap();
    assert_eq!((person.activity, person.support_request_id.as_deref()), (Activity::SeekingHelp, Some(&*request_id)));
    drop(rows);
    assert_eq!(world.id_counter("event").unwrap(), Some(11));
    assert!(world.queue().unwrap().has_event("support-timeout-10"));
}

#[test]
fn a_request_is_turned_down_or_closed_at_the_run_s_end() {
    let world = world();
    world.rows_mut().unwrap().agents.get_mut("a2").unwrap().activity = Activity::Moving;
    assert!(matches!(support::plan_request(&world, &service(), 0, "a2", 7).unwrap(), PlannedRequest::Rejected));
    world.rows_mut().unwrap().agents.get_mut("a2").unwrap().support_cooldown_until = 8;
    world.rows_mut().unwrap().agents.get_mut("a2").unwrap().activity = Activity::Resting;
    assert!(matches!(support::plan_request(&world, &service(), 0, "a2", 7).unwrap(), PlannedRequest::Rejected));
    assert_eq!(support::plan_request(&world, &service(), 0, "a9", 7).err(), Some(unsupported("no such agent")));
    // At the Run's last minute the request closes at once.
    let PlannedRequest::Closes { request_id, until } = support::plan_request(&world, &service(), 0, "a1", 99).unwrap() else { panic!() };
    assert_eq!(until, 114);
    support::write_closed_request(&world, true, "a1", 99, &request_id, until).unwrap();
    let request = world.rows().unwrap().support_requests.get(&request_id).unwrap().clone();
    assert_eq!((request.status, request.resolved_minute, request.cooldown_until, request.timeout_minute), (RequestStatus::Unresolved, Some(99), 114, 99));
    assert_eq!(world.rows().unwrap().agents.get("a1").unwrap().activity, Activity::Idle);
    assert_eq!(metric(&world, "support_unresolved_count"), 1.0);
}

#[test]
fn a_value_of_the_service_that_raised_is_refused_where_the_rule_reads_it() {
    let world = world();
    let refused = |service: RequestService, minute: i64| support::plan_request(&world, &service, 0, "a1", minute).err();
    assert_eq!(refused(RequestService { unresolved_after_minutes: None, ..service() }, 7), Some(unsupported(AFTER_UNREAD)));
    // The cooldown is read only for a request that closes at once, the policy only for one that opens.
    assert!(refused(RequestService { cooldown_minutes: None, ..service() }, 7).is_none());
    assert_eq!(refused(RequestService { cooldown_minutes: None, policy_version: None, ..service() }, 7), Some(unsupported(POLICY_UNREAD)));
    assert_eq!(refused(RequestService { cooldown_minutes: None, policy_version: None, ..service() }, 99), Some(unsupported(COOLDOWN_UNREAD)));
    // A person who may not ask reads none of them.
    world.rows_mut().unwrap().agents.get_mut("a1").unwrap().activity = Activity::Moving;
    assert!(refused(RequestService::default(), 7).is_none());
}

// ---- A request times out ------------------------------------------------------------------------------

#[test]
fn a_timeout_settles_a_pending_request_and_says_why_it_does_nothing_else() {
    let mut world = world();
    opened(&mut world, RequestStatus::Pending);
    let inputs = inputs(Held::Absent, false);
    let missing = support::timeout(&world, &inputs, &event([Some("q-1"), None, None, None], 7), 15);
    assert_eq!(missing.err(), Some(Error::Outcome(Outcome::Failed, "support timeout requires request_id and agent_id".to_owned())));
    let other = support::timeout(&world, &inputs, &event([Some("q-1"), Some("a2"), None, None], 7), 15);
    assert_eq!(other.err(), Some(Error::Outcome(Outcome::Failed, "support request 'q-1' does not match its Agent".to_owned())));

    assert_eq!(support::timeout(&world, &inputs, &event([Some("q-1"), Some("a1"), None, None], 7), 15), Ok(None));
    {
        let rows = world.rows().unwrap();
        let request = rows.support_requests.get("q-1").unwrap();
        assert_eq!((request.status, request.resolved_minute, request.cooldown_until), (RequestStatus::Unresolved, Some(7), 22));
        let person = rows.agents.get("a1").unwrap();
        assert_eq!((person.support_request_id.as_deref(), person.support_cooldown_until, person.activity), (None, 22, Activity::Idle));
    }
    assert_eq!(metric(&world, "support_unresolved_count"), 1.0);
    // Settled now: the event is suppressed, and nothing changes.
    assert_eq!(support::timeout(&world, &inputs, &event([Some("q-1"), Some("a1"), None, None], 7), 15), Ok(Some(NO_LONGER_PENDING)));
    assert_eq!(metric(&world, "support_unresolved_count"), 1.0);
    // A field assigned whole is refused before anything changes.
    let held = Inputs { metric_state_held: false, ..inputs };
    opened(&mut world, RequestStatus::Pending);
    assert_eq!(support::timeout(&world, &held, &event([Some("q-1"), Some("a1"), None, None], 7), 15).err(), Some(unsupported("the field holds another dict")));
}

// ---- The receipts around a request ----------------------------------------------------------------

#[test]
fn the_receipts_follow_the_request_where_the_namespace_is() {
    let mut world = world();
    opened(&mut world, RequestStatus::Unresolved);
    // Without the namespace nothing is recorded; with None there, it is refused.
    let timeout = event([Some("q-1"), None, None, None], 7);
    support::record_interrupted(&mut world, &inputs(Held::Absent, false), &timeout).unwrap();
    assert_eq!(world.id_counter("event").unwrap(), Some(10));
    assert!(support::record_interrupted(&mut world, &inputs(Held::Other(NAMESPACE_IS_NONE), false), &timeout).is_err());

    slot(&mut world, SHARED, Place::Namespace, Box::new(shared()));
    requested(&mut world, "emotional_support");
    // Another document than the slot's.
    assert!(support::record_interrupted(&mut world, &inputs(Held::Other(OTHER_DOCUMENT), false), &timeout).is_err());
    support::record_interrupted(&mut world, &inputs(Held::Held, false), &timeout).unwrap();
    // An interrupted request's receipt is the last: its head leaves the open ones.
    assert!(world.row::<SharedOperationsRow>(SHARED).unwrap().support_requests.map.get("q-1").is_none());
    assert!(world.log().unwrap().has_event_id("care-support-transition-11"));
    assert_eq!(world.id_counter("event").unwrap(), Some(12));
}

#[test]
fn an_assignment_records_its_receipts_and_a_start_where_the_person_waits() {
    let mut world = world();
    opened(&mut world, RequestStatus::Assigned);
    {
        let mut rows = world.rows_mut().unwrap();
        let request = rows.support_requests.get_mut("q-1").unwrap();
        request.assigned_staff_id = Some(text("s1"));
        request.task_id = Some(text("t-1"));
        rows.agents.get_mut("a1").unwrap().activity = Activity::SeekingHelp;
    }
    let assigned = [text("q-1"), text("q-9")];
    support::record_assigned(&mut world, &inputs(Held::Absent, false), &assigned).unwrap();
    assert_eq!(world.id_counter("event").unwrap(), Some(10));
    slot(&mut world, SHARED, Place::Namespace, Box::new(shared()));
    requested(&mut world, "emotional_support");
    support::record_assigned(&mut world, &inputs(Held::Held, false), &assigned).unwrap();
    let row = world.row::<SharedOperationsRow>(SHARED).unwrap();
    let head = row.support_requests.map.get("q-1").unwrap();
    assert_eq!((head.status, &*head.latest_event_id), (CareStatus::Started, "care-support-transition-12"));
    drop(row);
    // Past the first change, a refusal is the care receipts' KernelRefusal.
    let refused = support::record_assigned(&mut world, &inputs(Held::Other(OTHER_DOCUMENT), false), &assigned);
    assert!(matches!(refused, Err(sheltersim_core::events::writer::WriteError::Error(Error::Refused { ref label, ref reason }))
        if label == "care_receipts.append_transition" && reason == OTHER_DOCUMENT));
}

// ---- A staff member's support ends ------------------------------------------------------------------

/// q-1 assigned to s1 with task t-1 due now, a1 seeking help beside them.
fn assigned_world(kind: &str) -> World {
    let mut world = world();
    opened(&mut world, RequestStatus::Assigned);
    {
        let mut rows = world.rows_mut().unwrap();
        let request = rows.support_requests.get_mut("q-1").unwrap();
        request.assigned_staff_id = Some(text("s1"));
        request.task_id = Some(text("t-1"));
        rows.agents.get_mut("a1").unwrap().activity = Activity::SeekingHelp;
        let staff = rows.staff.get_mut("s1").unwrap();
        staff.active_task_id = Some(text("t-1"));
        staff.work_queue = vec![text("t-1")];
        staff.busy_until = 7;
        rows.active_tasks.insert(
            text("t-1"),
            TaskRow { id: text("t-1"), kind: text("staff_support"), agent_id: Some(text("a1")), resource_id: None, start_minute: 2, due_minute: 7, staff_id: Some(text("s1")) },
        );
    }
    slot(&mut world, SHARED, Place::Namespace, Box::new(shared()));
    slot(&mut world, BODY, Place::Namespace, Box::new(body(map(&[("complaint_reassurance_gain", V::Float(0.25))]))));
    requested(&mut world, kind);
    transition(&mut world, kind, "assigned");
    transition(&mut world, kind, "started");
    world
}

const HANDLER: Handler = Handler { cooldown_minutes: 15, require_colocated: true };

fn completion() -> SupportEvent {
    event([Some("q-1"), Some("a1"), Some("s1"), Some("t-1")], 7)
}

#[test]
fn emotional_support_relieves_the_person_and_records_what_changed() {
    let world = assigned_world("emotional_support");
    let inputs = inputs(Held::Held, true);
    let Completion::Done(Some(planned)) = support::complete_support(&world, &inputs, &completion(), HANDLER, Formula::Unread, 0).unwrap() else {
        panic!("a completion with its receipt");
    };
    assert_eq!((&*planned.row().event_id, planned.detail()), ("care-support-transition-13", "care transition completed recorded after state application"));
    assert_eq!(world.id_counter("event").unwrap(), Some(14));
    let rows = world.rows().unwrap();
    let person = rows.agents.get("a1").unwrap();
    // `max(0, 0.5 - 0.25)` and `min(1, 0.5 + 0.25 * 0.25)`.
    assert_eq!((person.stress, person.trust, person.activity, person.support_cooldown_until), (0.25, 0.5625, Activity::Idle, 22));
    let staff = rows.staff.get("s1").unwrap();
    assert!(staff.work_queue.is_empty() && staff.active_task_id.is_none() && staff.busy_until == 0);
    assert!(rows.active_tasks.get("t-1").is_none());
    assert_eq!(rows.support_requests.get("q-1").unwrap().status, RequestStatus::Resolved);
    drop(rows);
    let row = world.row::<BodyRow>(BODY).unwrap();
    let after = row.agents.get("a1").unwrap();
    assert_eq!((after.reassurance, after.positive_affect, after.negative_affect), (0.5, 0.625, 0.375));
    drop(row);
    assert_eq!(metric(&world, "support_resolved_count"), 1.0);
    let V::Map(payload) = &planned.row().payload else { panic!() };
    assert_eq!(payload.get("outcome_observation"), Some(&V::Str(text("observed_change"))));
}

#[test]
fn another_kind_asks_for_the_selected_formula_with_nothing_written() {
    let world = assigned_world("hygiene_support");
    let inputs = inputs(Held::Held, true);
    assert!(matches!(support::complete_support(&world, &inputs, &completion(), HANDLER, Formula::Unread, 0).unwrap(), Completion::NeedsFormula));
    assert_eq!(world.id_counter("event").unwrap(), Some(13));
    assert!(world.rows().unwrap().active_tasks.get("t-1").is_some());
    // The proportional formula: `0.75 * (1 - 0.5)`.
    let configs = map(&[("body_dynamics", map(&[("hygiene_support_relief", V::Float(0.5))]))]);
    let formula = CareOutcome::parse(&configs, "hygiene-support-proportional-relief-v1").unwrap();
    world.row_mut::<BodyRow>(BODY).unwrap().config = map(&[("hygiene_support_relief", V::Float(0.5))]);
    let Completion::Done(Some(planned)) = support::complete_support(&world, &inputs, &completion(), HANDLER, Formula::Read(Some(&formula)), 0).unwrap() else {
        panic!();
    };
    assert_eq!(world.row::<BodyRow>(BODY).unwrap().agents.get("a1").unwrap().hygiene_need, 0.375);
    let V::Map(payload) = &planned.row().payload else { panic!() };
    assert_eq!(
        payload.get("source_ids"),
        Some(&V::List(
            ["ASM-CARE-HYGIENE-PROPORTIONAL-RELIEF-V1", "ASM-CARE-TRANSITION-PRODUCER-V1", "ASM-LIFE-STATE-FEEDBACK-V1", "ASM-STAFF-SUPPORT-LIFECYCLE-V1"]
                .iter()
                .map(|source| V::Str(text(source)))
                .collect()
        ))
    );
}

#[test]
fn a_completion_that_does_not_match_says_why_with_nothing_written() {
    let world = assigned_world("emotional_support");
    let inputs = inputs(Held::Held, true);
    let failed = |text: &str| Some(Error::Outcome(Outcome::Failed, text.to_owned()));
    let complete = |event: SupportEvent| support::complete_support(&world, &inputs, &event, HANDLER, Formula::Unread, 0).err();
    assert_eq!(complete(event([Some("q-1"), Some(""), Some("s1"), Some("t-1")], 7)), failed("staff completion requires request, Agent, staff, and task IDs"));
    assert_eq!(complete(event([Some("q-1"), Some("a1"), Some("s1"), Some("t-1")], 8)), failed("staff completion does not match an active due task"));
    assert_eq!(complete(event([Some("q-1"), Some("a1"), Some("s1"), Some("t-2")], 7)), failed("staff completion does not match an active due task"));
    assert_eq!(complete(event([Some("q-1"), Some("a2"), Some("s1"), Some("t-1")], 7)), failed("support completion does not match its assigned task"));
    assert_eq!(world.id_counter("event").unwrap(), Some(13));
    // A task the operational events cancelled is suppressed.
    let mut world = assigned_world("emotional_support");
    let state = map(&[("cancelled_staff_task_ids", V::List(vec![V::Str(text("t-2"))]))]);
    slot(&mut world, "operational_events_v1", Place::Namespace, Box::new(ValueRow(state)));
    assert_eq!(
        support::complete_support(&world, &inputs, &event([Some("q-1"), Some("a1"), Some("s1"), Some("t-2")], 7), HANDLER, Formula::Unread, 0).err(),
        Some(Error::Outcome(Outcome::Suppressed, "staff support task 't-2' was cancelled".to_owned()))
    );
}

// ---- What the support kind and the formulas are -----------------------------------------------------

#[test]
fn the_built_in_care_formulas_are_the_catalog_s() {
    let configs = map(&[("body_dynamics", map(&[("x", V::Int(1))]))]);
    let typed = CareOutcome::parse(&configs, "typed-care-support-response-v1").unwrap();
    assert_eq!((&*typed.model_id, &*typed.source_id), ("typed-care-support-response-v1", "ASM-CARE-TYPED-SUPPORT-RESPONSE-V1"));
    assert_eq!(typed.deltas("language_access", "a1").unwrap(), [("uncertainty", -0.05)]);
    let fixed = CareOutcome::parse(&V::Map(IndexMap::new()), "hygiene-support-fixed-relief-v1").unwrap();
    assert_eq!((&*fixed.source_id, fixed.hygiene_after(0.75, 0.5)), ("ASM-CARE-HYGIENE-FIXED-RELIEF-V1", 0.25));
    assert!(CareOutcome::parse(&configs, "a-later-formula").is_err());
    assert!(CareOutcome::parse(&map(&[("body_dynamics", V::Int(1))]), "typed-care-support-response-v1").is_err());
}

#[test]
fn the_support_kind_is_the_first_need_not_yet_met() {
    let mut world = world();
    assert_eq!(&*support::support_kind_of(&world, "a1").unwrap(), "emotional_support");
    let person = map(&[
        ("care_support_needs", V::List(vec![V::Int(3), V::Str(text("language_access"))])),
        ("medication_support", V::Bool(true)),
    ]);
    let context = map(&[("people", map(&[("a1", person)]))]);
    slot(&mut world, care_receipts::POPULATION_CONTEXT, Place::Namespace, Box::new(ValueRow(context)));
    assert_eq!(&*support::support_kind_of(&world, "a1").unwrap(), "language_access");
    assert_eq!(&*support::support_kind_of(&world, "a2").unwrap(), "emotional_support");
}
