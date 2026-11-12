//! The Python-parsed sections and the floor plan of an exported world (#225 lane F2, #214).
//!
//! The exporter writes, for each section whose parser is the host's
//! (`SectionId::parser() == Parser::Host`), what the PyO3 crate's `PyHost::fill_section` reads from
//! Python: the model selection, the plan of the end-of-tick check, the parsed `FacilityGeometry`
//! and `SpatialConstraints` (as JSON, after their contracts), the formulas' IDs, the validated specs
//! and the values read in Python. `Sections::of_entry` turns each into the core's value as the PyO3
//! crate does (`native/src/core/configs.rs`, `geometry.rs`, and `rules/after_tick.rs`'s `Plan`),
//! with the same faults, and `Sections::fill` puts it in the configs when a rule asks the host
//! (`Host::fill_section`). The sections the core parses from the configs' `V` are not here: the core
//! parses them from the world's `model_configs` row.
//!
//! `Sections::floor_plan` is `Geo::of` of the PyO3 crate on the same world: the facility geometry's
//! plan, the spatial constraints where the configs have them or `model_state` has the overlay, and
//! the overlay's facility moves and partitions where they have entries. `FloorPlans` keeps the plan
//! last built and builds it again when the overlay's facility moves or partitions change (its
//! `OverlayKey`), as `Host::floor_plan` must answer at each call.

use std::collections::HashSet;
use std::sync::Arc;

use rustc_hash::FxHashSet;
use serde_json::Value;

use super::{array, bad, bool_of, fault, int_of, object, str_of, text, texts, value};
use crate::config::body::{BodyModels, CareOutcome};
use crate::config::environment::{Infrastructure, ZoneEnvironmentSpec};
use crate::config::information::InformationSpread;
use crate::config::overlay::OverlayRooms;
use crate::config::proximity::Proximity;
use crate::config::read::Read;
use crate::config::staff::StaffOperations;
use crate::config::tick_plan::{ResourceNumbers, TickPlan};
use crate::config::{
    ApproachRoute, Configs, FacilityGeometry, FacilityPlan, Fault, FaultClass, Lazy, Section, SectionId, Selection, SpatialConstraints,
};
use crate::error::{Error, Result};
use crate::geometry::{usable, zone_center, Geo, Opening, OverlayKey, Pt, Route, ServicePoint, Wall, Zone};
use crate::row::IdMap;
use crate::value::V;
use crate::world::World;

/// The overlay's namespace (`GEOMETRY_OVERLAY_KEY`).
pub const OVERLAY: &str = "geometry_overlay_v1";

/// The value of a reader, or the fault it keeps: a JSON shape the exporter never writes is the
/// outer error.
type Parsed<T> = Result<Read<T>>;

/// The value of a `Parsed`, or the fault returned from the reader that asked for it.
macro_rules! read {
    ($parsed:expr) => {
        match $parsed? {
            Ok(value) => value,
            Err(fault) => return Ok(Err(fault)),
        }
    };
}

/// What the PyO3 crate's `unsupported` gives where a part of a parsed model is read: `fault_of` an
/// `Unsupported`, which is not a refusal there.
fn unsupported(reason: &str) -> Fault {
    Fault::new(FaultClass::Other(text("sheltersim_native.Unsupported")), reason)
}

fn plain_text(json: &Value) -> Read<Arc<str>> {
    json.as_str().map(text).ok_or_else(|| Fault::refused("a value is not a str"))
}

// ---- The sections ----------------------------------------------------------------------------------

fn selection(json: &Value) -> Parsed<Selection> {
    let mut pairs = Vec::new();
    for pair in array(json, "the selection")? {
        pairs.push((text(str_of(&pair[0], "a selected domain")?), text(str_of(&pair[1], "a selected model")?)));
    }
    Ok(Ok(Selection::new(pairs)))
}

