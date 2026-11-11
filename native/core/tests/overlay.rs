//! Lane c4: the rules of `domain/temporary_spaces.py` and `domain/geometry_overlay.py`
//! (`rules::overlay`) on a world built here with no Python. The PyO3 crate's kernels of the same
//! names are glue over these, so the gate and the offline tests run them on every Run; these say
//! what each reads, decides and writes, and what it asks of the host (the parameters' contracts,
//! policy E15).

mod support;

use std::sync::Arc;

use indexmap::IndexMap;
use sheltersim_core::cell::Shard;
use sheltersim_core::config::geometry::FacilityPlan;
use sheltersim_core::config::service::ResourceService;
use sheltersim_core::config::SpatialConstraints;
use sheltersim_core::entities::{Activity, AgentRow, AnyTable, EdgeRow, Entities, FacilityRow, Mobility, NeedRow, ProfileRow, Table, ZoneRow};
use sheltersim_core::error::Error;
use sheltersim_core::events::{EventKind, EventRow};
use sheltersim_core::geometry::{zone_center, Geo, Pt, Route, Zone};
use sheltersim_core::host::Contract;
use sheltersim_core::row::{IdMap, ValueRow};
use sheltersim_core::rules::kit::{Held, Kept};
use sheltersim_core::rules::overlay::{self, Beds, Change, Places, OVERLAY_KEY, PLAN_UNREAD, SPACES_KEY};
use sheltersim_core::value::V;
use sheltersim_core::world::{Fit, Place, World, WorldCell};
use support::{Call, TestHost};

fn text(value: &str) -> Arc<str> {
    Arc::from(value)
}

fn s(value: &str) -> V {
    V::Str(text(value))
}

/// A `V` map of these entries, in this order.
fn map(entries: Vec<(&str, V)>) -> V {
    V::Map(entries.into_iter().map(|(key, value)| (text(key), value)).collect())
}

fn empty() -> V {
    V::Map(IndexMap::new())
}

fn value_error(text: &str) -> Error {
    Error::Value(text.to_owned())
}

fn unsupported(reason: &str) -> Error {
    Error::Unsupported(reason.to_owned().into())
}

fn put<R: Clone>(table: &mut Table<R>, id: &str, row: R) {
    let slot = table.reserve_slot();
    table.fill_slot(slot, row);
    assert_eq!(table.bind(text(id), slot), None);
}

fn needs() -> NeedRow {
    NeedRow { toilet_urgency: 0.0, hunger: 0.0, fatigue: 0.0, hydration_need: 0.0 }
}

fn agent(id: &str, zone: &str, activity: Activity, target: Option<&str>) -> AgentRow {
    AgentRow {
        profile: ProfileRow {
            id: text(id),
            group_id: None,
            initial_zone_id: text(zone),
            mobility: Mobility::Independent,
            support_need: 0.0,
            stress_sensitivity: 1.0,
            rng_key: text(id),
            initial_needs: needs(),
            provenance: V::Map(IndexMap::new()),
        },
        zone_id: text(zone),
        activity,
        target_id: target.map(text),
        needs: needs(),
        stress: 0.0,
        trust: 0.5,
        next_decision_at: 0,
        support_request_id: None,
        support_cooldown_until: 0,
    }
}

fn square(x: f64) -> Vec<Pt> {
    vec![Pt { x, y: 0.0 }, Pt { x: x + 10.0, y: 0.0 }, Pt { x: x + 10.0, y: 10.0 }, Pt { x, y: 10.0 }]
}

