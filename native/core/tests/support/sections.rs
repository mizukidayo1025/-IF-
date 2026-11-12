//! The Python-parsed sections and the floor plan of an exported world (Issue #225, lane F2; #214).
//!
//! `tests/support/core_fixtures.py` writes, for each world of a fixture, what the PyO3 crate's
//! `PyHost::fill_section` reads from Python for each section whose parser is the host's
//! (`SectionId::parser() == Parser::Host`): the model selection, the plan of the end-of-tick check,
//! the parsed `FacilityGeometry` and `SpatialConstraints` (as JSON, after their contracts), the
//! formulas' IDs, the validated specs and the values read in Python. `fill` turns each into the
//! core's value as the PyO3 crate does (`native/src/core/configs.rs`, `geometry.rs`, and
//! `rules/after_tick.rs`'s `Plan`), with the same refusals, and gives it to the `TestHost`, which
//! fills the section with it when a rule asks. The sections the core parses from the configs' `V`
//! are not here: the core parses them from the world's `model_configs` row.
//!
//! `floor_plan` is `Geo::of` of the PyO3 crate on the same world: the facility geometry's plan, the
//! spatial constraints where the configs have them or `model_state` has the overlay, and the
//! overlay's facility moves and partitions where they have entries. It is built once, from the world
//! as exported; a test that changes the overlay builds it again (`plan_of`).

use std::sync::Arc;

use rustc_hash::FxHashSet;
use serde_json::Value;
use sheltersim_core::config::body::{BodyModels, CareOutcome};
use sheltersim_core::config::environment::{Infrastructure, ZoneEnvironmentSpec};
use sheltersim_core::config::information::InformationSpread;
use sheltersim_core::config::overlay::OverlayRooms;
use sheltersim_core::config::proximity::Proximity;
use sheltersim_core::config::staff::StaffOperations;
use sheltersim_core::config::tick_plan::{ResourceNumbers, TickPlan};
use sheltersim_core::config::{
    ApproachRoute, Configs, FacilityGeometry, FacilityPlan, Fault, FaultClass, Lazy, Parser, Section, SectionId, Selection,
    SpatialConstraints,
};
use sheltersim_core::error::Error;
use sheltersim_core::geometry::{usable, zone_center, Geo, Opening, OverlayKey, Pt, Route, ServicePoint, Wall, Zone};
use sheltersim_core::row::IdMap;
use sheltersim_core::value::V;

use super::load::{fault, v};
use super::TestHost;

/// The overlay's namespace (`GEOMETRY_OVERLAY_KEY`).
const OVERLAY: &str = "geometry_overlay_v1";

fn text(value: &str) -> Arc<str> {
    Arc::from(value)
}

/// What the PyO3 crate's `unsupported` gives where a part of a parsed model is read: `fault_of` an
/// `Unsupported`, which is not a refusal there.
fn unsupported(reason: &str) -> Fault {
    Fault::new(FaultClass::Other(text("sheltersim_native.Unsupported")), reason)
}

/// A section as exported: its value, the fault of its parse, absent, or what the PyO3 crate raises
/// out of `fill_section` (the test panics with it when a rule asks).
enum Exported<'a> {
    Value(&'a Value),
    Fault(Fault),
    Absent,
    HostError(String),
}

fn exported(section: &Value) -> Exported<'_> {
    if let Some(value) = section.get("value") {
        Exported::Value(value)
    } else if let Some(found) = section.get("fault") {
        Exported::Fault(fault(found))
    } else if section.get("absent").is_some() {
        Exported::Absent
    } else {
        Exported::HostError(section["host_error"].as_str().expect("a section as `core_fixtures.py` writes it").to_owned())
    }
}

/// The section of an exported value, made into the core's value by `read`; a value `read` refuses
/// is the section's fault, as the PyO3 crate's `read` and `parsed` keep it.
fn section_of<T>(exported: Exported<'_>, read: impl FnOnce(&Value) -> Result<T, Fault>) -> Result<Section<T>, String> {
    match exported {
        Exported::Value(value) => Ok(read(value).into()),
        Exported::Fault(fault) => Ok(Section::Fault(fault)),
        Exported::Absent => Ok(Section::Absent),
        Exported::HostError(text) => Err(text),
    }
}

type Fill = Box<dyn Fn(&Configs)>;

/// The closure that fills one section with what was exported.
fn filler<T: Send + Sync + 'static>(get: fn(&Configs) -> &Lazy<T>, section: Result<Section<T>, String>, which: SectionId) -> Fill {
    Box::new(move |configs| match &section {
        Ok(section) => {
            let _ = get(configs).get_or_parse(|| section.clone());
        }
        Err(error) => panic!("the PyO3 crate raises out of fill_section({which:?}): {error}"),
    })
}

