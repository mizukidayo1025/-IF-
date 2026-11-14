//! `simulator_signatures(world.model_configs)` of `domain/simulator_state.py`, with Python's errors
//! and texts: the signed extension implementations the configs enable, by key, in the order of
//! the Python dict (lane D of #250, #349).
//!
//! `validate::world` asks it whether to validate the extensions at all (a world without any
//! extension key), and raises its error as it is; the head of the extensions
//! (`extension_head.rs`) reads the required states from it.
//!
//! The pydantic models it validates are the core's contracts (`crate::contracts`):
//! `ScenarioZoneEnvironmentConfig` and `FacilityGeometry`. Two small models are read here:
//! `SharedOperationsConfig` (whose error Python replaces with its own text) and
//! `FacilityAwarenessConfig`, of which only a config the model accepts is read; for another, the
//! core does not write pydantic's text (#250 follow-up) and refuses.

use std::collections::HashSet;
use std::sync::Arc;

use indexmap::IndexMap;

use super::agent_dynamics::canonical_agent_dynamics_policy;
use super::extension_head::py::{self, get, Held};
use super::staff_dynamics::canonical_staff_dynamics_config;
use super::{models, selection, View};
use crate::checkpoint::sha256_hex;
use crate::contracts;
use crate::error::{Error, Result};
use crate::events::is_stable_id;
use crate::host::Contract;
use crate::json::python_float_repr;
use crate::value::V;

/// The signatures: each key (`"simulator:..."`, `"operator:..."`) and its value.
pub type Signatures = Vec<(Arc<str>, Arc<str>)>;

type Map = IndexMap<Arc<str>, V>;

const ACTION_LIFECYCLE_POLICY: &str = "applied-action-lifecycle-v2";
const AUTONOMY: &str = "contextual-autonomy@2:stateless";
const AGENT_DYNAMICS: &str = "agent_dynamics_v1";
const POPULATION_DYNAMICS: &str = "population_dynamics_v1";
const POPULATION_DYNAMICS_POLICY: &str = "capacity-aware-arrivals-departures-v1";
const SOCIAL_NETWORK: &str = "social_network_v1";
const SOCIAL_NETWORK_POLICY: &str = "directed-contact-relationships-v1";
const STAFF_DYNAMICS: &str = "staff_dynamics_v1";
const STAFF_DYNAMICS_V1: &str = "continuous-duty-fatigue-v1";
pub const FACILITY_QUEUES: &str = "facility_queues_v1";
const FACILITY_QUEUES_POLICY: &str = "independent-facility-queues-v1";
const SHARED_OPERATIONS: &str = "shared_operations_v1";
const SHARED_OPERATIONS_POLICY: &str = "shared-operations-v1";
const POPULATION_LIFECYCLE: &str = "population_lifecycle_v1";
const POPULATION_LIFECYCLE_POLICY: &str = "same-id-population-lifecycle-v1";
pub const TEMPORARY_SPACES: &str = "temporary_spaces_v1";
const TEMPORARY_SPACES_POLICY: &str = "typed-temporary-capacity-v1";
const STAFF_OPERATIONS: &str = "staff_operations_v1";
const STAFF_OPERATIONS_POLICY: &str = "autonomous-routed-staff-v1";
const MOVEMENT: &str = "movement_v1";
const MOVEMENT_POLICY: &str = "spatial-route-and-room-waypoint-v2";
const GEOMETRY_OVERLAY: &str = "geometry_overlay_v1";
const GEOMETRY_OVERLAY_POLICY: &str = "bounded-runtime-geometry-v1";
const BODY_STATE: &str = "body_state_v1";
const BODY_POLICY: &str = "life-state-feedback-v1";
const FACILITY_AWARENESS_POLICY: &str = "entry-orientation-and-arrival-discovery-v1";
const FACILITY_AWARENESS_SOURCE: &str = "ASM-FACILITY-ENTRY-ORIENTATION-V1";
const PROXIMITY_POLICY: &str = "distance-sampled-proximity-v1";
const TEMPORARY_ROOM_BED_POLICY: &str = "exclusive-physical-sleep-beds-v1";
const TEMPORARY_ROOM_INTAKE_POLICY: &str = "physical-temporary-room-intake-v1";
const INSTALLED_FACILITY_PLACEMENT_POLICY: &str = "generator-grid-installed-facility-v1";
/// `LIFE_STATE_PSYCHOLOGY_POLICIES`.
const LIFE_STATE_PSYCHOLOGY: [&str; 2] = ["life-state-feedback-v1", "mean-reverting-life-state-v1"];

fn value_error<T>(text: &str) -> Result<T> {
    Err(Error::Value(text.to_owned()))
}

fn text(value: &str) -> Arc<str> {
    Arc::from(value)
}