/// Two rooms of 10 m by 10 m side by side, z0 and z1, the routes e1 (z0 to z1) and e2 (back) drawn
/// from centre to centre, the facility f1 at (3, 3) in z0, and empty spatial constraints.
fn geo() -> Geo {
    let mut zones = IdMap::default();
    for (id, x) in [("z0", 0.0), ("z1", 10.0)] {
        let polygon = square(x);
        let center = zone_center(&polygon);
        zones.insert(text(id), Zone { polygon, usable_area: 100.0, center });
    }
    let mut routes = IdMap::default();
    for id in ["e1", "e2"] {
        routes.insert(text(id), Route { points: vec![Pt { x: 5.0, y: 5.0 }, Pt { x: 15.0, y: 5.0 }], width: 2.0, accessible: true });
    }
    let mut facility_points = IdMap::default();
    facility_points.insert(text("f1"), Pt { x: 3.0, y: 3.0 });
    let plan = FacilityPlan { zones, routes, facility_points, service_points: Vec::new() };
    Geo::new(Some(&plan), Some(&SpatialConstraints::default()), None, None).unwrap()
}

fn no_plan() -> Geo {
    Geo::new(None, None, None, None).unwrap()
}

fn overlay_state() -> V {
    map(vec![
        ("schema_version", V::Int(1)),
        ("policy_version", s("bounded-runtime-geometry-v1")),
        ("facility_points", empty()),
        ("partitions", empty()),
        ("route_modifiers", empty()),
        ("history", V::List(Vec::new())),
        ("temporary_rooms", empty()),
        ("bed_reservations", empty()),
        ("room_admissions", empty()),
    ])
}

fn spaces_state() -> V {
    map(vec![
        ("schema_version", V::Int(1)),
        ("policy_version", s("typed-temporary-capacity-v1")),
        ("spaces", empty()),
        ("history", V::List(Vec::new())),
    ])
}

/// A world of 600 minutes at minute 7: a1 idle in z0, a2 walking to f1 (in z0), the zones z0 (10
/// places) and z1 (no capacity), the edges e1 and e2, and the two namespaces.
fn world_with(overlay: V) -> World {
    let mut world = World::default();
    world.scalars.get_mut().duration_minutes = Fit::Ok(600);
    world.scalars.get_mut().minute_index = Fit::Ok(7);
    let mut entities = Entities::default();
    put(&mut entities.agents, "a1", agent("a1", "z0", Activity::Idle, None));
    put(&mut entities.agents, "a2", agent("a2", "z1", Activity::Moving, Some("f1")));
    put(&mut entities.facilities, "f1", FacilityRow {
        id: text("f1"),
        kind: text("info_desk"),
        zone_id: text("z0"),
        capacity: 1,
        working_units: 1,
        active_agent_ids: Vec::new(),
        queue_agent_ids: Vec::new(),
        resource_active_agent_ids: Vec::new(),
        resource_queue_agent_ids: Vec::new(),
    });
    for (id, capacity) in [("z0", Some(10)), ("z1", None)] {
        put(&mut entities.zones, id, ZoneRow { id: text(id), name: text(id), capacity, display_x: None, display_y: None });
    }
    for (id, from, to) in [("e1", "z0", "z1"), ("e2", "z1", "z0")] {
        put(&mut entities.zone_edges, id, EdgeRow { id: text(id), from_zone_id: text(from), to_zone_id: text(to), travel_minutes: 3, closed: false });
    }
    *world.entities.get_mut() = entities;
    for (name, value) in [(SPACES_KEY, spaces_state()), (OVERLAY_KEY, overlay)] {
        let index = world.slots.add(text(name), Place::Namespace).unwrap();
        world.slots.at(index).unwrap().try_borrow_mut().unwrap().row = Some(Box::new(ValueRow(value)));
    }
    world
}

fn world() -> World {
    world_with(overlay_state())
}

fn state(world: &World, name: &str) -> V {
    world.row::<ValueRow>(name).unwrap().0.clone()
}

fn get<'a>(value: &'a V, path: &[&str]) -> &'a V {
    path.iter().fold(value, |node, key| match node {
        V::Map(map) => map.get(*key).unwrap_or_else(|| panic!("no {key}")),
        _ => panic!("not a map at {key}"),
    })
}

fn schema(value: i64) -> Kept<Option<i64>> {
    Ok(Arc::new(Some(value)))
}

