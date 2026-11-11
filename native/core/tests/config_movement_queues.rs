//! The sections of lane 7-5a, parsed from the configs as a `V`: movement, routing, the queue
//! policy, the overlay's schema, the resource service and the population's needs. Each gives the
//! value the kernels read, or the refusal (or the constructor's `ValueError`) they said when they
//! read the Python tree, in the same order of checks.

use std::sync::Arc;

use indexmap::IndexMap;
use serde_json::{json, Value};
use sheltersim_core::config::movement::{Movement, MovementModel, RouteCapacities, EXACT_INT};
use sheltersim_core::config::overlay::overlay_schema;
use sheltersim_core::config::population::AccessibilityNeeds;
use sheltersim_core::config::queues::{QueueConfig, QueuePolicy, RETRY_DELAY};
use sheltersim_core::config::read::Read;
use sheltersim_core::config::service::{ResourceService, ServiceSkills, NOT_A_MAPPING};
use sheltersim_core::config::{Fault, FaultClass};
use sheltersim_core::value::V;

fn v(value: Value) -> V {
    match value {
        Value::Null => V::Null,
        Value::Bool(flag) => V::Bool(flag),
        Value::Number(number) => match number.as_i64() {
            Some(int) if !number.to_string().contains(['.', 'e', 'E']) => V::Int(int),
            _ if !number.to_string().contains(['.', 'e', 'E']) => V::Big(number.to_string().into()),
            _ => V::Float(number.as_f64().expect("a float")),
        },
        Value::String(text) => V::Str(Arc::from(text)),
        Value::Array(items) => V::List(items.into_iter().map(v).collect()),
        Value::Object(map) => V::Map(map.into_iter().map(|(key, item)| (Arc::from(key), v(item))).collect::<IndexMap<_, _>>()),
    }
}

/// A map in the order given (`json!` sorts its keys).
fn ordered(items: Vec<(&str, Value)>) -> V {
    V::Map(items.into_iter().map(|(key, item)| (Arc::from(key), v(item))).collect())
}

fn refused<T: std::fmt::Debug>(read: Read<T>) -> String {
    let fault = read.expect_err("a refusal");
    assert!(fault.is_refusal(), "{fault:?}");
    fault.text.to_string()
}

fn value_error<T: std::fmt::Debug>(read: Read<T>) -> String {
    let fault = read.expect_err("a ValueError");
    assert_eq!(fault.class, FaultClass::Value, "{fault:?}");
    fault.text.to_string()
}

fn model(raw: Value) -> Read<MovementModel> {
    MovementModel::parse(Some(&v(raw)))
}