fn texts(value: &Value) -> Vec<Arc<str>> {
    value.as_array().expect("a list").iter().map(|item| text(item.as_str().expect("a str"))).collect()
}

fn plain_text(value: &Value) -> Result<Arc<str>, Fault> {
    value.as_str().map(text).ok_or_else(|| Fault::refused("a value is not a str"))
}

// ---- The sections ----------------------------------------------------------------------------------

fn selection(value: &Value) -> Result<Selection, Fault> {
    let pairs = value.as_array().expect("a list of pairs");
    Ok(Selection::new(pairs.iter().map(|pair| (text(pair[0].as_str().unwrap()), text(pair[1].as_str().unwrap()))).collect()))
}

/// `Plan.__new__` of `rules/after_tick.rs`, from `tick_plan_fields`.
fn tick_plan(value: &Value) -> Result<TickPlan, Fault> {
    let flag = |name: &str| value[name].as_bool().unwrap_or_else(|| panic!("{name} is not a bool"));
    let set = |name: &str| texts(&value[name]).into_iter().collect::<FxHashSet<_>>();
    let resource = &value["resource_config"];
    let resource_config = if resource.is_null() {
        None
    } else {
        Some(ResourceNumbers {
            water_ml_per_service: resource["water_ml_per_service"].as_i64().unwrap(),
            food_meal_units_per_service: resource["food_meal_units_per_service"].as_i64().unwrap(),
            water_staff_skill: text(resource["water_staff_skill"].as_str().unwrap()),
            meal_staff_skill: text(resource["meal_staff_skill"].as_str().unwrap()),
        })
    };
    Ok(TickPlan {
        declared: value["declared"].as_array().unwrap().iter().map(|item| (text(item[0].as_str().unwrap()), item[1].as_i64().unwrap())).collect(),
        required: texts(&value["required"]),
        independent_queues: flag("independent_queues"),
        information_enabled: flag("information_enabled"),
        mean_reverting: flag("mean_reverting"),
        shared_configured: flag("shared_configured"),
        operational_enabled: value["operational_enabled"].as_bool(),
        environment_zones: value["environment_zones"].as_array().map(|_| texts(&value["environment_zones"])),
        infrastructure_configured: flag("infrastructure_configured"),
        operational_events: set("operational_events"),
        infrastructure_events: set("infrastructure_events"),
        resource_config,
    })
}

// ---- The section `geometry`: the parsed models, read as `core/geometry.rs` reads them ----------------

fn float(value: &Value) -> Result<f64, Fault> {
    match v(value) {
        V::Float(number) => Ok(number),
        _ => Err(unsupported("a value is not a float")),
    }
}

fn point(value: &Value) -> Result<Pt, Fault> {
    let (x, y) = (float(&value["x"])?, float(&value["y"])?);
    if !usable(x, y) {
        return Err(unsupported("a coordinate is too large"));
    }
    Ok(Pt { x, y })
}

fn points(value: &Value) -> Result<Vec<Pt>, Fault> {
    value.as_array().expect("a list of points").iter().map(point).collect()
}

fn service_point(value: &Value) -> Result<ServicePoint, Fault> {
    let access = &value["access"];
    Ok(ServicePoint {
        id: text(value["id"].as_str().unwrap()),
        facility_id: text(value["facility_id"].as_str().unwrap()),
        point: point(&value["point_m"])?,
        service_capacity: value["service_capacity"].as_i64().unwrap(),
        queue_id: text(value["queue_id"].as_str().unwrap()),
        accessibility: text(access["accessibility"].as_str().unwrap()),
        required_access_tags: texts(&access["required_access_tags"]),
        required_staff_skills: texts(&access["required_staff_skills"]),
    })
}

/// `plan_of`: the Scenario's plan of a parsed `FacilityGeometry`.
fn facility_plan(value: &Value) -> Result<FacilityPlan, Fault> {
    let mut plan = FacilityPlan { zones: IdMap::default(), routes: IdMap::default(), facility_points: IdMap::default(), service_points: Vec::new() };
    for zone in value["zones"].as_array().unwrap() {
        let polygon = points(&zone["polygon"])?;
        if polygon.is_empty() {
            return Err(unsupported("a zone has no polygon"));
        }
        let usable_area = float(&zone["usable_area_m2"])?;
        let center = zone_center(&polygon);
        plan.zones.entry(text(zone["zone_id"].as_str().unwrap())).or_insert(Zone { polygon, usable_area, center });
    }
    for route in value["routes"].as_array().unwrap() {
        let points = points(&route["points"])?;
        if points.is_empty() {
            return Err(unsupported("a route has no points"));
        }
        let width = float(&route["width_m"])?;
        let accessible = route["accessible"].as_bool().unwrap();
        plan.routes.entry(text(route["edge_id"].as_str().unwrap())).or_insert(Route { points, width, accessible });
    }
    for (id, place) in value["facility_points"].as_object().unwrap() {
        plan.facility_points.insert(text(id), point(place)?);
    }
    for item in value["service_points"].as_array().unwrap() {
        plan.service_points.push(service_point(item)?);
    }
    Ok(plan)
}

