//! The inputs the dispatch's kernels read through the host (lane P9-1a, design-p9.md §5): whether
//! the world holds each namespace (`Holding`, H1), the configs' sections (H2), the EventLog's records
//! (`Host::find_record`) and the floor plan (`Host::floor_plan`), each with the texts the PyO3
//! crate's per-call reads said.

mod support;

use std::sync::Arc;

use indexmap::IndexMap;
use sheltersim_core::cell::Shard;
use sheltersim_core::config::{Configs, Fault, FaultClass, Lazy, Section, SectionId, Selection};
use sheltersim_core::error::Error;
use sheltersim_core::events::{EventKind, EventRow, EventStatus};
use sheltersim_core::geometry::Geo;
use sheltersim_core::host::{Hold, Holding, Key, NOT_OWNED, NO_PLAN, TABLE_MAPPING, TABLE_NOT_PLAIN};
use sheltersim_core::row::ValueRow;
use sheltersim_core::rules::common_operations::{self, Logged, NOT_SELECTED};
use sheltersim_core::rules::kit::{self, Held, NO_SUCH_SECTION};
use sheltersim_core::rules::{
    agent_dynamics, body_state, care_receipts, information_events, movement, social_interaction, staff_operation_events,
    support as care, travel_completion,
};
use sheltersim_core::value::V;
use sheltersim_core::world::{Place, World, METRIC_STATE};
use support::{Call, TestHost};

fn text(value: &str) -> Arc<str> {
    Arc::from(value)
}

fn unsupported(reason: &str) -> Error {
    Error::unsupported(reason.to_owned())
}

/// A holding of these keys of `model_state`: (name, hold, none, dict).
fn holding(keys: &[(&str, Hold, bool, bool)]) -> Arc<Holding> {
    let mut holding = Holding::default();
    for (name, hold, none, dict) in keys {
        holding.keys.insert(text(name), Key::with_dict(*hold, *none, *dict));
    }
    Arc::new(holding)
}

fn host_of(holding: Arc<Holding>) -> TestHost {
    TestHost { holding: Some(holding), ..TestHost::default() }
}

fn cell() -> Shard<World> {
    Shard::new(World::default())
}

fn event(event_type: &str, payload: Vec<(&str, V)>) -> EventRow {
    let payload = payload.into_iter().map(|(key, value)| (text(key), value)).collect();
    EventRow::new(text("E1"), EventKind::Exogenous, 3, 0, 1, text(event_type), payload, None, None, IndexMap::new()).unwrap()
}

fn str_v(value: &str) -> V {
    V::Str(text(value))
}

fn map(entries: Vec<(&str, V)>) -> V {
    V::Map(entries.into_iter().map(|(key, value)| (text(key), value)).collect())
}

/// A section put in the configs by the test host's `fill_section`.
fn put<T>(lazy: &Lazy<T>, section: Section<T>) {
    let _ = lazy.get_or_parse(|| section);
}

fn fault() -> Fault {
    Fault::new(FaultClass::Value, "bad config")
}

// ---- What the world holds -------------------------------------------------------------------------

#[test]
fn a_value_that_is_not_a_dict_is_no_state_where_the_rule_reads_dicts() {
    let held = holding(&[("a_v1", Hold::Other, false, false), ("b_v1", Hold::Other, false, true), ("c_v1", Hold::Held, false, true)]);
    assert_eq!(held.held_dict("a_v1", "mine"), Held::Absent);
    assert_eq!(held.held_dict("b_v1", "mine"), Held::Other("mine"));
    assert_eq!(held.held_dict("c_v1", "mine"), Held::Held);
    assert_eq!(held.held_dict("never_v1", "mine"), Held::Absent);
    assert!(held.contains("a_v1") && !held.contains("never_v1"));
    let table = Arc::new(Holding { table: false, ..(*held).clone() });
    assert_eq!(table.held_dict("a_v1", "mine"), Held::Other(TABLE_MAPPING));
    // A key made by `Key::new` is a dict where it holds the slot's tree.
    assert!(Key::new(Hold::Held, false).dict && !Key::new(Hold::Held, true).dict && !Key::new(Hold::Other, false).dict);
}