#[test]
fn the_movement_model_and_its_refusals_in_the_order_of_its_keys() {
    let defaults = MovementModel::default();
    assert_eq!(MovementModel::parse(None), Ok(defaults));
    assert_eq!(model(json!(null)), Ok(defaults));
    assert_eq!(model(json!({})), Ok(defaults));
    let full = model(json!({
        "schema_version": 1,
        "policy_version": "spatial-route-and-room-waypoint-v2",
        "source_id": "ASM-SPATIAL-MOVEMENT-V2",
        "speed_m_per_minute_by_mobility": {"independent": 60, "assisted": 30.5, "wheelchair": 40},
        "density_slowdown_per_person_m2": 0,
        "maximum_density_slowdown": 3.0,
    }))
    .unwrap();
    assert_eq!((full.independent, full.assisted, full.wheelchair), (60.0, 30.5, 40.0));
    assert_eq!((full.density_slowdown, full.maximum_slowdown), (0.0, 3.0));
    assert_eq!((full.speed("independent"), full.speed("assisted"), full.speed("anything")), (60.0, 30.5, 40.0));

    let speeds = |table: Value| model(json!({"speed_m_per_minute_by_mobility": table}));
    assert_eq!(refused(model(json!([]))), "a value is not a plain dict");
    assert_eq!(refused(model(json!({"schema_version": 2}))), "another movement schema");
    assert_eq!(refused(model(json!({"schema_version": true}))), "a value is not an int");
    assert_eq!(refused(model(json!({"policy_version": "x"}))), "another movement policy");
    assert_eq!(refused(model(json!({"policy_version": 1}))), "a value is not a str");
    assert_eq!(refused(model(json!({"source_id": "x"}))), "another movement source");
    assert_eq!(refused(speeds(json!({"independent": 1, "assisted": 1}))), "the speed table has other fields");
    assert_eq!(refused(speeds(json!({"independent": 1, "assisted": 1, "other": 1}))), "a speed is missing");
    assert_eq!(refused(speeds(json!({"independent": 0, "assisted": 1, "wheelchair": 1}))), "a speed is not positive");
    assert_eq!(refused(speeds(json!({"independent": true, "assisted": 1, "wheelchair": 1}))), "a value is not a float");
    assert_eq!(refused(speeds(json!({"independent": "1", "assisted": 1, "wheelchair": 1}))), "a value is not a float");
    assert_eq!(refused(speeds(json!({"independent": EXACT_INT, "assisted": 1, "wheelchair": 1}))), "a coefficient is too large for a float");
    assert_eq!(
        refused(speeds(json!({"independent": 18446744073709551615u64, "assisted": 1, "wheelchair": 1}))),
        "an int is too large"
    );
    assert_eq!(refused(model(json!({"density_slowdown_per_person_m2": -0.1}))), "a slowdown is not a plain value");
    assert_eq!(refused(model(json!({"maximum_density_slowdown": 0.5}))), "a slowdown is not a plain value");
    assert_eq!(refused(model(json!({"other": 1}))), "the movement config has other fields");
    // The first key that fails is the refusal: the dict's order.
    let first = |items| refused(MovementModel::parse(Some(&ordered(items))));
    assert_eq!(first(vec![("other", json!(1)), ("schema_version", json!(2))]), "the movement config has other fields");
    assert_eq!(first(vec![("schema_version", json!(2)), ("other", json!(1))]), "another movement schema");
}

#[test]
fn the_movement_section_keeps_the_model_s_refusal_for_where_a_walk_is_timed() {
    assert_eq!(refused(Movement::parse(&v(json!([])))), "a value is not a plain dict");
    let none = Movement::parse(&v(json!({}))).unwrap();
    assert!(!none.has_model && !none.has_geometry && !none.times_routes());
    assert_eq!(none.model, Ok(MovementModel::default()));
    // A key holding None times routes with the defaults.
    let null = Movement::parse(&v(json!({"movement_model": null}))).unwrap();
    assert!(null.has_model && null.times_routes());
    assert_eq!(null.model, Ok(MovementModel::default()));
    let geometry = Movement::parse(&v(json!({"facility_geometry": null}))).unwrap();
    assert!(!geometry.has_model && geometry.has_geometry && geometry.times_routes());
    let bad = Movement::parse(&v(json!({"movement_model": {"schema_version": 3}}))).unwrap();
    assert!(bad.has_model);
    assert_eq!(refused(bad.model), "another movement schema");
}

#[test]
fn the_route_capacities_up_to_the_edges() {
    let parse = |configs: Value| RouteCapacities::parse(&v(configs));
    assert_eq!(parse(json!({})), Ok(None));
    assert_eq!(parse(json!({"layout_routing": null})), Ok(None));
    assert_eq!(refused(parse(json!(1))), "a value is not a plain dict");
    assert_eq!(refused(parse(json!({"layout_routing": []}))), "a value is not a plain dict");
    let config = |extra: Value| {
        let mut config = json!({
            "schema_version": 1,
            "source_type": "assumption",
            "source_id": "ASM-A05-ROUTE-EDIT-V1",
            "max_agents_in_transit_by_edge": {"e1": 3, "e2": "x", "e3": 1e3},
        });
        for (key, value) in extra.as_object().unwrap() {
            if value.is_null() {
                config.as_object_mut().unwrap().remove(key);
            } else {
                config[key] = value.clone();
            }
        }
        json!({"layout_routing": config})
    };
    let routing = parse(config(json!({}))).unwrap().unwrap();
    let edges: Vec<&str> = routing.capacities.iter().map(|(edge, _)| &**edge).collect();
    assert_eq!(edges, ["e1", "e2", "e3"]);
    assert_eq!(routing.capacities[0].1, Ok(3));
    assert_eq!(refused(routing.capacities[1].1.clone()), "a value is not an int");
    assert_eq!(refused(routing.capacities[2].1.clone()), "a value is not an int");
    assert_eq!(refused(parse(config(json!({"max_agents_in_transit_by_edge": null})))), "the route config lacks its capacities");
    for extra in [
        json!({"schema_version": true}),
        json!({"schema_version": 1.0}),
        json!({"source_type": "survey"}),
        json!({"source_id": 5}),
        json!({"other": 1}),
    ] {
        assert_eq!(refused(parse(config(extra))), "the route config is the Python rule's");
    }
    assert_eq!(refused(parse(config(json!({"max_agents_in_transit_by_edge": [1]})))), "a value is not a plain dict");
}

