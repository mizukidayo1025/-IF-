//! The fixtures of the core's tests: worlds the Python side exported, read back without Python
//! (Issue #225, lane F2; the floor plan of #214).
//!
//! `tests/data/sections.json` holds a generated Scenario's world (`core_fixtures.sections_world`:
//! the UI's floor plan with three people, ten ticks through `TickEngine`, then a facility moved and
//! a partition set in its overlay by hand) and what the native path read of that world: the floor
//! plan the movement rules read (`geo_summary_of`), the approach routes, whether queues are
//! independent, and the sections as their read sites read them (`config_section`). The host
//! `load::host` makes from the export must give the same: its Python-parsed sections are the
//! world's (`support/sections.rs`), and its floor plan is `Geo::of` of the world. `json_dumps_*`
//! checks the writer of Python's `json.dumps` text (`support/dumps.rs`) against Python's bytes.

mod support;

use std::sync::Arc;

use indexmap::IndexMap;
use serde_json::Value;
use sheltersim_core::config::body::{AffectModel, TrustModel};
use sheltersim_core::config::{Configs, FaultClass, Section, SectionId};
use sheltersim_core::geometry::Pt;
use sheltersim_core::host::Host;
use sheltersim_core::value::V;

use support::dumps::{dumps, Dumps};
use support::{load, sections};

fn entry() -> Value {
    load::entry("sections", "generated_moved")
}

/// The configs a host filled with one section, as a rule's reader asks it.
fn filled(entry: &Value, which: SectionId) -> Arc<Configs> {
    let mut host = load::host(entry);
    let configs = Arc::new(Configs::new(None));
    host.fill_section(&configs, which).unwrap();
    configs
}

fn ok<T>(section: Option<&Section<T>>) -> Arc<T> {
    match section {
        Some(Section::Ok(value)) => value.clone(),
        Some(Section::Fault(fault)) => panic!("a fault: {fault:?}"),
        _ => panic!("not filled"),
    }
}

fn pt(value: &Value) -> Pt {
    Pt { x: value[0].as_f64().unwrap(), y: value[1].as_f64().unwrap() }
}

fn point_list(value: &Value) -> Vec<(String, Pt)> {
    value.as_array().unwrap().iter().map(|pair| (pair[0].as_str().unwrap().to_owned(), pt(&pair[1]))).collect()
}

#[test]
fn an_exported_world_s_sections_are_what_their_read_sites_read() {
    let entry = entry();
    let expected = &entry["expected"]["config_sections"];
    let value = |name: &str| &expected[name]["value"];

    let operations = ok(filled(&entry, SectionId::StaffOperations).staff_operations.get());
    let (source, policy, event_source) = operations.provenance();
    let wanted = value("staff_operations");
    assert_eq!(operations.travel_first(), wanted[0].as_bool().unwrap());
    assert_eq!([source, policy, event_source], [wanted[1].as_str().unwrap(), wanted[2].as_str().unwrap(), wanted[3].as_str().unwrap()]);

    let cooldown = ok(filled(&entry, SectionId::SupportCooldownMinutes).support_cooldown_minutes.get());
    assert_eq!(*cooldown, value("support_cooldown_minutes").as_i64().unwrap());

    let spread = ok(filled(&entry, SectionId::InformationSpread).information_spread.get());
    assert_eq!(spread.query_pressure_per_claim.clone().unwrap(), value("information_spread").as_f64().unwrap());

    let body = ok(filled(&entry, SectionId::BodyModels).body_models.get());
    assert_eq!(value("body_models")[0], "linear-positive-negative-v1");
    assert_eq!(body.affect, AffectModel::Linear);
    assert_eq!(value("body_models")[1], "distance-duration-repeat-trust-v1");
    assert!(matches!(body.trust, TrustModel::DistanceDurationRepeat { .. }));

    let care = ok(filled(&entry, SectionId::CareOutcome).care_outcome.get());
    assert_eq!([&*care.model_id, &*care.source_id], [value("care_outcome")[0].as_str().unwrap(), value("care_outcome")[1].as_str().unwrap()]);

    let environment = ok(filled(&entry, SectionId::ZoneEnvironment).zone_environment.get());
    let wanted = value("zone_environment");
    assert_eq!([&*environment.policy_version, &*environment.source_id], [wanted[0].as_str().unwrap(), wanted[1].as_str().unwrap()]);

    // The configs have no infrastructure: the parse raised `KeyError('infrastructure')`, and the read
    // site raises it again.
    assert_eq!(expected["infrastructure"]["raises"], serde_json::json!(["KeyError", "'infrastructure'"]));
    match filled(&entry, SectionId::Infrastructure).infrastructure.get() {
        Some(Section::Fault(fault)) => assert_eq!((&fault.class, &*fault.text), (&FaultClass::Key, "'infrastructure'")),
        _ => panic!("the section is not the parse's fault"),
    }

    assert!(value("proximity").is_null());
    assert!(ok(filled(&entry, SectionId::Proximity).proximity.get()).is_none());
    assert_eq!(*ok(filled(&entry, SectionId::TransitCapacities).transit_capacities.get()), value("transit_capacities").as_bool().unwrap());
    let rooms = ok(filled(&entry, SectionId::OverlayRooms).overlay_rooms.get());
    assert_eq!([rooms.is_dict, rooms.room_schema.clone().unwrap()], [value("overlay_rooms")[0].as_bool().unwrap(), value("overlay_rooms")[1].as_bool().unwrap()]);
    assert_eq!(*ok(filled(&entry, SectionId::ExtensionsEnabled).extensions_enabled.get()), value("extensions_enabled").as_bool().unwrap());

    // The model selection and the plan of the end-of-tick check, which no read site returns.
    let selection = ok(filled(&entry, SectionId::Selection).selection.get());
    assert_eq!(selection.get("relationship").map(|model| &**model), Some("distance-duration-repeat-trust-v1"));
    let plan = ok(filled(&entry, SectionId::TickPlan).tick_plan.get());
    assert!(plan.information_enabled && plan.shared_configured && plan.resource_config.is_some());
    assert!(plan.required.iter().any(|name| &**name == "movement_v1"));
}