fn approach_route(value: &Value) -> Result<ApproachRoute, Fault> {
    let minutes = &value["travel_minutes"];
    let travel_minutes = match minutes.get("fault") {
        Some(found) => Err(fault(found)),
        None => Ok(minutes["value"].as_i64().unwrap()),
    };
    Ok(ApproachRoute {
        route_id: text(value["route_id"].as_str().unwrap()),
        destination_zone_id: text(value["destination_zone_id"].as_str().unwrap()),
        source_ids: texts(&value["source_ids"]),
        walking_speed: float(&value["walking_speed_m_per_min"])?,
        distance: float(&value["distance_m"])?,
        travel_minutes,
    })
}

/// `independent_of`: a service point whose queue is not its facility's ID, or a facility with two.
fn independent(value: &Value) -> Result<bool, Fault> {
    let mut facilities = std::collections::HashSet::new();
    for item in value["service_points"].as_array().unwrap() {
        let facility = item["facility_id"].as_str().unwrap();
        if item["queue_id"].as_str().unwrap() != facility || !facilities.insert(facility.to_owned()) {
            return Ok(true);
        }
    }
    Ok(false)
}

fn facility_geometry(value: &Value) -> Result<FacilityGeometry, Fault> {
    Ok(FacilityGeometry {
        plan: facility_plan(value).map(Arc::new),
        approach_routes: value["arrival_approach_routes"].as_array().unwrap().iter().map(approach_route).collect::<Result<Vec<_>, _>>().map(Arc::from),
        independent_queues: independent(value),
    })
}

/// `spatial_of`.
fn spatial_constraints(value: &Value) -> Result<SpatialConstraints, Fault> {
    let mut spatial = SpatialConstraints::default();
    for item in value["obstacles"].as_array().unwrap() {
        let polygon = points(&item["polygon"])?;
        if polygon.is_empty() {
            return Err(unsupported("an obstacle has no polygon"));
        }
        spatial.obstacles.push(polygon);
    }
    for item in value["equipment_footprints"].as_array().unwrap() {
        let polygon = points(&item["polygon"])?;
        if polygon.is_empty() {
            return Err(unsupported("a footprint has no polygon"));
        }
        spatial.footprints.push((text(item["facility_id"].as_str().unwrap()), polygon));
    }
    for wall in value["walls"].as_array().unwrap() {
        spatial.walls.push(Wall { id: text(wall["id"].as_str().unwrap()), start: point(&wall["start"])?, end: point(&wall["end"])? });
    }
    for opening in value["openings"].as_array().unwrap() {
        spatial.openings.push(Opening {
            wall_id: text(opening["wall_id"].as_str().unwrap()),
            center: point(&opening["center"])?,
            width: float(&opening["width_m"])?,
            accessible: opening["accessible"].as_bool().unwrap(),
            edge_ids: texts(&opening["edge_ids"]),
        });
    }
    Ok(spatial)
}

// ---- Each section, filled ---------------------------------------------------------------------------