fn space(id: &str, zone: &str, people: i64) -> V {
    map(vec![
        ("space_id", s(id)),
        ("zone_id", s(zone)),
        ("people_capacity", V::Int(people)),
        ("sleeping_capacity", V::Int(0)),
        ("start_minute", V::Int(7)),
        ("end_minute", V::Int(100)),
        ("active", V::Bool(true)),
        ("start_event_id", s("ev-start")),
        ("end_event_id", V::Null),
        ("program_id", s("p1")),
        ("effect_id", s("x1")),
        ("source_ids", V::List(vec![s("SRC-1")])),
        ("geometry_status", s("zone_only_capacity")),
    ])
}

// ---- temporary spaces ----------------------------------------------------------------------------

#[test]
fn a_space_is_registered_counted_and_expired_on_the_row() {
    let world = world();
    let held = Held::Held;
    overlay::register_space(&world, &held, space("sp1", "z0", 5), "ev-start").unwrap();
    let spaces = state(&world, SPACES_KEY);
    assert_eq!(get(&spaces, &["spaces", "sp1"]), &space("sp1", "z0", 5));
    let activated = map(vec![
        ("event_id", s("ev-start")),
        ("space_id", s("sp1")),
        ("zone_id", s("z0")),
        ("minute", V::Int(7)),
        ("transition", s("activated")),
        ("causation_id", V::Null),
    ]);
    assert_eq!(get(&spaces, &["history"]), &V::List(vec![activated.clone()]));
    assert_eq!(overlay::register_space(&world, &held, space("sp1", "z0", 5), "ev-again"), Err(value_error("temporary space ID already exists")));
    assert_eq!(overlay::register_space(&world, &Held::Absent, space("sp2", "z0", 5), "ev"), Err(value_error("temporary_spaces_v1 is not installed")));
    assert_eq!(overlay::validate_spaces(&world, &held), Ok(true));

    let schema = schema(2);
    let places = Places { spaces: &held, overlay: &Held::Held, schema: &schema };
    let found = overlay::capacity(&world, &places, "z0", 10, Some(12)).unwrap();
    assert_eq!((found.effective, found.active_people, found.expired_people, found.retained), (15, 5, 0, 0));
    assert_eq!(overlay::zone_capacity(&world, &places, "z1", None).unwrap(), None);
    assert_eq!(overlay::zone_capacity(&world, &places, "z9", None), Err(unsupported("no such zone")));

    assert_eq!(overlay::expire_space(&world, &held, "sp1", "ev-end", Some("ev-start")), Ok(true));
    assert_eq!(overlay::expire_space(&world, &held, "sp1", "ev-end", None), Ok(false));
    assert_eq!(overlay::expire_space(&world, &held, "sp9", "ev-end", None), Ok(false));
    let spaces = state(&world, SPACES_KEY);
    assert_eq!((get(&spaces, &["spaces", "sp1", "active"]), get(&spaces, &["spaces", "sp1", "end_event_id"])), (&V::Bool(false), &s("ev-end")));
    let V::List(history) = get(&spaces, &["history"]) else { panic!() };
    assert_eq!((history.len(), get(&history[1], &["transition"]), get(&history[1], &["causation_id"])), (2, &s("expired"), &s("ev-start")));
    // An expired space keeps the people it holds beyond the nominal capacity.
    let found = overlay::capacity(&world, &places, "z0", 10, Some(18)).unwrap();
    assert_eq!((found.effective, found.active_people, found.expired_people, found.retained), (15, 0, 5, 5));
    assert_eq!(found.total(), Ok(15));
    // The same from the values a world outside the core holds.
    let (spaces, overlay_value) = (state(&world, SPACES_KEY), state(&world, OVERLAY_KEY));
    assert_eq!(overlay::capacity_of_values(Some(&spaces), Some(&overlay_value), &schema, "z0", 10, 18), Ok(found));
}