#[test]
fn the_support_rules_read_each_namespace_with_their_own_texts() {
    let cell = cell();
    let mut host = host_of(holding(&[]));
    let read = care::Inputs::read(&cell, &mut host).unwrap();
    assert_eq!((read.shared, read.body_present, read.metric_state_held), (Held::Absent, false, true));

    let mut given = (*holding(&[
        ("shared_operations_v1", Hold::Other, true, false),
        ("body_state_v1", Hold::Held, false, true),
    ]))
    .clone();
    given.fields.insert(text(METRIC_STATE), false);
    let read = care::Inputs::read(&cell, &mut host_of(Arc::new(given))).unwrap();
    assert_eq!(read.shared, Held::Other(care::NAMESPACE_IS_NONE));
    assert!(read.body_present && !read.metric_state_held);

    let other = holding(&[("shared_operations_v1", Hold::Other, false, true)]);
    assert_eq!(care::Inputs::read(&cell, &mut host_of(other)).unwrap().shared, Held::Other(care::OTHER_DOCUMENT));
    let table = Arc::new(Holding { table: false, ..Holding::default() });
    assert_eq!(care::Inputs::read(&cell, &mut host_of(table.clone())).err(), Some(unsupported(TABLE_NOT_PLAIN)));
    // A timeout reads the metric state alone.
    assert!(care::Inputs::read_metrics(&cell, &mut host_of(table)).unwrap().metric_state_held);
}

#[test]
fn a_holding_read_from_the_slots_follows_a_write_of_the_core() {
    let mut world = World::default();
    let index = world.slots.add(text("shared_operations_v1"), Place::Namespace).unwrap();
    let cell = Shard::new(world);
    let mut host = TestHost::default();
    assert_eq!(care::Inputs::read(&cell, &mut host).unwrap().shared, Held::Absent);
    // A write that gives the namespace a row adds its key: the next read sees it.
    cell.try_borrow_mut().unwrap().slots.at(index).unwrap().try_borrow_mut().unwrap().row = Some(Box::new(ValueRow(V::Map(IndexMap::new()))));
    assert_eq!(care::Inputs::read(&cell, &mut host).unwrap().shared, Held::Held);
    assert_eq!(host.calls.iter().filter(|call| **call == Call::Holding).count(), 2);
}

#[test]
fn the_receipts_say_what_the_namespace_is() {
    let none = holding(&[("shared_operations_v1", Hold::Other, true, false)]);
    assert_eq!(care_receipts::nothing_to_append(&none).err(), Some(unsupported(care_receipts::NAMESPACE_IS_NONE)));
    assert_eq!(care_receipts::nothing_to_append(&holding(&[])), Ok(true));
    let held = holding(&[("shared_operations_v1", Hold::Held, false, true)]);
    assert_eq!(care_receipts::nothing_to_append(&held), Ok(false));
    assert_eq!(care_receipts::check_tree(&held), Ok(()));
    let loose = holding(&[("shared_operations_v1", Hold::NotOwned, false, true)]);
    assert_eq!(care_receipts::check_tree(&loose).err(), Some(unsupported(NOT_OWNED)));
    assert_eq!(care_receipts::check_tree(&holding(&[])).err(), Some(unsupported(common_operations::OTHER_DOCUMENT)));
    // The ingest: nothing without the namespace, a refusal of another value.
    assert_eq!(care_receipts::has_namespace(&holding(&[])), Ok(false));
    assert_eq!(care_receipts::has_namespace(&held), Ok(true));
    let list = holding(&[("shared_operations_v1", Hold::Other, false, false)]);
    assert_eq!(care_receipts::has_namespace(&list).err(), Some(unsupported(care_receipts::ANOTHER_VALUE)));
    // `record_shared_operation`'s first check.
    assert_eq!(common_operations::check_held(&held), Ok(()));
    let dict = holding(&[("shared_operations_v1", Hold::Other, false, true)]);
    assert_eq!(common_operations::check_held(&dict).err(), Some(unsupported(common_operations::OTHER_DOCUMENT)));
    assert_eq!(common_operations::check_held(&none).err(), Some(Error::value_error(NOT_SELECTED)));
    assert_eq!(common_operations::check_held(&Holding { table: false, ..Holding::default() }).err(), Some(unsupported(TABLE_NOT_PLAIN)));
}