#[test]
fn an_exported_world_s_floor_plan_is_the_native_path_s() {
    let entry = entry();
    let expected = &entry["expected"];
    let wanted = &expected["floor_plan"];
    let mut host = load::host(&entry);
    let geo = host.floor_plan().unwrap();
    let layout = geo.layout.as_ref().expect("a layout");

    // The facility moved in the overlay is where it was moved to, and its service point with it.
    let mut points: Vec<(String, Pt)> = layout.facility_points.iter().map(|(id, point)| (id.to_string(), *point)).collect();
    let mut wanted_points = point_list(&wanted["facility_points"]);
    // The points' order follows the Scenario's dict, which the export does not keep (no rule reads it).
    points.sort_by(|first, second| first.0.cmp(&second.0));
    wanted_points.sort_by(|first, second| first.0.cmp(&second.0));
    assert_eq!(points, wanted_points);
    let services: Vec<(String, Pt)> = layout.service_points.iter().map(|point| (point.id.to_string(), point.point)).collect();
    assert_eq!(services, point_list(&wanted["service_points"]));

    let centers: IndexMap<String, Option<Pt>> =
        wanted["zone_centers"].as_object().unwrap().iter().map(|(id, center)| (id.clone(), (!center.is_null()).then(|| pt(center)))).collect();
    assert_eq!(centers.len(), layout.zones.len());
    for (id, center) in &centers {
        assert_eq!(geo.center(id).unwrap(), *center, "{id}");
    }

    // The partition set in the overlay blocks routes, as on the native path.
    let mut clear = Vec::new();
    for mobility in ["independent", "assisted", "wheelchair"] {
        for id in layout.routes.keys() {
            clear.push(serde_json::json!([mobility, &**id, geo.route_is_clear(id, mobility)]));
        }
    }
    assert_eq!(Value::Array(clear), wanted["routes_clear"]);
    assert!(wanted["routes_clear"].as_array().unwrap().iter().any(|item| item[2] == false));

    let constraints = geo.constraints.as_ref().expect("constraints");
    assert_eq!(constraints.obstacle_count as u64, wanted["obstacle_count"].as_u64().unwrap());
    let blocked: Vec<Vec<Pt>> = wanted["blocked"].as_array().unwrap().iter().map(|polygon| polygon.as_array().unwrap().iter().map(pt).collect()).collect();
    assert_eq!(constraints.blocked, blocked);

    // The facility geometry's other parts, as their read sites read them.
    let configs = filled(&entry, SectionId::FacilityGeometry);
    let geometry = ok(configs.facility_geometry.get());
    let routes: Vec<Value> = geometry
        .approach_routes
        .clone()
        .unwrap()
        .iter()
        .map(|route| serde_json::json!([&*route.route_id, route.travel_minutes.clone().unwrap()]))
        .collect();
    assert_eq!(Value::Array(routes), expected["approach_routes"]);
    assert_eq!(geometry.independent_queues.clone().unwrap(), expected["independent_queues"].as_bool().unwrap());
}