#[test]
fn the_people_moving_to_a_zone_are_counted_by_their_target() {
    let world = world();
    assert_eq!(overlay::incoming_travelers(&world, "z0", None), Ok(1));
    assert_eq!(overlay::incoming_travelers(&world, "z0", Some("a2")), Ok(0));
    assert_eq!(overlay::incoming_travelers(&world, "z1", None), Ok(0));
}

// ---- beds ----------------------------------------------------------------------------------------

#[test]
fn a_release_writes_the_row_only_when_the_person_held_something() {
    let mut overlay_value = overlay_state();
    let V::Map(fields) = &mut overlay_value else { panic!() };
    fields.insert(text("bed_reservations"), map(vec![("a1", map(vec![("room_id", s("r1"))]))]));
    let mut world = world_with(overlay_value);
    world.slots.take_written();
    assert_eq!(overlay::release(&world, &Held::Held, "bed_reservations", Some("a2")), Ok(false));
    assert!(world.slots.written_names().is_empty());
    assert_eq!(overlay::release(&world, &Held::Held, "bed_reservations", None), Err(unsupported("a value is not a str")));
    assert_eq!(overlay::release(&world, &Held::Held, "bed_reservations", Some("a1")), Ok(true));
    assert_eq!(world.slots.written_names(), [text(OVERLAY_KEY)]);
    assert_eq!(get(&state(&world, OVERLAY_KEY), &["bed_reservations"]), &empty());
    assert_eq!(overlay::release(&world, &Held::Absent, "bed_reservations", Some("a1")), Ok(false));
}

#[test]
fn a_bed_plan_gives_an_older_state_its_empty_reservations_first() {
    let mut older = overlay_state();
    let V::Map(fields) = &mut older else { panic!() };
    fields.shift_remove("bed_reservations");
    let world = world_with(older);
    let (two, walking) = (schema(2), Err(sheltersim_core::config::Fault::refused("not read")));
    let plan = no_plan();
    let beds = Beds { overlay: &Held::Held, movement: &Held::Absent, schema: &two, walking: &walking, geo: Some(&plan) };
    // No floor plan: no bed, but the reservations are there now, last, as `setdefault` puts them.
    assert_eq!(overlay::plan_bed(&world, &beds, "a1"), Ok(None));
    let V::Map(fields) = state(&world, OVERLAY_KEY) else { panic!() };
    assert_eq!(fields.keys().last().map(|key| &**key), Some("bed_reservations"));
    // A plan the PyO3 crate could not read is said where the rule reads it.
    let unread = Beds { geo: None, ..beds };
    assert_eq!(overlay::plan_bed(&world, &unread, "a1"), Err(unsupported(PLAN_UNREAD)));
    // An unknown person, or a schema before beds, has none and reads no plan.
    assert_eq!(overlay::plan_bed(&world, &unread, "a9"), Ok(None));
    let one = schema(1);
    assert_eq!(overlay::plan_bed(&world, &Beds { schema: &one, ..unread }, "a1"), Ok(None));
}

#[test]
fn an_installed_facility_without_a_point_raises_the_rule_text() {
    let world = world();
    let geo = geo();
    assert_eq!(
        overlay::place_facility(&world, &Held::Held, Some(&geo), "f9", "ev1", &[]),
        Err(value_error("no free floor-plan point for installed facility 'f9'"))
    );
    assert_eq!(overlay::placement_enabled(&schema(4), &Held::Held), Ok(true));
    assert_eq!(overlay::placement_enabled(&schema(4), &Held::Absent), Ok(false));
    assert_eq!(overlay::placement_enabled(&schema(3), &Held::Held), Ok(false));
    assert!(overlay::placement_enabled(&schema(4), &Held::Other("another overlay")).is_err());
}

// ---- geometry changes -------------------------------------------------------------------------------

fn no_service() -> Kept<ResourceService> {
    Ok(Arc::new(ResourceService::parse(&empty()).unwrap()))
}

fn change<'a>(geo: Option<&'a Geo>, service: &'a Kept<ResourceService>) -> Change<'a> {
    Change { overlay: &Held::Held, movement: &Held::Absent, staff_operations: &Held::Absent, resource_service: &Held::Absent, service, geo }
}