#[test]
fn an_ingest_without_the_namespace_reads_nothing_more() {
    let cell = cell();
    let mut host = host_of(holding(&[]));
    let receipt = event("staff_support_continuity", Vec::new());
    care_receipts::ingest_applied(&cell, &mut host, &receipt, 3).unwrap();
    assert_eq!(host.calls, [Call::Holding]);
}

// ---- The sections of the configs ---------------------------------------------------------------------

#[test]
fn an_information_reaction_reads_its_trigger_and_its_model_through_the_host() {
    let cell = cell();
    let mut host = host_of(holding(&[("information_spread_v1", Hold::Other, false, false)]));
    let trigger = event("information_contact_update", Vec::new());
    host.records.insert(text("T1"), Logged { status: EventStatus::Applied, realized_minute: 2, event: trigger.clone() });
    let reaction = event(
        "social_interaction",
        vec![("interaction_type", str_v("information_reaction")), ("trigger_event_id", str_v("T1"))],
    );
    let inputs = social_interaction::Inputs::read(&cell, &mut host, &reaction).unwrap();
    let (id, found) = inputs.trigger.unwrap();
    assert_eq!((&*id, found.map(|logged| logged.realized_minute)), ("T1", Some(2)));
    let (config, held) = inputs.information.unwrap();
    assert!(config.is_ok());
    // Not a dict: no state.
    assert_eq!(held, Held::Absent);
    assert_eq!(host.calls, [Call::FindRecord(text("T1")), Call::Holding, Call::FillSection(SectionId::InformationSpread)]);

    // The common social actions read nothing outside the world.
    let mut host = host_of(holding(&[]));
    let action = event("social_interaction", vec![("interaction_type", str_v("family_care"))]);
    let inputs = social_interaction::Inputs::read(&cell, &mut host, &action).unwrap();
    assert!(inputs.trigger.is_none() && inputs.information.is_none() && host.calls.is_empty());
}

#[test]
fn the_information_events_keep_the_config_s_fault_for_their_caller() {
    let cell = cell();
    let mut host = host_of(holding(&[("information_spread_v1", Hold::Other, false, true)]));
    host.sections.push((SectionId::InformationSpread, Box::new(|configs: &Configs| put(&configs.information_spread, Section::Fault(fault())))));
    let read = information_events::Gathered::read(&cell, &mut host).unwrap();
    assert_eq!(read.config.err(), Some(fault()));
    assert_eq!(read.held, Held::Other(information_events::OTHER_DOCUMENT));
    assert!(read.selection.is_ok());
}

#[test]
fn the_body_reads_the_selection_with_its_own_refusals() {
    let cell = cell();
    let mut host = host_of(holding(&[]));
    assert_eq!(body_state::namespace_absent(&cell, &mut host), Ok(true));
    let read = body_state::Inputs::read(&cell, &mut host).unwrap();
    assert!(read.models.is_ok());

    for (fault, text) in [(Fault::refused("odd"), body_state::NOT_A_SELECTION), (fault(), body_state::SELECTION_RAISES)] {
        let mut host = host_of(holding(&[]));
        host.sections.push((SectionId::Selection, Box::new(move |configs: &Configs| put(&configs.selection, Section::Fault(fault.clone())))));
        assert_eq!(body_state::Inputs::read(&cell, &mut host).err(), Some(unsupported(text)));
    }
    let table = Arc::new(Holding { table: false, ..Holding::default() });
    assert_eq!(body_state::namespace_absent(&cell, &mut host_of(table)).err(), Some(unsupported(TABLE_NOT_PLAIN)));
}