#[test]
fn a_world_without_a_facility_geometry_has_an_empty_floor_plan() {
    // The small world of the tick test: no geometry, no constraints, no overlay.
    let fixture = load::fixture();
    let geo = sections::floor_plan(&fixture).unwrap();
    assert!(geo.layout.is_none() && geo.constraints.is_none());
    assert!(matches!(sections::section(&fixture, SectionId::FacilityGeometry).facility_geometry.get(), Some(Section::Absent)));
}

// ---- Python's json.dumps ------------------------------------------------------------------------------

fn s(text: &str) -> V {
    V::Str(Arc::from(text))
}

fn m(items: Vec<(&str, V)>) -> V {
    V::Map(items.into_iter().map(|(key, value)| (Arc::from(key), value)).collect())
}

/// `{"b": 1, "a": [...], "日本": "...", "": {}, "z": [[], {"y": 2, "x": [3]}]}`, in this order.
fn sample() -> V {
    let numbers = vec![
        V::Float(1.5),
        V::Float(1e-05),
        V::Float(1e16),
        V::Float(123456789.125),
        V::Float(-0.0),
        V::Float(f64::NAN),
        V::Float(f64::INFINITY),
        V::Float(f64::NEG_INFINITY),
        V::Big("12345678901234567890".into()),
        V::Int(-7),
        V::Bool(true),
        V::Bool(false),
        V::Null,
        V::Float(0.1),
        V::Float(100.0),
    ];
    m(vec![
        ("b", V::Int(1)),
        ("a", V::List(numbers)),
        ("日本", s("é😀\"\\\n\t\r\u{8}\u{c}\u{7f}\u{1}/")),
        ("", m(vec![])),
        ("z", V::List(vec![V::List(vec![]), m(vec![("y", V::Int(2)), ("x", V::List(vec![V::Int(3)]))])])),
    ])
}

/// What Python 3.12 wrote for `sample()`, with each set of options.
#[test]
fn json_dumps_writes_python_s_text() {
    let value = sample();
    assert_eq!(
        dumps(&value),
        "{\"b\": 1, \"a\": [1.5, 1e-05, 1e+16, 123456789.125, -0.0, NaN, Infinity, -Infinity, 12345678901234567890, -7, true, false, null, 0.1, 100.0], \"\\u65e5\\u672c\": \"\\u00e9\\ud83d\\ude00\\\"\\\\\\n\\t\\r\\b\\f\\u007f\\u0001/\", \"\": {}, \"z\": [[], {\"y\": 2, \"x\": [3]}]}"
    );
    assert_eq!(
        Dumps::default().sorted().text(&value),
        "{\"\": {}, \"a\": [1.5, 1e-05, 1e+16, 123456789.125, -0.0, NaN, Infinity, -Infinity, 12345678901234567890, -7, true, false, null, 0.1, 100.0], \"b\": 1, \"z\": [[], {\"x\": [3], \"y\": 2}], \"\\u65e5\\u672c\": \"\\u00e9\\ud83d\\ude00\\\"\\\\\\n\\t\\r\\b\\f\\u007f\\u0001/\"}"
    );
    assert_eq!(
        Dumps::default().unicode().text(&value),
        "{\"b\": 1, \"a\": [1.5, 1e-05, 1e+16, 123456789.125, -0.0, NaN, Infinity, -Infinity, 12345678901234567890, -7, true, false, null, 0.1, 100.0], \"日本\": \"é😀\\\"\\\\\\n\\t\\r\\b\\f\u{7f}\\u0001/\", \"\": {}, \"z\": [[], {\"y\": 2, \"x\": [3]}]}"
    );
    assert_eq!(
        Dumps::default().unicode().compact().sorted().text(&value),
        "{\"\":{},\"a\":[1.5,1e-05,1e+16,123456789.125,-0.0,NaN,Infinity,-Infinity,12345678901234567890,-7,true,false,null,0.1,100.0],\"b\":1,\"z\":[[],{\"x\":[3],\"y\":2}],\"日本\":\"é😀\\\"\\\\\\n\\t\\r\\b\\f\u{7f}\\u0001/\"}"
    );
}