fn event(op: &str, target: &str, parameters: V, end_minute: Option<i64>) -> EventRow {
    EventRow {
        event_id: text("ev-geo"),
        kind: EventKind::Intervention,
        due_minute: 7,
        priority: 0,
        sequence: 1,
        event_type: text("effect_program_geometry"),
        payload: map(vec![
            ("program_id", s("p1")),
            ("effect_id", s("x1")),
            ("modifier_id", s("m1")),
            ("target_id", s(target)),
            ("op", s(op)),
            ("parameters", parameters),
            ("end_minute", end_minute.map_or(V::Null, V::Int)),
        ]),
        source_id: Some(text("SRC-B")),
        causation_id: Some(text("ev-cause")),
        provenance: map(vec![("effect_program_v1", map(vec![("assumption_ids", V::List(vec![s("ASM-A")]))]))]),
    }
}

#[test]
fn a_route_closure_writes_the_overlay_and_the_edge_row() {
    let cell = Shard::new(world());
    let (geo, service) = (geo(), no_service());
    let parameters = map(vec![("accessible", V::Bool(false)), ("source_ids", V::List(vec![s("SRC-A")]))]);
    let mut host = TestHost::default();
    overlay::apply_change(&cell, &mut host, &change(Some(&geo), &service), &event("set_route_access", "e1", parameters.clone(), Some(50)), false).unwrap();
    assert_eq!(host.calls, [Call::Contract(Contract::RouteAccess, parameters.clone())]);
    cell.with(|world| {
        assert!(world.entities.try_borrow().unwrap().zone_edges.get("e1").unwrap().closed);
        let overlay = state(world, OVERLAY_KEY);
        let modifier = map(vec![
            ("accessible", V::Bool(false)),
            ("event_id", s("ev-geo")),
            ("source_ids", V::List(vec![s("ASM-A"), s("SRC-A"), s("SRC-B")])),
            ("minute", V::Int(7)),
            ("end_minute", V::Int(50)),
        ]);
        assert_eq!(get(&overlay, &["route_modifiers", "e1"]), &map(vec![("base_closed", V::Bool(false)), ("modifiers", map(vec![("m1", modifier)]))]));
        let V::List(history) = get(&overlay, &["history"]) else { panic!() };
        assert_eq!((get(&history[0], &["status"]), get(&history[0], &["causation_id"]), get(&history[0], &["target_id"])), (&s("applied"), &s("ev-cause"), &s("e1")));
    })
    .unwrap();
    // The expiry pops the modifier, drops the route's record and opens the edge again, asking no
    // contract.
    let mut host = TestHost::default();
    overlay::apply_change(&cell, &mut host, &change(Some(&geo), &service), &event("set_route_access", "e1", parameters, Some(50)), true).unwrap();
    assert!(host.calls.is_empty());
    cell.with(|world| {
        assert!(!world.entities.try_borrow().unwrap().zone_edges.get("e1").unwrap().closed);
        let overlay = state(world, OVERLAY_KEY);
        assert_eq!(get(&overlay, &["route_modifiers"]), &empty());
        let V::List(history) = get(&overlay, &["history"]) else { panic!() };
        assert_eq!(get(&history[1], &["status"]), &s("expired"));
    })
    .unwrap();
}

