//! The section `geometry` of the configs: `facility_geometry` and `spatial_constraints` of
//! `model_configs`, parsed by their contracts (`FacilityGeometry`, `SpatialConstraints` of
//! `contracts/`) and read into the values the rules use (design-p7.md, lane 7-2).
//!
//! Each part keeps what reading it gave on its own, so a read site meets the error it met when it
//! read the parsed model itself: the plan of the floor (`Geo`), the arrival approach routes, and
//! whether facilities need independent queues.

use std::sync::Arc;

use super::Fault;
use crate::geometry::{Opening, Pt, Route, ServicePoint, Wall, Zone};
use crate::row::IdMap;
use crate::value::V;

/// `FacilityGeometry.model_validate(model_configs["facility_geometry"])`, as the rules read it.
pub struct FacilityGeometry {
    /// The zones, routes and points of `Geo`'s layout, or why they cannot be read (a coordinate
    /// too large to compute with).
    pub plan: Result<Arc<FacilityPlan>, Fault>,
    /// `arrival_approach_routes`.
    pub approach_routes: Result<Arc<[ApproachRoute]>, Fault>,
    /// `independent_queues_required`: a service point whose queue is not its facility's ID, or a
    /// facility with two points.
    pub independent_queues: Result<bool, Fault>,
    /// `model_dump(mode="json")` of the parsed model, which a frame shows (`frame.rs`, #264), or
    /// why it cannot be read.
    pub document: Result<Arc<V>, Fault>,
}

/// The Scenario's own floor plan, before the overlay moves anything.
pub struct FacilityPlan {
    /// The first of a repeated ID, as `next(...)` finds; the contract refuses a repeat anyway.
    pub zones: IdMap<Zone>,
    pub routes: IdMap<Route>,
    pub facility_points: IdMap<Pt>,
    pub service_points: Vec<ServicePoint>,
}

/// An `ArrivalApproachRoute`.
#[derive(Clone, Debug, PartialEq)]
pub struct ApproachRoute {
    pub route_id: Arc<str>,
    pub destination_zone_id: Arc<str>,
    pub source_ids: Vec<Arc<str>>,
    /// `walking_speed_m_per_min`.
    pub walking_speed: f64,
    /// `distance_m`.
    pub distance: f64,
    /// `travel_minutes`, or what reading it raised (`math.ceil` of an infinite distance).
    pub travel_minutes: Result<i64, Fault>,
}

/// `SpatialConstraints.model_validate(model_configs.get("spatial_constraints", {}))`.
#[derive(Clone, Default)]
pub struct SpatialConstraints {
    pub walls: Vec<Wall>,
    pub openings: Vec<Opening>,
    /// The obstacles' polygons.
    pub obstacles: Vec<Vec<Pt>>,
    /// The equipment footprints, by facility.
    pub footprints: Vec<(Arc<str>, Vec<Pt>)>,
    /// `model_dump(mode="json")` of the parsed model, which a frame shows (`frame.rs`, #264); None
    /// where the host did not keep it.
    pub document: Option<Arc<V>>,
}