/// `Plan.__new__` of `rules/after_tick.rs`, from `tick_plan_fields`.
fn tick_plan(json: &Value) -> Parsed<TickPlan> {
    let flag = |name: &str| bool_of(&json[name], name);
    let set = |name: &str| texts(&json[name], name).map(|items| items.into_iter().collect::<FxHashSet<_>>());
    let resource = &json["resource_config"];
    let resource_config = if resource.is_null() {
        None
    } else {
        Some(ResourceNumbers {
            water_ml_per_service: int_of(&resource["water_ml_per_service"], "water_ml_per_service")?,
            food_meal_units_per_service: int_of(&resource["food_meal_units_per_service"], "food_meal_units_per_service")?,
            water_staff_skill: text(str_of(&resource["water_staff_skill"], "water_staff_skill")?),
            meal_staff_skill: text(str_of(&resource["meal_staff_skill"], "meal_staff_skill")?),
        })
    };
    let mut declared = Vec::new();
    for item in array(&json["declared"], "declared")? {
        declared.push((text(str_of(&item[0], "a declared namespace")?), int_of(&item[1], "a declared version")?));
    }
    let environment_zones = match &json["environment_zones"] {
        Value::Array(_) => Some(texts(&json["environment_zones"], "environment_zones")?),
        _ => None,
    };
    Ok(Ok(TickPlan {
        declared,
        required: texts(&json["required"], "required")?,
        independent_queues: flag("independent_queues")?,
        information_enabled: flag("information_enabled")?,
        mean_reverting: flag("mean_reverting")?,
        shared_configured: flag("shared_configured")?,
        operational_enabled: json["operational_enabled"].as_bool(),
        environment_zones,
        infrastructure_configured: flag("infrastructure_configured")?,
        operational_events: set("operational_events")?,
        infrastructure_events: set("infrastructure_events")?,
        resource_config,
    }))
}

// ---- The section `geometry`: the parsed models, read as `core/geometry.rs` reads them ----------------

fn float(json: &Value) -> Parsed<f64> {
    Ok(match value(json)? {
        V::Float(number) => Ok(number),
        _ => Err(unsupported("a value is not a float")),
    })
}

fn point(json: &Value) -> Parsed<Pt> {
    let (x, y) = (read!(float(&json["x"])), read!(float(&json["y"])));
    if !usable(x, y) {
        return Ok(Err(unsupported("a coordinate is too large")));
    }
    Ok(Ok(Pt { x, y }))
}

fn points(json: &Value) -> Parsed<Vec<Pt>> {
    let mut points = Vec::new();
    for item in array(json, "a list of points")? {
        points.push(read!(point(item)));
    }
    Ok(Ok(points))
}

fn service_point(json: &Value) -> Parsed<ServicePoint> {
    let access = &json["access"];
    let id = text(str_of(&json["id"], "a service point's id")?);
    let facility_id = text(str_of(&json["facility_id"], "a service point's facility_id")?);
    let point = read!(point(&json["point_m"]));
    Ok(Ok(ServicePoint {
        id,
        facility_id,
        point,
        service_capacity: int_of(&json["service_capacity"], "service_capacity")?,
        queue_id: text(str_of(&json["queue_id"], "queue_id")?),
        accessibility: text(str_of(&access["accessibility"], "accessibility")?),
        required_access_tags: texts(&access["required_access_tags"], "required_access_tags")?,
        required_staff_skills: texts(&access["required_staff_skills"], "required_staff_skills")?,
    }))
}