fn queues(configs: Value) -> QueueConfig {
    QueueConfig::parse(&v(configs)).unwrap()
}

#[test]
fn the_queue_policies_and_the_errors_of_their_constructors() {
    assert_eq!(refused(QueueConfig::parse(&v(json!([])))), "a value is not a plain dict");
    let none = queues(json!({}));
    assert!(!none.has_geometry);
    assert_eq!(none.strict, Ok(QueuePolicy::Strict));
    assert_eq!(
        none.need_weighted,
        Ok(QueuePolicy::NeedWeighted { limits: [120, 240, 360], threshold: 0.8, retry: RETRY_DELAY })
    );
    assert!(queues(json!({"facility_geometry": {}})).has_geometry);
    let given = queues(json!({"facility_queues": {
        "schema_version": 1,
        "abandon_after_minutes_by_service": {"meal": 30, "toilet": 10},
        "retry_delay_minutes": 5,
        "persistence_urgency_threshold": 1,
    }}));
    assert_eq!(given.strict, Ok(QueuePolicy::Strict));
    assert_eq!(given.need_weighted, Ok(QueuePolicy::NeedWeighted { limits: [10, 240, 30], threshold: 1.0, retry: 5 }));
    assert_eq!(given.policy(false), &given.strict);

    // None passes the checks, and neither policy reads it.
    let null = queues(json!({"facility_queues": null}));
    assert_eq!(refused(null.strict), "the queue config is not a dict");
    assert_eq!(refused(null.need_weighted), "the queue config is not a dict");
    // Not a dict: both constructors raise.
    let listed = queues(json!({"facility_queues": [1]}));
    assert_eq!(value_error(listed.strict), "facility_queues config must be an object");
    assert_eq!(value_error(listed.need_weighted), "facility_queues config must be an object");

    let weighted = |config: Value| queues(json!({"facility_queues": config})).need_weighted;
    assert_eq!(value_error(weighted(json!({"other": 1}))), "facility_queues config must be an object");
    assert_eq!(value_error(weighted(json!({"schema_version": 2}))), "facility_queues requires schema_version 1");
    // Python's `!=`: 1.0 and True are 1.
    assert!(weighted(json!({"schema_version": 1.0})).is_ok());
    assert!(weighted(json!({"schema_version": true})).is_ok());
    assert_eq!(value_error(weighted(json!({"schema_version": "1"}))), "facility_queues requires schema_version 1");
    let unsupported = "queue abandonment limits contain an unsupported service";
    assert_eq!(value_error(weighted(json!({"abandon_after_minutes_by_service": []}))), unsupported);
    assert_eq!(value_error(weighted(json!({"abandon_after_minutes_by_service": {"bath": 1}}))), unsupported);
    let limits = "queue abandonment limits must be within 1..10080 minutes";
    assert_eq!(value_error(weighted(json!({"abandon_after_minutes_by_service": {"meal": 0}}))), limits);
    assert_eq!(value_error(weighted(json!({"abandon_after_minutes_by_service": {"meal": true}}))), limits);
    assert_eq!(value_error(weighted(json!({"abandon_after_minutes_by_service": {"meal": 5.0}}))), limits);
    assert_eq!(
        refused(weighted(json!({"abandon_after_minutes_by_service": {"meal": 18446744073709551615u64}}))),
        "an int is too large"
    );
    let retry = "queue retry delay must be between 1 and 1440 minutes";
    assert_eq!(value_error(weighted(json!({"retry_delay_minutes": 1441}))), retry);
    let threshold = "queue persistence threshold must be normalized to 0..1";
    assert_eq!(value_error(weighted(json!({"persistence_urgency_threshold": 1.5}))), threshold);
    assert_eq!(value_error(weighted(json!({"persistence_urgency_threshold": false}))), threshold);
    assert_eq!(refused(weighted(json!({"persistence_urgency_threshold": 9007199254740993u64}))), "a number does not fit a float");
    assert_eq!(weighted(json!({"persistence_urgency_threshold": 0})).unwrap(), QueuePolicy::NeedWeighted { limits: [120, 240, 360], threshold: 0.0, retry: 15 });
    // The strict policy reads no field of a dict.
    assert_eq!(queues(json!({"facility_queues": {"other": 1}})).strict, Ok(QueuePolicy::Strict));
}