/// The filler of one section of an exported world, or None where the export has no such section.
fn fill_of(sections: &Value, configs: &V, which: SectionId) -> Option<Fill> {
    let section = exported(sections.get(which.name())?);
    Some(match which {
        SectionId::Selection => filler(|c| &c.selection, section_of(section, selection), which),
        SectionId::TickPlan => filler(|c| &c.tick_plan, section_of(section, tick_plan), which),
        SectionId::FacilityGeometry => filler(|c| &c.facility_geometry, section_of(section, facility_geometry), which),
        SectionId::SpatialConstraints => filler(|c| &c.spatial_constraints, section_of(section, spatial_constraints), which),
        SectionId::StaffOperations => {
            let read = |value: &Value| {
                Ok(StaffOperations { model_id: plain_text(&value["model_id"])?, selected_source: value["selected_source"].as_str().map(text) })
            };
            filler(|c| &c.staff_operations, section_of(section, read), which)
        }
        SectionId::SupportCooldownMinutes => {
            filler(|c| &c.support_cooldown_minutes, section_of(section, |value| sheltersim_core::config::read::int(&v(value))), which)
        }
        SectionId::InformationSpread => filler(|c| &c.information_spread, section_of(section, |value| InformationSpread::from_v(&v(value))), which),
        SectionId::BodyModels => {
            let read = |value: &Value| BodyModels::parse(value["affect"].as_str().unwrap(), value["relationship"].as_str(), configs);
            filler(|c| &c.body_models, section_of(section, read), which)
        }
        SectionId::CareOutcome => {
            let read = |value: &Value| CareOutcome::new(plain_text(&value["model_id"])?, plain_text(&value["source_id"])?, v(&value["config"]));
            filler(|c| &c.care_outcome, section_of(section, read), which)
        }
        SectionId::ZoneEnvironment => filler(|c| &c.zone_environment, section_of(section, |value| ZoneEnvironmentSpec::from_v(&v(value))), which),
        SectionId::Infrastructure => filler(|c| &c.infrastructure, section_of(section, |value| Infrastructure::from_v(&v(value))), which),
        SectionId::Proximity => filler(|c| &c.proximity, section_of(section, |value| Proximity::from_v(&v(value))), which),
        SectionId::TransitCapacities => filler(|c| &c.transit_capacities, section_of(section, |value| Ok(value.as_bool().unwrap())), which),
        SectionId::OverlayRooms => {
            let read = |value: &Value| {
                let schema = &value["room_schema"];
                let room_schema = match schema.get("fault") {
                    Some(found) => Err(fault(found)),
                    None => Ok(schema["value"].as_bool().unwrap()),
                };
                Ok(OverlayRooms { is_dict: value["is_dict"].as_bool().unwrap(), room_schema })
            };
            filler(|c| &c.overlay_rooms, section_of(section, read), which)
        }
        SectionId::ExtensionsEnabled => filler(|c| &c.extensions_enabled, section_of(section, |value| Ok(value.as_bool().unwrap())), which),
        // The core parses the others from the configs' `V`.
        _ => return None,
    })
}

/// Give the host every Python-parsed section of an exported world (`entry`): it fills each with the
/// world's value when a rule asks. Each section the core parses itself is left to the core.
pub fn fill(host: &mut TestHost, entry: &Value) {
    let sections = &entry["sections"];
    assert!(sections.is_object(), "the world was exported without its sections");
    let configs = v(&entry["world"]["model_configs"]);
    for &which in SectionId::ALL {
        if which.parser() != Parser::Host {
            continue;
        }
        let fill = fill_of(sections, &configs, which).unwrap_or_else(|| panic!("the export has no section {}", which.name()));
        host.sections.push((which, fill));
    }
}

/// One section of an exported world, as the host fills it: in configs of its own.
pub fn section(entry: &Value, which: SectionId) -> Arc<Configs> {
    let configs = Arc::new(Configs::new(None));
    let fill = fill_of(&entry["sections"], &v(&entry["world"]["model_configs"]), which).unwrap_or_else(|| panic!("no section {}", which.name()));
    fill(&configs);
    configs
}

// ---- The floor plan ---------------------------------------------------------------------------------

/// `Geo::of` of an exported world: the plan of its facility geometry and spatial constraints with
/// `overlay`'s facility moves and partitions (the world's `geometry_overlay_v1`; None without it).
/// An error is the refusal `Geo::of` raises.
pub fn plan_of(entry: &Value, overlay: Option<&V>) -> Result<Arc<Geo>, Error> {
    let configs = Arc::new(Configs::new(None));
    let sections = &entry["sections"];
    let raw = entry["world"]["model_configs"].as_object().expect("the configs are a dict");
    let refusal = |fault: &Fault| {
        if fault.is_refusal() {
            Error::unsupported(fault.text.to_string())
        } else {
            Error::unsupported("the Python rule's parse raises")
        }
    };
    fill_of(sections, &V::Null, SectionId::FacilityGeometry).expect("a facility geometry section")(&configs);
    let facility = match configs.facility_geometry.get().unwrap() {
        Section::Absent => None,
        Section::Ok(geometry) => Some(geometry.plan.clone().map_err(|fault| refusal(&fault))?),
        Section::Fault(fault) => return Err(refusal(fault)),
    };
    let spatial = if raw.contains_key("spatial_constraints") || overlay.is_some() {
        fill_of(sections, &V::Null, SectionId::SpatialConstraints).expect("a spatial constraints section")(&configs);
        match configs.spatial_constraints.get().unwrap() {
            Section::Ok(spatial) => Some(spatial.clone()),
            Section::Absent => None,
            Section::Fault(fault) => return Err(refusal(fault)),
        }
    } else {
        None
    };
    let key = OverlayKey::of(overlay, None)?;
    let (moves, partitions) = key.parts(overlay);
    Ok(Arc::new(Geo::new(facility.as_deref(), spatial.as_deref(), moves.as_ref(), partitions.as_ref())?))
}

/// `plan_of` the world as exported.
pub fn floor_plan(entry: &Value) -> Result<Arc<Geo>, Error> {
    let overlay = entry["world"]["model_state"].get(OVERLAY).map(v);
    plan_of(entry, overlay.as_ref())
}