#[test]
fn the_care_formula_is_read_only_where_the_selection_names_a_care_model() {
    let cell = cell();
    let mut host = host_of(holding(&[]));
    assert!(care::care_formula(&cell, &mut host).unwrap().is_none());
    assert!(!host.calls.contains(&Call::FillSection(SectionId::CareOutcome)));

    let mut host = host_of(holding(&[]));
    host.sections.push((
        SectionId::Selection,
        Box::new(|configs: &Configs| {
            put(&configs.selection, Section::Ok(Arc::new(Selection::new(vec![(text("care_outcome"), text("typed-care-support-response-v1"))]))))
        }),
    ));
    host.sections.push((SectionId::CareOutcome, Box::new(|configs: &Configs| put(&configs.care_outcome, Section::Fault(fault())))));
    assert_eq!(care::care_formula(&cell, &mut host).unwrap().map(|formula| formula.err()), Some(Some(fault())));
    // A selection that raised is the care's refusal.
    let mut host = host_of(holding(&[]));
    host.sections.push((SectionId::Selection, Box::new(|configs: &Configs| put(&configs.selection, Section::Fault(fault())))));
    assert_eq!(care::care_formula(&cell, &mut host).err(), Some(unsupported(care::SELECTION_REFUSES)));
}

#[test]
fn a_section_the_configs_lack_is_refused_as_the_crate_refused_it() {
    assert_eq!(kit::kept_of::<i64>(Section::Absent).err(), Some(Fault::refused(NO_SUCH_SECTION)));
    let cell = cell();
    let mut host = host_of(holding(&[]));
    host.sections.push((SectionId::FacilityAwareness, Box::new(|configs: &Configs| put(&configs.facility_awareness, Section::Fault(fault())))));
    assert_eq!(agent_dynamics::Configs::awareness(&cell, &mut host).unwrap().err(), Some(fault()));
    assert!(agent_dynamics::Configs::policy(&cell, &mut host).unwrap().is_ok());
    assert!(agent_dynamics::Configs::knowledge_is_limited(&cell, &mut host).unwrap().is_ok());
}

// ---- The staff's operations ---------------------------------------------------------------------------

#[test]
fn the_staff_operations_keep_what_the_plan_and_the_formula_raised() {
    let cell = cell();
    let given = holding(&[
        ("staff_operations_v1", Hold::Other, false, false),
        ("movement_v1", Hold::Other, false, true),
        ("body_state_v1", Hold::Held, false, true),
    ]);
    let mut host = host_of(given.clone());
    host.sections.push((SectionId::StaffOperations, Box::new(|configs: &Configs| put(&configs.staff_operations, Section::Fault(fault())))));
    let read = staff_operation_events::Gathered::read(&cell, &mut host, true, true).unwrap();
    assert_eq!(read.staff, Held::Absent);
    assert_eq!(read.movement, Held::Other(staff_operation_events::OTHER_MOVEMENT));
    assert_eq!((read.body.clone(), read.program.clone(), read.shared_absent), (Held::Held, Held::Absent, true));
    assert_eq!(read.plan.as_ref().unwrap().as_ref().err(), Some(&unsupported(NO_PLAN)));
    assert_eq!(read.formula.as_ref().unwrap().as_ref().err(), Some(&fault()));
    let inputs = read.inputs();
    assert!(inputs.geo.is_none() && inputs.formula.is_none());
    // What an entry does not read, it does not ask.
    let mut host = host_of(given);
    let read = staff_operation_events::Gathered::read(&cell, &mut host, false, false).unwrap();
    assert!(read.plan.is_none() && read.formula.is_none());
    assert_eq!(host.calls, [Call::Holding]);
}

#[test]
fn a_geometry_event_closes_a_route_by_its_payload() {
    let closing = |op: &str, parameters: Option<V>| {
        let mut payload = vec![("op", str_v(op))];
        if let Some(parameters) = parameters {
            payload.push(("parameters", parameters));
        }
        staff_operation_events::closes_a_route(&event("effect_program_geometry", payload))
    };
    assert!(closing("set_route_access", Some(map(vec![("accessible", V::Bool(false))]))));
    assert!(!closing("set_route_access", Some(map(vec![("accessible", V::Bool(true))]))));
    // Only the bool False: not 0, not None, not a missing key.
    assert!(!closing("set_route_access", Some(map(vec![("accessible", V::Int(0))]))));
    assert!(!closing("set_route_access", Some(map(vec![("accessible", V::Null)]))));
    assert!(!closing("set_route_access", Some(map(Vec::new()))));
    assert!(!closing("set_route_access", None));
    assert!(!closing("set_route_access", Some(V::List(vec![V::Bool(false)]))));
    assert!(!closing("set_zone_access", Some(map(vec![("accessible", V::Bool(false))]))));
}