#[test]
fn the_overlay_schema_the_service_and_the_needs() {
    assert_eq!(overlay_schema(&v(json!({}))), Ok(None));
    assert_eq!(overlay_schema(&v(json!({"geometry_overlay": [3]}))), Ok(None));
    assert_eq!(overlay_schema(&v(json!({"geometry_overlay": {"schema_version": null}}))), Ok(None));
    assert_eq!(overlay_schema(&v(json!({"geometry_overlay": {"schema_version": 4}}))), Ok(Some(4)));
    assert_eq!(refused(overlay_schema(&v(json!({"geometry_overlay": {"schema_version": 4.0}})))), "a value is not an int");
    assert_eq!(refused(overlay_schema(&v(json!(null)))), "a value is not a plain dict");

    let service = ResourceService::parse(&v(json!({}))).unwrap();
    assert!(!service.present && service.config.is_none());
    assert_eq!(service.skills, Ok(ServiceSkills::default()));
    let config = ordered(vec![("water_staff_skill", json!("water")), ("meal_staff_skill", json!(3)), ("x", json!(1))]);
    let service = ResourceService::parse(&V::Map(IndexMap::from([(Arc::from("resource_service"), config)]))).unwrap();
    assert!(service.present);
    let keys: Vec<&str> = service.config.as_ref().unwrap().keys().map(|key| &**key).collect();
    assert_eq!(keys, ["water_staff_skill", "meal_staff_skill", "x"]);
    assert_eq!(service.skills, Ok(ServiceSkills { water: Some(Arc::from("water")), meal: None }));
    let service = ResourceService::parse(&v(json!({"resource_service": [["a", 1]]}))).unwrap();
    assert!(service.present && service.config.is_none());
    assert_eq!(service.skills, Err(Fault::refused(NOT_A_MAPPING)));

    let needs = AccessibilityNeeds::parse(&v(json!({"population": {"people": {
        "a1": {"accessibility_needs": ["wheelchair", "quiet"]},
        "a2": {},
        "a3": {"accessibility_needs": "wheelchair"},
        "a4": {"accessibility_needs": [1]},
        "a5": [],
    }}}))).unwrap();
    let names = |read: Read<&[Arc<str>]>| read.map(|needs| needs.iter().map(|need| need.to_string()).collect::<Vec<_>>());
    assert_eq!(names(needs.of("a1")), Ok(vec!["wheelchair".to_owned(), "quiet".to_owned()]));
    assert_eq!(names(needs.of("a2")), Ok(vec![]));
    assert_eq!(names(needs.of("nobody")), Ok(vec![]));
    assert_eq!(refused(needs.of("a3")), "the needs are not a list");
    assert_eq!(refused(needs.of("a4")), "a need is not a str");
    assert_eq!(refused(needs.of("a5")), "a value is not a plain dict");
    assert!(AccessibilityNeeds::parse(&v(json!({"population": {}}))).is_ok());
    assert_eq!(refused(AccessibilityNeeds::parse(&v(json!({"population": {"people": []}})))), "a value is not a plain dict");
    assert_eq!(refused(AccessibilityNeeds::parse(&v(json!({"population": null})))), "a value is not a plain dict");
}