/// `plan_of`: the Scenario's plan of a parsed `FacilityGeometry`.
fn facility_plan(json: &Value) -> Parsed<FacilityPlan> {
    let mut plan =
        FacilityPlan { zones: IdMap::default(), routes: IdMap::default(), facility_points: IdMap::default(), service_points: Vec::new() };
    for zone in array(&json["zones"], "zones")? {
        let polygon = read!(points(&zone["polygon"]));
        if polygon.is_empty() {
            return Ok(Err(unsupported("a zone has no polygon")));
        }
        let usable_area = read!(float(&zone["usable_area_m2"]));
        let center = zone_center(&polygon);
        plan.zones.entry(text(str_of(&zone["zone_id"], "a zone's zone_id")?)).or_insert(Zone { polygon, usable_area, center });
    }
    for route in array(&json["routes"], "routes")? {
        let points = read!(points(&route["points"]));
        if points.is_empty() {
            return Ok(Err(unsupported("a route has no points")));
        }
        let width = read!(float(&route["width_m"]));
        let accessible = bool_of(&route["accessible"], "a route's accessible")?;
        plan.routes.entry(text(str_of(&route["edge_id"], "a route's edge_id")?)).or_insert(Route { points, width, accessible });
    }
    for (id, place) in object(&json["facility_points"], "facility_points")? {
        plan.facility_points.insert(text(id), read!(point(place)));
    }
    for item in array(&json["service_points"], "service_points")? {
        plan.service_points.push(read!(service_point(item)));
    }
    Ok(Ok(plan))
}

fn approach_route(json: &Value) -> Parsed<ApproachRoute> {
    let minutes = &json["travel_minutes"];
    let travel_minutes = match minutes.get("fault") {
        Some(found) => Err(fault(found)?),
        None => Ok(int_of(&minutes["value"], "travel_minutes")?),
    };
    let route_id = text(str_of(&json["route_id"], "route_id")?);
    let destination_zone_id = text(str_of(&json["destination_zone_id"], "destination_zone_id")?);
    let source_ids = texts(&json["source_ids"], "source_ids")?;
    Ok(Ok(ApproachRoute {
        route_id,
        destination_zone_id,
        source_ids,
        walking_speed: read!(float(&json["walking_speed_m_per_min"])),
        distance: read!(float(&json["distance_m"])),
        travel_minutes,
    }))
}

/// `independent_of`: a service point whose queue is not its facility's ID, or a facility with two.
fn independent(json: &Value) -> Result<bool> {
    let mut facilities = HashSet::new();
    for item in array(&json["service_points"], "service_points")? {
        let facility = str_of(&item["facility_id"], "a service point's facility_id")?;
        if str_of(&item["queue_id"], "a service point's queue_id")? != facility || !facilities.insert(facility.to_owned()) {
            return Ok(true);
        }
    }
    Ok(false)
}

fn facility_geometry(json: &Value) -> Parsed<FacilityGeometry> {
    let plan = facility_plan(json)?.map(Arc::new);
    let mut routes = Vec::new();
    let mut fault = None;
    for item in array(&json["arrival_approach_routes"], "arrival_approach_routes")? {
        match approach_route(item)? {
            Ok(route) => routes.push(route),
            Err(found) => {
                fault = Some(found);
                break;
            }
        }
    }
    let approach_routes = match fault {
        Some(fault) => Err(fault),
        None => Ok(Arc::from(routes)),
    };
    Ok(Ok(FacilityGeometry { plan, approach_routes, independent_queues: Ok(independent(json)?) }))
}

/// `spatial_of`.
fn spatial_constraints(json: &Value) -> Parsed<SpatialConstraints> {
    let mut spatial = SpatialConstraints::default();
    for item in array(&json["obstacles"], "obstacles")? {
        let polygon = read!(points(&item["polygon"]));
        if polygon.is_empty() {
            return Ok(Err(unsupported("an obstacle has no polygon")));
        }
        spatial.obstacles.push(polygon);
    }
    for item in array(&json["equipment_footprints"], "equipment_footprints")? {
        let polygon = read!(points(&item["polygon"]));
        if polygon.is_empty() {
            return Ok(Err(unsupported("a footprint has no polygon")));
        }
        spatial.footprints.push((text(str_of(&item["facility_id"], "a footprint's facility_id")?), polygon));
    }
    for wall in array(&json["walls"], "walls")? {
        let id = text(str_of(&wall["id"], "a wall's id")?);
        spatial.walls.push(Wall { id, start: read!(point(&wall["start"])), end: read!(point(&wall["end"])) });
    }
    for opening in array(&json["openings"], "openings")? {
        let wall_id = text(str_of(&opening["wall_id"], "an opening's wall_id")?);
        let center = read!(point(&opening["center"]));
        let width = read!(float(&opening["width_m"]));
        spatial.openings.push(Opening {
            wall_id,
            center,
            width,
            accessible: bool_of(&opening["accessible"], "an opening's accessible")?,
            edge_ids: texts(&opening["edge_ids"], "an opening's edge_ids")?,
        });
    }
    Ok(Ok(spatial))
}