// ---- Movement ----------------------------------------------------------------------------------------------

#[test]
fn finishing_a_walk_reads_whether_there_is_a_movement_state() {
    assert_eq!(movement::finish_held(&holding(&[])), Ok(false));
    assert_eq!(movement::finish_held(&holding(&[("movement_v1", Hold::Held, false, true)])), Ok(true));
    let other = holding(&[("movement_v1", Hold::Other, false, true)]);
    assert_eq!(movement::finish_held(&other).err(), Some(unsupported(movement::OTHER_MOVEMENT)));
    let loose = holding(&[("movement_v1", Hold::NotOwned, false, true)]);
    assert_eq!(movement::check_held(&loose).err(), Some(unsupported(NOT_OWNED)));
    assert_eq!(movement::finish_held(&Holding { table: false, ..Holding::default() }).err(), Some(unsupported(TABLE_NOT_PLAIN)));
    // The floor plan is the host's: its refusal is the finish's.
    let cell = cell();
    let mut host = TestHost::default();
    assert_eq!(movement::finish_through(&cell, &mut host, "A1").err(), Some(unsupported(NO_PLAN)));
}

#[test]
fn a_bed_is_placed_on_the_row_of_a_detached_namespace_only() {
    let world = World::default();
    let held = holding(&[("movement_v1", Hold::Held, false, true)]);
    let bed = map(vec![("x", V::Float(1.0)), ("y", V::Float(2.0))]);
    let zone = text("Z1");
    // Not detached: the caller writes through the tree.
    assert_eq!(movement::place_at_bed(&world, &held, "A1", &zone, &bed), Ok(false));
    assert_eq!(movement::held_detached(&world, &holding(&[])), Ok(false));
}

#[test]
fn the_reserved_bed_is_read_as_python_reads_the_overlay() {
    let overlay = map(vec![
        ("bed_reservations", map(vec![("A1", map(vec![("room_id", str_v("R1")), ("bed_index", V::Int(1))]))])),
        ("temporary_rooms", map(vec![("R1", map(vec![("bed_points_m", V::List(vec![str_v("first"), str_v("second")]))]))])),
    ]);
    assert_eq!(movement::reserved_bed_of(Some(&overlay), "A1"), Ok(Some(str_v("second"))));
    assert_eq!(movement::reserved_bed_of(Some(&overlay), "A2"), Ok(None));
    assert_eq!(movement::reserved_bed_of(None, "A1"), Ok(None));
    assert_eq!(movement::reserved_bed_of(Some(&V::Int(1)), "A1").err(), Some(unsupported(movement::OVERLAY_NOT_A_DICT)));
    let unhashable = map(vec![("bed_reservations", map(vec![("A1", map(vec![("room_id", V::List(Vec::new()))]))]))]);
    assert_eq!(movement::reserved_bed_of(Some(&unhashable), "A1").err(), Some(unsupported("a room ID is not hashable")));
    let past = map(vec![
        ("bed_reservations", map(vec![("A1", map(vec![("room_id", str_v("R1")), ("bed_index", V::Int(5))]))])),
        ("temporary_rooms", map(vec![("R1", map(vec![("bed_points_m", V::List(Vec::new()))]))])),
    ]);
    assert_eq!(movement::reserved_bed_of(Some(&past), "A1"), Ok(None));
}

// ---- A walk's end and `_after_arrival` --------------------------------------------------------------

struct Recording {
    own: bool,
    calls: Vec<&'static str>,
    toilet: Option<Error>,
    resource: Option<String>,
}

impl travel_completion::Managers<TestHost> for Recording {
    fn own(&mut self, _toilet: bool) -> Result<bool, Error> {
        Ok(self.own)
    }

    fn toilet(&mut self, _host: &mut TestHost, _agent_id: &str, _facility_id: &str) -> Result<(), Error> {
        self.calls.push("toilet");
        match self.toilet.take() {
            Some(error) => Err(error),
            None => Ok(()),
        }
    }

    fn resource(&mut self, _host: &mut TestHost, _agent_id: &str, _facility_id: &str) -> Result<Option<String>, Error> {
        self.calls.push("resource");
        Ok(self.resource.take())
    }
}