#[test]
fn a_transfer_writes_the_facility_zone_on_its_row() {
    let cell = Shard::new(world());
    let (geo, service) = (geo(), no_service());
    let parameters = map(vec![
        ("schema_version", V::Int(1)),
        ("equipment_id", s("f1")),
        ("from_zone_id", s("z0")),
        ("to_zone_id", s("z1")),
        ("point_m", map(vec![("x", V::Float(15.0)), ("y", V::Float(3.0))])),
        ("source_ids", V::List(Vec::new())),
    ]);
    let mut host = TestHost::default();
    overlay::apply_change(&cell, &mut host, &change(Some(&geo), &service), &event("transfer_equipment", "f1", parameters.clone(), None), false).unwrap();
    // One contract call: the change writes what the check validated.
    assert_eq!(host.calls, [Call::Contract(Contract::EquipmentTransfer, parameters)]);
    cell.with(|world| {
        assert_eq!(&*world.rows().unwrap().facilities.get("f1").unwrap().zone_id, "z1");
        let overlay = state(world, OVERLAY_KEY);
        assert_eq!(get(&overlay, &["facility_points", "f1", "point_m"]), &map(vec![("x", V::Float(15.0)), ("y", V::Float(3.0))]));
        assert_eq!(get(&overlay, &["facility_points", "f1", "source_ids"]), &V::List(vec![s("ASM-A"), s("SRC-B")]));
    })
    .unwrap();
}

#[test]
fn the_checks_raise_the_rule_texts_in_their_order() {
    let (geo, service) = (geo(), no_service());
    let check = |cell: &Shard<World>, change: &Change<'_>, op: &str, target: &str, end: Option<i64>| {
        let mut host = TestHost::default();
        let result = overlay::check_change(cell, &mut host, change, op, target, &map(vec![("accessible", V::Bool(false))]), end).map(drop);
        (result, host.calls.len())
    };
    let cell = Shard::new(world());
    let absent = Change { overlay: &Held::Absent, ..change(Some(&geo), &service) };
    assert_eq!(
        check(&cell, &absent, "set_route_access", "e1", Some(5)),
        (Err(value_error("geometry_overlay_required: declare the versioned geometry overlay before approving effects")), 0)
    );
    let plain = no_plan();
    assert_eq!(check(&cell, &change(Some(&plain), &service), "set_route_access", "e1", Some(5)), (Err(value_error("geometry_unavailable")), 0));
    assert_eq!(check(&cell, &change(None, &service), "set_route_access", "e1", Some(5)), (Err(unsupported(PLAN_UNREAD)), 0));
    assert_eq!(check(&cell, &change(Some(&geo), &service), "rotate", "e1", Some(5)), (Err(value_error("unsupported_geometry_operator")), 0));
    // The contract is asked before the operator's own checks.
    assert_eq!(check(&cell, &change(Some(&geo), &service), "set_route_access", "e9", Some(5)), (Err(value_error("target_missing: route does not exist")), 1));
    assert_eq!(
        check(&cell, &change(Some(&geo), &service), "set_route_access", "e1", None),
        (Err(value_error("expiry_required: route-access changes require a bounded end")), 1)
    );
    assert_eq!(check(&cell, &change(Some(&geo), &service), "set_route_access", "e1", Some(5)), (Ok(()), 1));
}

#[test]
fn the_overlay_state_passes_its_checks_until_the_graph_disagrees() {
    let geo = geo();
    let mut world = world();
    assert_eq!(overlay::check_overlay(&world, &Held::Held, Some(&geo)), Ok(()));
    assert_eq!(overlay::check_overlay(&world, &Held::Absent, None), Ok(()));
    let modifier = map(vec![
        ("accessible", V::Bool(false)),
        ("event_id", s("ev1")),
        ("source_ids", V::List(Vec::new())),
        ("minute", V::Int(1)),
        ("end_minute", V::Int(50)),
    ]);
    let mut value = overlay_state();
    let V::Map(fields) = &mut value else { panic!() };
    fields.insert(text("route_modifiers"), map(vec![("e1", map(vec![("base_closed", V::Bool(false)), ("modifiers", map(vec![("m1", modifier)]))]))]));
    world.slots.row_mut::<ValueRow>(OVERLAY_KEY).unwrap().0 = value;
    assert_eq!(overlay::check_overlay(&world, &Held::Held, Some(&geo)), Err(unsupported("the overlay and the graph disagree")));
    world.entities.get_mut().zone_edges.get_mut("e1").unwrap().closed = true;
    assert_eq!(overlay::check_overlay(&world, &Held::Held, Some(&geo)), Ok(()));
}