fn staff_operations(json: &Value) -> Parsed<StaffOperations> {
    let model_id = match plain_text(&json["model_id"]) {
        Ok(model_id) => model_id,
        Err(fault) => return Ok(Err(fault)),
    };
    Ok(Ok(StaffOperations { model_id, selected_source: json["selected_source"].as_str().map(text) }))
}

fn care_outcome(json: &Value) -> Parsed<CareOutcome> {
    let model_id = match plain_text(&json["model_id"]) {
        Ok(model_id) => model_id,
        Err(fault) => return Ok(Err(fault)),
    };
    let source_id = match plain_text(&json["source_id"]) {
        Ok(source_id) => source_id,
        Err(fault) => return Ok(Err(fault)),
    };
    Ok(CareOutcome::new(model_id, source_id, value(&json["config"])?))
}

fn overlay_rooms(json: &Value) -> Parsed<OverlayRooms> {
    let schema = &json["room_schema"];
    let room_schema = match schema.get("fault") {
        Some(found) => Err(fault(found)?),
        None => Ok(bool_of(&schema["value"], "room_schema")?),
    };
    Ok(Ok(OverlayRooms { is_dict: bool_of(&json["is_dict"], "is_dict")?, room_schema }))
}

fn flag(json: &Value) -> Parsed<bool> {
    Ok(Ok(bool_of(json, "a section's flag")?))
}

// ---- Each section, as exported ----------------------------------------------------------------------

/// One section as exported: its value, the fault of its parse or absent (`Section`), or the text of
/// what the PyO3 crate raises out of `fill_section` instead of keeping a fault (`host_error`).
type Exported<T> = std::result::Result<Section<T>, Arc<str>>;

/// The section `name` of the export, made into the core's value by `read`: a value `read` refuses
/// is the section's fault, as the PyO3 crate's `read` and `parsed` keep it.
fn exported<T>(sections: &Value, name: &str, read: impl FnOnce(&Value) -> Parsed<T>) -> Result<Exported<T>> {
    let section = sections.get(name).ok_or_else(|| bad(format_args!("the export has no section {name}")))?;
    Ok(if let Some(found) = section.get("value") {
        Ok(read(found)?.into())
    } else if let Some(found) = section.get("fault") {
        Ok(Section::Fault(fault(found)?))
    } else if section.get("absent").is_some() {
        Ok(Section::Absent)
    } else {
        Err(text(str_of(&section["host_error"], "a section's host_error")?))
    })
}

/// Put an exported section in the configs, unless they hold it already.
fn put<T>(lazy: &Lazy<T>, exported: &Exported<T>, which: SectionId) -> Result<()> {
    match exported {
        Ok(section) => {
            let _ = lazy.get_or_parse(|| section.clone());
            Ok(())
        }
        Err(raised) => Err(Error::Runtime(format!("the PyO3 crate raises out of fill_section({}): {raised}", which.name()))),
    }
}