fn arrival<'a>(purpose: &'a str, origin: Option<&'a str>, validates: bool) -> travel_completion::Arrival<'a> {
    travel_completion::Arrival {
        agent_id: "A1",
        purpose,
        facility_id: "F1",
        destination: "Z1",
        origin,
        has_geometry: false,
        resource_service: true,
        validates,
    }
}

#[test]
fn a_walk_reads_the_floor_plan_only_for_a_queue_walk() {
    let cell = cell();
    let mut host = host_of(holding(&[("movement_v1", Hold::Other, false, true)]));
    let walk = event("agent_travel_complete", vec![("purpose", str_v("toilet_request"))]);
    let read = travel_completion::Gathered::read(&cell, &mut host, &walk).unwrap();
    assert!(read.geo.is_none() && read.section.is_none());
    assert_eq!(read.movement, Held::Other(travel_completion::OTHER_MOVEMENT));
    assert_eq!(host.calls, [Call::Holding]);
    let queue_walk = event("agent_travel_complete", vec![("purpose", str_v("toilet_queue_position"))]);
    assert_eq!(travel_completion::Gathered::read(&cell, &mut host, &queue_walk).err(), Some(unsupported(NO_PLAN)));
    host.plan = Some(Arc::new(Geo::new(None, None, None, None).unwrap()));
    let read = travel_completion::Gathered::read(&cell, &mut host, &queue_walk).unwrap();
    // No layout: no movement section.
    assert!(read.geo.is_some() && read.section.is_none());
}

#[test]
fn a_toilet_request_is_validated_around_unless_the_manager_has_its_own() {
    let cell = cell();
    let mut host = TestHost::default();
    let mut managers = Recording { own: false, calls: Vec::new(), toilet: None, resource: None };
    travel_completion::arrive(&cell, &mut host, &arrival("toilet_request", None, true), &mut managers).unwrap().unwrap();
    assert_eq!(host.calls, [Call::ValidateWorld, Call::ValidateWorld]);
    assert_eq!(managers.calls, ["toilet"]);

    let mut host = TestHost::default();
    let mut managers = Recording { own: true, calls: Vec::new(), toilet: None, resource: None };
    travel_completion::arrive(&cell, &mut host, &arrival("toilet_request", None, true), &mut managers).unwrap().unwrap();
    assert!(host.calls.is_empty());

    // `except ValueError as exc: raise EventApplicationError(str(exc))`.
    let mut managers = Recording { own: false, calls: Vec::new(), toilet: Some(Error::value_error("queue full")), resource: None };
    let failed = travel_completion::arrive(&cell, &mut TestHost::default(), &arrival("toilet_request", None, false), &mut managers);
    assert_eq!(failed.err(), Some(Error::failed("queue full")));
    // Another error goes through as it is.
    let mut managers = Recording { own: false, calls: Vec::new(), toilet: Some(Error::Runtime("boom".to_owned())), resource: None };
    let raised = travel_completion::arrive(&cell, &mut TestHost::default(), &arrival("toilet_request", None, false), &mut managers);
    assert_eq!(raised.err(), Some(Error::Runtime("boom".to_owned())));
}

#[test]
fn other_walks_lead_nowhere_and_a_turned_down_resource_needs_its_origin() {
    let cell = cell();
    let mut host = TestHost::default();
    let mut managers = Recording { own: false, calls: Vec::new(), toilet: None, resource: None };
    travel_completion::arrive(&cell, &mut host, &arrival("return", None, true), &mut managers).unwrap().unwrap();
    assert!(host.calls.is_empty() && managers.calls.is_empty());

    // The resource branch checks the service's row first: a world without it refuses.
    let mut host = host_of(holding(&[("resource_service_v1", Hold::Other, false, true)]));
    let mut managers = Recording { own: false, calls: Vec::new(), toilet: None, resource: Some("full".to_owned()) };
    let refused = travel_completion::arrive(&cell, &mut host, &arrival("resource_request", None, true), &mut managers);
    assert_eq!(refused.err(), Some(unsupported(travel_completion::OTHER_SERVICE)));
    assert!(managers.calls.is_empty());
}