/// `configs.get("simulator_extensions", {}).get("enabled", False)`, as a truth.
fn extensions_enabled(configs: &Map) -> Result<bool> {
    match configs.get("simulator_extensions") {
        None => Ok(false),
        Some(V::Map(extensions)) => Ok(extensions.get("enabled").is_some_and(V::is_truthy)),
        // `.get` of another value is Python's AttributeError, which the core has no class for.
        Some(_) => Err(Error::unsupported("simulator_extensions is not a dict")),
    }
}

/// `validate_shared_operations_config(raw)`: `SharedOperationsConfig` (`schema_version:
/// Literal[1]`, `policy_version: Literal["shared-operations-v1"]`, no other field) accepts it, or
/// Python's ValueError.
fn shared_operations_config(raw: &V) -> Result<()> {
    let accepted = match raw {
        V::Map(raw) => raw.iter().all(|(key, value)| match &**key {
            "schema_version" => matches!(value, V::Int(1) | V::Bool(true)) || matches!(value, V::Float(number) if *number == 1.0),
            "policy_version" => matches!(value, V::Str(policy) if &**policy == SHARED_OPERATIONS_POLICY),
            _ => false,
        }),
        _ => false,
    };
    if accepted {
        Ok(())
    } else {
        value_error("invalid shared_operations config")
    }
}

/// `canonical_proximity_contact_config(raw)`: None for no config or a disabled one, else its
/// `max_distance_m` (a float), `sample_interval_minutes` and `max_pairs_per_sample`.
pub fn canonical_proximity(raw: Option<&V>) -> Result<Option<(f64, i64, i64)>> {
    let raw = match raw {
        None | Some(V::Null) => return Ok(None),
        Some(V::Map(raw)) => raw,
        Some(_) => return value_error("social_proximity config fields do not match"),
    };
    let fields = ["schema_version", "enabled", "policy_version", "sample_interval_minutes", "max_distance_m", "max_pairs_per_sample"];
    if !py::keys_are(raw, &fields) {
        return value_error("social_proximity config fields do not match");
    }
    if !matches!(raw["schema_version"], V::Int(1)) || !matches!(raw["enabled"], V::Bool(_)) || !py::eq_str(raw.get("policy_version"), PROXIMITY_POLICY) {
        return value_error("social_proximity config version is unsupported");
    }
    let interval = match raw["sample_interval_minutes"] {
        V::Int(interval) if (1..=60).contains(&interval) => Some(interval),
        _ => None,
    };
    let maximum = match &raw["max_distance_m"] {
        V::Int(_) | V::Big(_) | V::Float(_) => py::number(&raw["max_distance_m"]).filter(|maximum| maximum.is_finite() && (0.25..=10.0).contains(maximum)),
        _ => None,
    };
    let pairs = match raw["max_pairs_per_sample"] {
        V::Int(pairs) if (1..=10_000).contains(&pairs) => Some(pairs),
        _ => None,
    };
    let (Some(interval), Some(maximum), Some(pairs)) = (interval, maximum, pairs) else {
        return value_error("social_proximity bounds are invalid");
    };
    Ok(matches!(raw["enabled"], V::Bool(true)).then_some((maximum, interval, pairs)))
}

/// `ScenarioZoneEnvironmentConfig.model_validate(raw).policy_version`, or pydantic's error.
fn zone_environment_policy(raw: &V) -> Result<Arc<str>> {
    match contracts::validate(&Contract::ZoneEnvironment, raw).map_err(models::tracked)? {
        V::Map(dump) => match dump.get("policy_version") {
            Some(V::Str(policy)) => Ok(policy.clone()),
            _ => Err(Error::unsupported("the zone environment has no policy")),
        },
        _ => Err(Error::unsupported("the zone environment is not a model")),
    }
}

/// `independent_queues_required(configs)`, a kernel of Python's: false without a config dict;
/// true for a service point whose queue is not its facility's ID, or a facility with two points
/// of the parsed `FacilityGeometry`; the parse's error raised as it is.
pub fn independent_queues_required(configs: &Map) -> Result<bool> {
    let Some(raw @ V::Map(_)) = configs.get("facility_geometry") else {
        return Ok(false);
    };
    let geometry = contracts::validate(&Contract::FacilityGeometry, raw).map_err(models::tracked)?;
    let points = match &geometry {
        V::Map(geometry) => match geometry.get("service_points") {
            Some(V::List(points)) => points.as_slice(),
            _ => &[],
        },
        _ => &[],
    };
    let mut facilities = HashSet::with_capacity(points.len());
    for point in points {
        let V::Map(point) = point else {
            return Err(Error::unsupported("a service point is not a model"));
        };
        let (Some(V::Str(facility)), Some(V::Str(queue))) = (point.get("facility_id"), point.get("queue_id")) else {
            return Err(Error::unsupported("a service point has no IDs"));
        };
        if queue != facility || !facilities.insert(facility.clone()) {
            return Ok(true);
        }
    }
    Ok(false)
}