/// The Python-parsed sections of an exported world (`Parser::Host`), read once, which a host fills
/// the configs with (`fill`) and builds the floor plan from (`floor_plan`).
pub struct Sections {
    selection: Exported<Selection>,
    tick_plan: Exported<TickPlan>,
    facility_geometry: Exported<FacilityGeometry>,
    spatial_constraints: Exported<SpatialConstraints>,
    staff_operations: Exported<StaffOperations>,
    support_cooldown_minutes: Exported<i64>,
    information_spread: Exported<InformationSpread>,
    body_models: Exported<BodyModels>,
    care_outcome: Exported<CareOutcome>,
    zone_environment: Exported<ZoneEnvironmentSpec>,
    infrastructure: Exported<Infrastructure>,
    proximity: Exported<Option<Proximity>>,
    transit_capacities: Exported<bool>,
    overlay_rooms: Exported<OverlayRooms>,
    extensions_enabled: Exported<bool>,
    /// Whether the configs as exported have `spatial_constraints` (`Geo::of` reads the constraints
    /// then, or where the world has the overlay).
    spatial_configured: bool,
}

impl Sections {
    /// The sections of an exported entry (its `sections`, and its world's `model_configs`, which
    /// the body's operators are parsed with). Every section the host parses must be there.
    pub fn of_entry(entry: &Value) -> Result<Self> {
        let sections = &entry["sections"];
        if !sections.is_object() {
            return Err(bad("the world was exported without its sections"));
        }
        let raw = &entry["world"]["model_configs"];
        let configs = value(raw)?;
        let spatial_configured = object(raw, "model_configs")?.contains_key("spatial_constraints");
        Ok(Self {
            selection: exported(sections, "selection", selection)?,
            tick_plan: exported(sections, "tick_plan", tick_plan)?,
            facility_geometry: exported(sections, "facility_geometry", facility_geometry)?,
            spatial_constraints: exported(sections, "spatial_constraints", spatial_constraints)?,
            staff_operations: exported(sections, "staff_operations", staff_operations)?,
            support_cooldown_minutes: exported(sections, "support_cooldown_minutes", |json| Ok(crate::config::read::int(&value(json)?)))?,
            information_spread: exported(sections, "information_spread", |json| Ok(InformationSpread::from_v(&value(json)?)))?,
            body_models: exported(sections, "body_models", |json| {
                let affect = str_of(&json["affect"], "the affect model")?;
                Ok(BodyModels::parse(affect, json["relationship"].as_str(), &configs))
            })?,
            care_outcome: exported(sections, "care_outcome", care_outcome)?,
            zone_environment: exported(sections, "zone_environment", |json| Ok(ZoneEnvironmentSpec::from_v(&value(json)?)))?,
            infrastructure: exported(sections, "infrastructure", |json| Ok(Infrastructure::from_v(&value(json)?)))?,
            proximity: exported(sections, "proximity", |json| Ok(Proximity::from_v(&value(json)?)))?,
            transit_capacities: exported(sections, "transit_capacities", flag)?,
            overlay_rooms: exported(sections, "overlay_rooms", overlay_rooms)?,
            extensions_enabled: exported(sections, "extensions_enabled", flag)?,
            spatial_configured,
        })
    }

    /// `Host::fill_section` of a host that owns this world: the section put in the configs (kept
    /// for their stamp, `Filled::Kept`) unless they hold it already. False for a section the core
    /// parses from the configs' `V` itself (`Parser::Value`), which is not exported. An error is
    /// what the PyO3 crate raises out of `fill_section` for this world.
    pub fn fill(&self, configs: &Configs, which: SectionId) -> Result<bool> {
        match which {
            SectionId::Selection => put(&configs.selection, &self.selection, which)?,
            SectionId::TickPlan => put(&configs.tick_plan, &self.tick_plan, which)?,
            SectionId::FacilityGeometry => put(&configs.facility_geometry, &self.facility_geometry, which)?,
            SectionId::SpatialConstraints => put(&configs.spatial_constraints, &self.spatial_constraints, which)?,
            SectionId::StaffOperations => put(&configs.staff_operations, &self.staff_operations, which)?,
            SectionId::SupportCooldownMinutes => put(&configs.support_cooldown_minutes, &self.support_cooldown_minutes, which)?,
            SectionId::InformationSpread => put(&configs.information_spread, &self.information_spread, which)?,
            SectionId::BodyModels => put(&configs.body_models, &self.body_models, which)?,
            SectionId::CareOutcome => put(&configs.care_outcome, &self.care_outcome, which)?,
            SectionId::ZoneEnvironment => put(&configs.zone_environment, &self.zone_environment, which)?,
            SectionId::Infrastructure => put(&configs.infrastructure, &self.infrastructure, which)?,
            SectionId::Proximity => put(&configs.proximity, &self.proximity, which)?,
            SectionId::TransitCapacities => put(&configs.transit_capacities, &self.transit_capacities, which)?,
            SectionId::OverlayRooms => put(&configs.overlay_rooms, &self.overlay_rooms, which)?,
            SectionId::ExtensionsEnabled => put(&configs.extensions_enabled, &self.extensions_enabled, which)?,
            // The core parses the others from the configs' `V`.
            _ => return Ok(false),
        }
        Ok(true)
    }