/// `canonical_facility_awareness_config(raw)`: `FacilityAwarenessConfig`'s `model_dump(mode="json")`
/// of a config it accepts. Another config is refused: pydantic's text of it is not written here.
fn canonical_facility_awareness(raw: &V) -> Result<V> {
    let refused = || Err(Error::unsupported("a facility_awareness config FacilityAwarenessConfig does not accept"));
    let V::Map(raw) = raw else {
        return refused();
    };
    let mut ids: Vec<Arc<str>> = Vec::new();
    let mut discover = true;
    for (key, value) in raw {
        match (&**key, value) {
            ("schema_version", V::Int(1) | V::Bool(true)) => {}
            ("schema_version", V::Float(number)) if *number == 1.0 => {}
            ("initial_known_facility_ids", V::List(items)) if items.len() <= 1000 => {
                for item in items {
                    let V::Str(id) = item else {
                        return refused();
                    };
                    ids.push(id.clone());
                }
                if ids.iter().any(|id| !is_stable_id(id)) {
                    return refused();
                }
                ids.sort_unstable();
                if ids.windows(2).any(|pair| pair[0] == pair[1]) {
                    return refused();
                }
            }
            ("discover_co_located_facilities_on_arrival", V::Bool(flag)) => discover = *flag,
            ("source_type", V::Str(source)) if &**source == "assumption" => {}
            ("source_id", V::Str(source)) if &**source == FACILITY_AWARENESS_SOURCE => {}
            _ => return refused(),
        }
    }
    let mut dump = Map::new();
    dump.insert(text("schema_version"), V::Int(1));
    dump.insert(text("initial_known_facility_ids"), V::List(ids.into_iter().map(V::Str).collect()));
    dump.insert(text("discover_co_located_facilities_on_arrival"), V::Bool(discover));
    dump.insert(text("source_type"), V::Str(text("assumption")));
    dump.insert(text("source_id"), V::Str(text(FACILITY_AWARENESS_SOURCE)));
    Ok(V::Map(dump))
}

/// `_simulator_signature(key, version)`.
fn simulator_signature(key: &str, version: &str) -> String {
    let implementation = if key == STAFF_DYNAMICS {
        if version == STAFF_DYNAMICS_V1 {
            "1"
        } else {
            "2"
        }
    } else if key == BODY_STATE {
        "3"
    } else if [STAFF_OPERATIONS, SOCIAL_NETWORK].contains(&key) {
        "2"
    } else {
        "1"
    };
    let state = if key == MOVEMENT { "2" } else { "1" };
    format!("{version}@{implementation}:state{state}")
}

/// The signatures, or the error Python raises for the configs.
pub fn simulator_signatures(view: &View<'_>) -> Result<Signatures> {
    let configs = view.configs()?;
    let V::Map(configs) = &*configs else {
        // `selected_models` refuses configs that are not a dict.
        selection::selected_models(view)?;
        return Err(Error::unsupported("model_configs is not a dict"));
    };
    signatures_of(view, configs)
}