    /// `Geo::of` of the exported world: the plan of its facility geometry and spatial constraints
    /// with `overlay`'s facility moves and partitions (the world's `geometry_overlay_v1`; None
    /// without it). An error is the refusal `Geo::of` raises.
    pub fn floor_plan(&self, overlay: Option<&V>) -> Result<Arc<Geo>> {
        let refusal = |fault: &Fault| {
            if fault.is_refusal() {
                Error::unsupported(fault.text.to_string())
            } else {
                Error::unsupported("the Python rule's parse raises")
            }
        };
        let raised = |which: SectionId, raised: &str| {
            Error::Runtime(format!("the PyO3 crate raises out of fill_section({}): {raised}", which.name()))
        };
        let facility = match &self.facility_geometry {
            Ok(Section::Absent) => None,
            Ok(Section::Ok(geometry)) => Some(geometry.plan.clone().map_err(|fault| refusal(&fault))?),
            Ok(Section::Fault(fault)) => return Err(refusal(fault)),
            Err(text) => return Err(raised(SectionId::FacilityGeometry, text)),
        };
        let spatial = if self.spatial_configured || overlay.is_some() {
            match &self.spatial_constraints {
                Ok(Section::Ok(spatial)) => Some(spatial.clone()),
                Ok(Section::Absent) => None,
                Ok(Section::Fault(fault)) => return Err(refusal(fault)),
                Err(text) => return Err(raised(SectionId::SpatialConstraints, text)),
            }
        } else {
            None
        };
        let key = OverlayKey::of(overlay, None)?;
        let (moves, partitions) = key.parts(overlay);
        Ok(Arc::new(Geo::new(facility.as_deref(), spatial.as_deref(), moves.as_ref(), partitions.as_ref())?))
    }
}

// ---- The floor plan of a world as it is now ---------------------------------------------------------

/// The overlay of a world now (`model_state["geometry_overlay_v1"]`), or None without it.
pub fn overlay(world: &World) -> Result<Option<V>> {
    Ok(world.slots.try_value(OVERLAY)?.map(|value| value.clone()))
}

/// `Host::floor_plan` of a host that owns the world: the plan last built, built again
/// (`Sections::floor_plan`) when the overlay's facility moves or partitions change (its
/// `OverlayKey`), so that a tick that moves a facility walks on the new plan.
#[derive(Default)]
pub struct FloorPlans {
    /// The plan last built, and the overlay's key it was built for.
    built: Option<(OverlayKey, Arc<Geo>)>,
    builds: usize,
}

impl FloorPlans {
    /// The plan of `sections` with the world's overlay now (`overlay`).
    pub fn plan(&mut self, sections: &Sections, overlay: Option<&V>) -> Result<Arc<Geo>> {
        let key = OverlayKey::of(overlay, None)?;
        if let Some((held, geo)) = &self.built {
            if *held == key {
                return Ok(geo.clone());
            }
        }
        let geo = sections.floor_plan(overlay)?;
        self.built = Some((key, geo.clone()));
        self.builds += 1;
        Ok(geo)
    }

    /// How many times `plan` built the plan.
    pub fn builds(&self) -> usize {
        self.builds
    }
}