fn signatures_of(view: &View<'_>, configs: &Map) -> Result<Signatures> {
    let has = |name: &str| configs.contains_key(name);
    if has("geometry_overlay") && !has("facility_geometry") {
        return value_error("geometry_overlay requires facility_geometry");
    }
    if let Some(raw) = configs.get("shared_operations") {
        shared_operations_config(raw)?;
    }
    // `selected_models(configs)["psychology"]`: the selection always has the domain.
    let body = selection::selected_models(view)?.get("psychology").is_some_and(|model| LIFE_STATE_PSYCHOLOGY.contains(&&**model));
    let proximity = canonical_proximity(configs.get("social_proximity"))?;
    if proximity.is_some() && (!has("facility_geometry") || !body) {
        return value_error("social_proximity requires facility_geometry and a life-state psychology policy");
    }
    let enabled = extensions_enabled(configs)?;
    let mut signatures: Signatures = Vec::new();
    let mut sign = |key: &str, value: String| signatures.push((text(key), Arc::from(value)));
    if !enabled
        && !has("facility_geometry")
        && !has("population")
        && !has("staff_operations")
        && !has("staff_dynamics")
        && !has("shared_operations")
        && !has("facility_awareness")
        && !body
    {
        sign("operator:action_lifecycle", format!("{ACTION_LIFECYCLE_POLICY}@1:state1"));
        if has("population_context") {
            sign("operator:autonomy_behavior", AUTONOMY.to_owned());
        }
        if let Some(raw) = configs.get("zone_environment") {
            sign("operator:zone_environment", format!("{}@1:state1", zone_environment_policy(raw)?));
        }
        return Ok(signatures);
    }
    let mut versions: Vec<(&str, Arc<str>)> = vec![
        (AGENT_DYNAMICS, text(canonical_agent_dynamics_policy(configs.get("agent_dynamics"))?)),
        (POPULATION_DYNAMICS, text(POPULATION_DYNAMICS_POLICY)),
        (SOCIAL_NETWORK, text(SOCIAL_NETWORK_POLICY)),
        (STAFF_DYNAMICS, text(STAFF_DYNAMICS_V1)),
    ];
    if independent_queues_required(configs)? {
        versions.push((FACILITY_QUEUES, text(FACILITY_QUEUES_POLICY)));
    }
    if has("shared_operations") {
        versions.push((SHARED_OPERATIONS, text(SHARED_OPERATIONS_POLICY)));
    }
    let staff = canonical_staff_dynamics_config(configs.get("staff_dynamics").unwrap_or(&V::Null), Held::Tracked)?;
    let V::Str(staff_policy) = get(&staff, "policy_version") else {
        return Err(Error::unsupported("the staff dynamics policy is not a str"));
    };
    versions[3].1 = staff_policy.clone();
    if enabled || has("population") {
        versions.push((POPULATION_LIFECYCLE, text(POPULATION_LIFECYCLE_POLICY)));
        if enabled {
            versions.push((TEMPORARY_SPACES, text(TEMPORARY_SPACES_POLICY)));
        }
    }
    if enabled || has("staff_operations") || body {
        versions.push((STAFF_OPERATIONS, text(STAFF_OPERATIONS_POLICY)));
    }
    if has("facility_geometry") {
        versions.push((MOVEMENT, text(MOVEMENT_POLICY)));
    }
    if let Some(overlay) = configs.get("geometry_overlay") {
        let supported = matches!(overlay, V::Map(overlay) if py::keys_are(overlay, &["schema_version"]) && matches!(overlay["schema_version"], V::Int(1..=4)));
        if !supported {
            return value_error("unsupported geometry_overlay config");
        }
        versions.push((GEOMETRY_OVERLAY, text(GEOMETRY_OVERLAY_POLICY)));
    }
    if body {
        versions.push((BODY_STATE, text(BODY_POLICY)));
    }
    for (key, version) in &versions {
        sign(&format!("simulator:{key}"), simulator_signature(key, version));
    }
    if let Some(V::Map(geometry)) = configs.get("facility_geometry") {
        if let Some(points) = geometry.get("service_points").filter(|points| points.is_truthy()) {
            let hash = sha256_hex(py::dumps_sorted(points)?.as_bytes());
            sign("operator:facility_service_points", format!("facility-service-points@1:{hash}"));
        }
    }
    if enabled || has("population_context") || body {
        sign("operator:autonomy_behavior", AUTONOMY.to_owned());
    }
    if let Some(raw) = configs.get("zone_environment") {
        sign("operator:zone_environment", format!("{}@1:state1", zone_environment_policy(raw)?));
    }
    if let Some(raw) = configs.get("facility_awareness") {
        let hash = sha256_hex(py::dumps_sorted(&canonical_facility_awareness(raw)?)?.as_bytes());
        sign("operator:facility_awareness", format!("{FACILITY_AWARENESS_POLICY}@1:{hash}"));
    }
    if let Some((distance, interval, pairs)) = proximity {
        sign(
            "operator:social_proximity",
            format!("{PROXIMITY_POLICY}@1:state1:distance={}:interval={interval}:pairs={pairs}", python_float_repr(distance)),
        );
    }
    // The overlay's schema was checked above: an int of 1..4, or no overlay.
    let overlay_schema = match configs.get("geometry_overlay") {
        Some(V::Map(overlay)) => match overlay.get("schema_version") {
            Some(V::Int(schema)) => Some(*schema),
            _ => None,
        },
        _ => None,
    };
    if matches!(overlay_schema, Some(2..=4)) {
        sign("operator:temporary_room_beds", format!("{TEMPORARY_ROOM_BED_POLICY}@1:state1"));
    }
    if matches!(overlay_schema, Some(3..=4)) {
        sign("operator:temporary_room_intake", format!("{TEMPORARY_ROOM_INTAKE_POLICY}@1:state1"));
    }
    if overlay_schema == Some(4) {
        sign("operator:installed_facility_placement", format!("{INSTALLED_FACILITY_PLACEMENT_POLICY}@1:state1"));
    }
    sign("operator:action_lifecycle", format!("{ACTION_LIFECYCLE_POLICY}@1:state1"));
    Ok(signatures)
}
