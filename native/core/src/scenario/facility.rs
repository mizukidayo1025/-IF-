//! The standard synthetic shelter of `region/facility.py` (Issue #296): `StandardShelterGenerator
//! .generate` with its helpers and the checks it runs on what it made, for the inputs the Scenario
//! screen can give (#291). The builder (`InitialScenarioBuilder.build`) asks it for the people, the
//! floor area (or none), the accessible toilets, the staff (or none) and the facility conditions;
//! typed zone changes, zone area shares, equipment and a selected shelter's capacity are not
//! computed here (the browser refuses them before, #295).
//!
//! Every float is computed in Python's order of operations: `sum` is `py_sum`, `min` and `max`
//! are `py_min` and `py_max`, `math.hypot` is `mathx::hypot` and `round(x, 12)` is `py_round`. The
//! design is `FacilityDesign.model_dump(mode="json")` as a `V`, whose canonical JSON (`V::to_json`)
//! is the bytes Python's `storage.projects.canonical` writes; the cases of
//! `native/core/tests/data/scenario_facility.json` check them by sha256.

use indexmap::IndexMap;
use std::sync::Arc;

use crate::checkpoint::sha256_hex;
use crate::error::{Error, Result};
use crate::geometry::{point_in_polygon, Pt};
use crate::mathx::{hypot, py_max, py_min, py_round, py_sum};
use crate::text::casefold;
use crate::value::V;

pub const SYNTHETIC_FACILITY_SOURCE_ID: &str = "ASM-STANDARD-SHELTER-GEOMETRY-V1";
pub const SYNTHETIC_CAPACITY_SOURCE_ID: &str = "ASM-SHELTER-AREA-HEURISTIC-V1";
pub const SYNTHETIC_STAFFING_SOURCE_ID: &str = "ASM-SHELTER-STAFFING-HEURISTIC-V2";
pub const SYNTHETIC_TOILET_SOURCE_ID: &str = "ASM-SHELTER-TOILET-HEURISTIC-V2";
pub const SERVICE_STATION_SOURCE_ID: &str = "ASM-SHELTER-SERVICE-STATIONS-V1";
/// The source of an operator-entered area (`InitialScenarioBuilder.build`).
pub const OPERATOR_AREA_SOURCE_ID: &str = "SRC-OPERATOR-AREA";
/// `ACCESSIBLE_ROUTE_MIN_WIDTH_M` of `region/models.py`.
pub const ACCESSIBLE_ROUTE_MIN_WIDTH_M: f64 = 1.8;
/// `FacilityDesignRequest.aspect_ratio`'s default, the only one the builder uses.
pub const ASPECT_RATIO: f64 = 1.5;
const STATION_STAFF_SKILLS: [&str; 2] = ["water_service", "meal_service"];
const GENERAL_STAFF_SKILLS: [&str; 6] = ["support", "intake", "information", "water_service", "meal_service", "medical_support"];

/// The coefficients `standard_service_counts` reads from the model's defaults
/// (`sanitation_diagnostics`, `service_capacity`, `ToiletServiceConfig`, `PsychologyModelConfig`,
/// `default_resource_service_config`). The fixture records them as Python has them; the core's
/// test fails when they differ.
pub mod coefficients {
    pub const PEOPLE_PER_TOILET_INITIAL: i64 = 50;
    pub const PLANNING_TOILET_USES_PER_PERSON_DAY: i64 = 5;
    pub const TOILET_MINIMUM_MINUTES: i64 = 4;
    pub const TOILET_MAXIMUM_MINUTES: i64 = 8;
    pub const OPERATING_MINUTES_PER_DAY: i64 = 960;
    pub const TARGET_UTILIZATION: f64 = 0.8;
    pub const RESOURCE_SERVICE_MINUTES: i64 = 5;
    pub const HYDRATION_GROWTH_PER_MINUTE: f64 = 0.001;
    pub const HYDRATION_RELIEF: f64 = 0.35;
    pub const HYDRATION_ACTION_THRESHOLD: f64 = 0.25;
    pub const HUNGER_GROWTH: f64 = 0.0005;
    pub const HUNGER_RELIEF: f64 = 0.5;
    pub const HUNGER_ACTION_THRESHOLD: f64 = 0.7;
}

/// `_ZONE_DESIGN`: (zone ID, name, share of the floor).
const ZONE_DESIGN: [(&str, &str, f64); 7] = [
    ("sleeping", "避難スペース", 0.62),
    ("quiet", "静養スペース", 0.08),
    ("medical", "救護スペース", 0.06),
    ("food", "食事・給水スペース", 0.08),
    ("toilet", "トイレ", 0.06),
    ("intake", "受付", 0.05),
    ("staff", "運営本部", 0.05),
];

/// The optional zones the facility conditions add: (terms, zone ID, name, share).
const OPTIONAL_ZONES: [(&[&str], &str, &str, f64); 3] = [
    (&["ペット", "pet"], "pet", "ペット同行スペース", 0.08),
    (&["授乳", "乳幼児", "family", "infant"], "family", "乳幼児・授乳スペース", 0.06),
    (&["個室", "プライバシー", "privacy"], "private", "個別・プライバシースペース", 0.08),
];
const ACCESSIBLE_TERMS: [&str; 4] = ["車いす", "車椅子", "wheelchair", "バリアフリー"];

/// What the builder asks of the generator for a request of the Scenario screen
/// (`FacilityDesignRequest` with the default aspect ratio, `area_source_ids` of an entered area,
/// and no zone changes, shares, equipment or stated capacity).
#[derive(Clone, Debug, PartialEq)]
pub struct FacilityRequest {
    pub population: i64,
    pub gross_area_m2: Option<f64>,
    pub accessible_toilet_count: i64,
    pub staff_count: Option<i64>,
    pub facility_conditions: Vec<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Zone {
    pub id: String,
    pub name: String,
    pub capacity: Option<i64>,
    pub display_x: f64,
    pub display_y: f64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ZoneEdge {
    pub id: String,
    pub from_zone_id: String,
    pub to_zone_id: String,
    pub travel_minutes: i64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Facility {
    pub id: String,
    pub kind: String,
    pub zone_id: String,
    pub capacity: i64,
    pub working_units: Option<i64>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Staff {
    pub id: String,
    pub zone_id: String,
    pub skills: Vec<&'static str>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct PhysicalZone {
    pub zone_id: String,
    pub polygon: Vec<Pt>,
    pub usable_area_m2: f64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Route {
    pub edge_id: String,
    pub points: Vec<Pt>,
    pub width_m: f64,
    pub accessible: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ServicePoint {
    pub id: String,
    pub facility_id: String,
    pub point: Pt,
    pub service_capacity: i64,
}

/// `FacilityGeometry` (`model_configs.facility_geometry`) as the generator makes it: no arrival
/// routes, every service point a legacy one of unknown access.
#[derive(Clone, Debug, PartialEq)]
pub struct Geometry {
    pub width_m: f64,
    pub height_m: f64,
    pub source_kind: &'static str,
    pub source_ids: Vec<String>,
    /// Polygons share the geometry's source IDs (`geometry_source_ids`).
    pub zone_source_ids: Vec<String>,
    pub zones: Vec<PhysicalZone>,
    /// By facility ID, in the facilities' order.
    pub facility_points: Vec<(String, Pt)>,
    pub service_points: Vec<ServicePoint>,
    pub routes: Vec<Route>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Wall {
    pub wall_id: String,
    pub start: Pt,
    pub end: Pt,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Opening {
    pub opening_id: String,
    pub wall_id: String,
    pub start: Pt,
    pub end: Pt,
    pub width_m: f64,
}

/// `EditableFacilityGeometry` as the generator makes it: walls and doorways with the geometry's
/// source IDs, and no obstacles or equipment footprints.
#[derive(Clone, Debug, PartialEq)]
pub struct EditableGeometry {
    pub width_m: f64,
    pub height_m: f64,
    pub source_ids: Vec<String>,
    pub walls: Vec<Wall>,
    pub openings: Vec<Opening>,
    /// (route ID, opening ID).
    pub route_openings: Vec<(String, String)>,
}

/// `FacilityDesign`.
#[derive(Clone, Debug, PartialEq)]
pub struct FacilityDesign {
    pub zones: Vec<Zone>,
    pub zone_edges: Vec<ZoneEdge>,
    pub facilities: Vec<Facility>,
    pub staff: Vec<Staff>,
    pub geometry: Geometry,
    pub editable_geometry: EditableGeometry,
    pub capacity_supported: i64,
    pub requested_population: i64,
    pub observations: Vec<V>,
    pub assumptions: Vec<V>,
    pub warnings: Vec<String>,
}

/// `standard_service_counts`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ServiceCounts {
    pub toilet_standard: i64,
    pub toilet_demand: i64,
    pub water_station: i64,
    pub meal_distribution: i64,
}

fn ceil_int(value: f64) -> i64 {
    value.ceil() as i64
}

/// `uses_per_person_day` of `experiments/service_capacity.py`.
fn uses_per_person_day(growth_per_minute: f64, relief: f64, action_threshold: f64) -> f64 {
    let removed = if action_threshold > 0.0 { py_min(relief, action_threshold) } else { relief };
    growth_per_minute * 1440.0 / removed
}

/// `required_service_units`.
fn required_service_units(daily_service_minutes: f64) -> i64 {
    if daily_service_minutes <= 0.0 {
        return 0;
    }
    use coefficients::*;
    ceil_int(daily_service_minutes / (OPERATING_MINUTES_PER_DAY as f64 * TARGET_UTILIZATION))
}

/// `standard_service_counts(population)`.
pub fn standard_service_counts(population: i64) -> ServiceCounts {
    use coefficients::*;
    let water_uses = uses_per_person_day(HYDRATION_GROWTH_PER_MINUTE, HYDRATION_RELIEF, HYDRATION_ACTION_THRESHOLD);
    let meal_uses = uses_per_person_day(HUNGER_GROWTH, HUNGER_RELIEF, HUNGER_ACTION_THRESHOLD);
    let people = population as f64;
    // population * USES * (min + max) / 2: ints until the division by 2.
    let toilet_work = (population * PLANNING_TOILET_USES_PER_PERSON_DAY * (TOILET_MINIMUM_MINUTES + TOILET_MAXIMUM_MINUTES)) as f64 / 2.0;
    ServiceCounts {
        toilet_standard: ceil_int(people / PEOPLE_PER_TOILET_INITIAL as f64),
        toilet_demand: required_service_units(toilet_work),
        water_station: required_service_units(people * water_uses * RESOURCE_SERVICE_MINUTES as f64).max(1),
        meal_distribution: required_service_units(people * meal_uses * RESOURCE_SERVICE_MINUTES as f64).max(1),
    }
}

/// `shelter_staff`: the general staff, then one member per station, in the station's zone.
pub fn shelter_staff(population: i64, staff_count: Option<i64>, station_zone_ids: &[&str], general_zone_id: &str) -> Vec<Staff> {
    let needed = station_zone_ids.len() as i64;
    let (general, stations) = match staff_count {
        None => (ceil_int(population as f64 / 100.0).max(1), needed),
        Some(count) => {
            let stations = needed.min((count - 1).max(0));
            (count - stations, stations)
        }
    };
    let mut staff: Vec<Staff> = (1..=general)
        .map(|index| Staff { id: format!("staff_{index:03}"), zone_id: general_zone_id.to_owned(), skills: GENERAL_STAFF_SKILLS.to_vec() })
        .collect();
    for (index, zone_id) in station_zone_ids.iter().take(stations.max(0) as usize).enumerate() {
        staff.push(Staff { id: format!("staff_{:03}", general + index as i64 + 1), zone_id: (*zone_id).to_owned(), skills: STATION_STAFF_SKILLS.to_vec() });
    }
    staff
}

/// `shelter_staff_assumption`.
pub fn shelter_staff_assumption(staff: &[Staff], entered_total: Option<i64>) -> V {
    let stations: Vec<&Staff> = staff.iter().filter(|member| member.skills == STATION_STAFF_SKILLS).collect();
    let mut zones: Vec<&str> = stations.iter().map(|member| member.zone_id.as_str()).collect();
    zones.sort_unstable();
    zones.dedup();
    map(vec![
        ("id", s(SYNTHETIC_STAFFING_SOURCE_ID)),
        ("subject", s("synthetic_staff_count")),
        (
            "value",
            map(vec![
                ("total", V::Int(staff.len() as i64)),
                ("general_staff", V::Int((staff.len() - stations.len()) as i64)),
                ("station_staff", V::Int(stations.len() as i64)),
                ("station_staff_zone_ids", V::List(zones.into_iter().map(s).collect())),
                ("entered_total", entered_total.map_or(V::Null, V::Int)),
            ]),
        ),
        (
            "reason",
            s("One general staff member per 100 requested occupants (minimum one) plus one station-based staff member per water or meal station, in that station's zone; an entered total keeps at least one general staff member. Not an operational staffing standard."),
        ),
        ("source_id", s(SYNTHETIC_STAFFING_SOURCE_ID)),
    ])
}

type ZoneDesign = Vec<(String, String, f64)>;

fn share_of(design: &ZoneDesign, zone_id: &str) -> f64 {
    design.iter().find(|item| item.0 == zone_id).map_or(0.0, |item| item.2)
}

/// `_with_accessible_frontage`.
fn with_accessible_frontage(design: &ZoneDesign, width_m: f64) -> ZoneDesign {
    let minimum_share = ACCESSIBLE_ROUTE_MIN_WIDTH_M / width_m;
    let widened: ZoneDesign = design
        .iter()
        .map(|(id, label, share)| (id.clone(), label.clone(), if id == "sleeping" { *share } else { py_max(*share, minimum_share) }))
        .collect();
    let others: Vec<f64> = widened.iter().filter(|item| item.0 != "sleeping").map(|item| item.2).collect();
    let sleeping_share = 1.0 - py_sum(&others);
    if sleeping_share < minimum_share {
        return design.clone();
    }
    widened.into_iter().map(|(id, label, share)| { let share = if id == "sleeping" { sleeping_share } else { share }; (id, label, share) }).collect()
}

/// `_default_gross_area`.
fn default_gross_area(population: i64, design: &ZoneDesign) -> f64 {
    let requested_sleeping_share = share_of(design, "sleeping");
    let mut gross_area = py_max(80.0, population as f64 * 3.5 / py_max(0.01, requested_sleeping_share * 0.9) * 1.1);
    for _ in 0..160 {
        let rounded_area = (gross_area / 10.0).ceil() * 10.0;
        let width_m = (rounded_area * ASPECT_RATIO).sqrt();
        let height_m = rounded_area / width_m;
        let corridor_depth_m = py_min(1.8, height_m * 0.25);
        let widened = with_accessible_frontage(design, width_m);
        let sleeping_share = share_of(&widened, "sleeping");
        let sleeping_usable_area = rounded_area * sleeping_share * (height_m - corridor_depth_m) / height_m * 0.9;
        let frontage = widened.iter().map(|item| width_m * item.2).reduce(py_min).unwrap_or(f64::INFINITY);
        let frontage_met = frontage >= ACCESSIBLE_ROUTE_MIN_WIDTH_M - 1e-9;
        if frontage_met && (sleeping_usable_area / 3.5).floor() >= population as f64 {
            return rounded_area;
        }
        gross_area *= 1.05;
    }
    (gross_area / 10.0).ceil() * 10.0
}

/// `facility_grid_point`.
pub fn facility_grid_point(polygon: &[Pt], index: i64, count: i64) -> Pt {
    let min_x = polygon.iter().map(|point| point.x).reduce(py_min).unwrap_or(0.0);
    let max_x = polygon.iter().map(|point| point.x).reduce(py_max).unwrap_or(0.0);
    let min_y = polygon.iter().map(|point| point.y).reduce(py_min).unwrap_or(0.0);
    let max_y = polygon.iter().map(|point| point.y).reduce(py_max).unwrap_or(0.0);
    let columns = ceil_int((count as f64).sqrt());
    let rows = ceil_int(count as f64 / columns as f64);
    let (column, row) = (index % columns, index / columns);
    let x_fraction = if count == 1 { 0.35 } else { (column + 1) as f64 / (columns + 1) as f64 };
    let y_fraction = 0.68 + 0.28 * (row + 1) as f64 / (rows + 1) as f64;
    Pt { x: min_x + (max_x - min_x) * x_fraction, y: min_y + (max_y - min_y) * y_fraction }
}

fn station_id(kind: &str, index: i64) -> String {
    if index == 1 { format!("{kind}_main") } else { format!("{kind}_{index:02}") }
}

fn facility(id: String, kind: &str, zone_id: &str, working_units: Option<i64>) -> Facility {
    Facility { id, kind: kind.to_owned(), zone_id: zone_id.to_owned(), capacity: 1, working_units }
}

fn invalid(text: impl Into<String>) -> Error {
    Error::Value(text.into())
}

/// `StandardShelterGenerator().generate(FacilityDesignRequest(...))` for a screen request. An error
/// is `Error::Value` with the text of what Python raises (pydantic's field checks are given in
/// their own words, without pydantic's framing).
pub fn generate(request: &FacilityRequest) -> Result<FacilityDesign> {
    if !(1..=10_000).contains(&request.population) {
        return Err(invalid("population must be from 1 to 10000"));
    }
    if let Some(area) = request.gross_area_m2 {
        if !area.is_finite() || area <= 0.0 || area > 100_000.0 {
            return Err(invalid("gross_area_m2 must be finite, above 0 and at most 100000"));
        }
    }
    if !(0..=20).contains(&request.accessible_toilet_count) {
        return Err(invalid("accessible_toilet_count must be from 0 to 20"));
    }
    if request.staff_count.is_some_and(|count| !(0..=100).contains(&count)) {
        return Err(invalid("staff_count must be from 0 to 100"));
    }
    let population = request.population;
    let area_source_ids: Vec<String> = if request.gross_area_m2.is_some() { vec![OPERATOR_AREA_SOURCE_ID.to_owned()] } else { Vec::new() };
    let mut design: ZoneDesign = ZONE_DESIGN.iter().map(|(id, name, share)| ((*id).to_owned(), (*name).to_owned(), *share)).collect();
    let condition_text = casefold(&request.facility_conditions.join(" ")).into_owned();
    let mut recognized: Vec<&str> = Vec::new();
    for (terms, zone_id, name, share) in OPTIONAL_ZONES {
        if terms.iter().any(|term| condition_text.contains(term)) {
            design.push((zone_id.to_owned(), name.to_owned(), share));
            recognized.extend_from_slice(terms);
            if let Some(sleeping) = design.iter_mut().find(|item| item.0 == "sleeping") {
                sleeping.2 -= share;
            }
        }
    }
    // _apply_zone_conditions with no conditions: only its last check.
    let shares: Vec<f64> = design.iter().map(|item| item.2).collect();
    if (py_sum(&shares) - 1.0).abs() > 1e-8 || shares.iter().any(|share| *share <= 0.0) {
        return Err(invalid("facility zone changes must preserve positive shares totaling one"));
    }
    if ACCESSIBLE_TERMS.iter().any(|term| condition_text.contains(term)) {
        recognized.extend_from_slice(&ACCESSIBLE_TERMS);
    }
    let gross_area = match request.gross_area_m2 {
        Some(area) => area,
        None => default_gross_area(population, &design),
    };
    let width_m = (gross_area * ASPECT_RATIO).sqrt();
    let requested: ZoneDesign = design.clone();
    let design = with_accessible_frontage(&design, width_m);
    let widened_zones: Vec<String> = design
        .iter()
        .filter(|(id, _, share)| id != "sleeping" && *share > share_of(&requested, id) + 1e-12)
        .map(|item| item.0.clone())
        .collect();
    let sleeping_share = share_of(&design, "sleeping");
    let height_m = gross_area / width_m;
    let corridor_depth_m = py_min(1.8, height_m * 0.25);
    let room_height_m = height_m - corridor_depth_m;
    let room_fraction = room_height_m / height_m;
    let sleeping_usable_area_m2 = gross_area * sleeping_share * room_fraction * 0.9;
    let area_capacity = (sleeping_usable_area_m2 / 3.5).floor() as i64;
    let capacity_supported = area_capacity;
    let mut warnings: Vec<String> = Vec::new();
    if !widened_zones.is_empty() {
        warnings.push(format!(
            "出入口幅{ACCESSIBLE_ROUTE_MIN_WIDTH_M} mを確保するため、次のゾーンの面積配分を広げ、その分を避難スペースから差し引きました: {}",
            widened_zones.join("、")
        ));
    }
    let unrecognized: Vec<&str> = request
        .facility_conditions
        .iter()
        .filter(|condition| {
            let folded = casefold(condition);
            !recognized.iter().any(|term| folded.contains(term))
        })
        .map(String::as_str)
        .collect();
    if !unrecognized.is_empty() {
        warnings.push(format!("次の施設条件は標準モデルへ自動反映できず、編集時に確認が必要です: {}", unrecognized.join("、")));
    }
    if capacity_supported < population {
        warnings.push(format!(
            "入力人数{population}人に対し、避難スペースの参考収容数は{capacity_supported}人です。実際の利用可能面積を確認してください。"
        ));
    }
    let minimum_route_width = design.iter().map(|item| width_m * item.2).fold(corridor_depth_m, py_min);
    if minimum_route_width < ACCESSIBLE_ROUTE_MIN_WIDTH_M {
        warnings.push(
            "標準モデルの共用通路または居室出入口は1.8 m未満です。一部の生成経路はアクセシブル経路として扱っていません。".to_owned(),
        );
    }
    warnings.push("平面形状、1.8 m通路、面積配分、設備数は標準モデルの仮定です。実施設の図面・法令適合・避難安全性を示しません。".to_owned());
    warnings.push("食料・飲料水の在庫量は施設数から推定していません。在庫を入力しない場合の既定量はScenarioの仮定として記録します。".to_owned());
    let mut sources: Vec<String> = [
        SYNTHETIC_FACILITY_SOURCE_ID,
        SYNTHETIC_CAPACITY_SOURCE_ID,
        SYNTHETIC_STAFFING_SOURCE_ID,
        SYNTHETIC_TOILET_SOURCE_ID,
        SERVICE_STATION_SOURCE_ID,
    ]
    .iter()
    .map(|id| (*id).to_owned())
    .chain(area_source_ids.iter().cloned())
    .collect();
    sources.sort_unstable();
    sources.dedup();
    let mut geometry_source_ids: Vec<String> = std::iter::once(SYNTHETIC_FACILITY_SOURCE_ID.to_owned()).chain(area_source_ids.iter().cloned()).collect();
    geometry_source_ids.sort_unstable();
    geometry_source_ids.dedup();
    let source_kind = if area_source_ids.is_empty() { "synthetic" } else { "inferred" };
    if source_kind == "inferred" {
        warnings.push("選択施設の出典値を使っていますが、長さ・ゾーン・設備配置は実図面ではなく編集可能な概略推定です。".to_owned());
    }

    let mut zones = vec![Zone { id: "corridor".to_owned(), name: "共用通路".to_owned(), capacity: None, display_x: 100.0, display_y: 100.0 }];
    let mut physical_zones = vec![PhysicalZone {
        zone_id: "corridor".to_owned(),
        polygon: vec![Pt { x: 0.0, y: 0.0 }, Pt { x: width_m, y: 0.0 }, Pt { x: width_m, y: corridor_depth_m }, Pt { x: 0.0, y: corridor_depth_m }],
        usable_area_m2: width_m * corridor_depth_m * 0.9,
    }];
    let mut zone_centers: Vec<(String, Pt)> = vec![("corridor".to_owned(), Pt { x: width_m / 2.0, y: corridor_depth_m / 2.0 })];
    let mut x_cursor = 0.0;
    let last = design.len() - 1;
    for (index, (zone_id, name, share)) in design.iter().enumerate() {
        let zone_width = if index < last { width_m * share } else { width_m - x_cursor };
        let left = x_cursor;
        let right = if index == last { width_m } else { x_cursor + zone_width };
        let (top, bottom) = (corridor_depth_m, height_m);
        let area = (right - left) * (bottom - top);
        zones.push(Zone {
            id: zone_id.clone(),
            name: name.clone(),
            capacity: Some(if zone_id == "sleeping" { capacity_supported } else { population }),
            display_x: 100.0,
            display_y: 260.0 + index as f64 * 120.0,
        });
        physical_zones.push(PhysicalZone {
            zone_id: zone_id.clone(),
            polygon: vec![Pt { x: left, y: top }, Pt { x: right, y: top }, Pt { x: right, y: bottom }, Pt { x: left, y: bottom }],
            usable_area_m2: area * 0.9,
        });
        zone_centers.push((zone_id.clone(), Pt { x: (left + right) / 2.0, y: (top + bottom) / 2.0 }));
        x_cursor = right;
    }

    let counts = standard_service_counts(population);
    let mut toilets = (request.accessible_toilet_count + counts.toilet_standard).max(counts.toilet_demand);
    if toilets > 20 {
        warnings.push("標準モデルのトイレ設備は20基で上限化されています。追加設備を手動指定してください。".to_owned());
        toilets = 20;
    }
    let accessible = request.accessible_toilet_count.min(toilets);
    let mut facilities: Vec<Facility> =
        (1..=accessible).map(|index| facility(format!("accessible_toilet_{index:02}"), "toilet", "toilet", Some(1))).collect();
    facilities.extend((1..=toilets - accessible).map(|index| facility(format!("toilet_{index:02}"), "toilet", "toilet", Some(1))));
    facilities.extend((1..=counts.water_station).map(|index| facility(station_id("water_station", index), "water_station", "food", None)));
    facilities.extend((1..=counts.meal_distribution).map(|index| facility(station_id("meal_distribution", index), "meal_distribution", "food", None)));
    facilities.push(facility("medical_desk".to_owned(), "medical_support", "medical", None));
    facilities.push(facility("intake_desk".to_owned(), "intake", "intake", None));
    facilities.push(facility("quiet_support".to_owned(), "quiet_space", "quiet", None));
    for (zone_id, _, _) in &design {
        match zone_id.as_str() {
            "pet" => facilities.push(facility("pet_area".to_owned(), "pet_support", "pet", None)),
            "family" => facilities.push(facility("family_space".to_owned(), "family_support", "family", None)),
            "private" => facilities.push(facility("privacy_space".to_owned(), "privacy_support", "private", None)),
            _ => {}
        }
    }
    facilities.sort_by(|a, b| a.id.cmp(&b.id));

    // _place_facilities: the zones in the order of their first facility, each zone's by ID.
    let mut facility_points: Vec<(String, Pt)> = Vec::with_capacity(facilities.len());
    let mut group_order: Vec<&str> = Vec::new();
    for item in &facilities {
        if !group_order.contains(&item.zone_id.as_str()) {
            group_order.push(&item.zone_id);
        }
    }
    for zone_id in group_order {
        let polygon = &physical_zones.iter().find(|zone| zone.zone_id == zone_id).ok_or_else(|| invalid(format!("'{zone_id}'")))?.polygon;
        let members: Vec<&Facility> = facilities.iter().filter(|item| item.zone_id == zone_id).collect();
        for (index, item) in members.iter().enumerate() {
            facility_points.push((item.id.clone(), facility_grid_point(polygon, index as i64, members.len() as i64)));
        }
    }

    let station_zones = vec!["food"; (counts.water_station + counts.meal_distribution) as usize];
    let staff = shelter_staff(population, request.staff_count, &station_zones, "staff");

    let center_of = |zone_id: &str| zone_centers.iter().find(|item| item.0 == zone_id).map(|item| item.1).unwrap_or(Pt { x: 0.0, y: 0.0 });
    let corridor_point = center_of("corridor");
    let mut edges: Vec<ZoneEdge> = Vec::new();
    let mut routes: Vec<Route> = Vec::new();
    for (zone_id, _, share) in &design {
        let zone_center = center_of(zone_id);
        let doorway_point = Pt { x: zone_center.x, y: corridor_depth_m };
        let route_width = py_min(py_min(1.8, corridor_depth_m), width_m * share);
        let corridor_approach = Pt { x: zone_center.x, y: corridor_point.y };
        let candidates = [zone_center, doorway_point, corridor_approach, corridor_point];
        let mut route_points = vec![candidates[0]];
        for pair in candidates.windows(2) {
            if pair[1] != pair[0] {
                route_points.push(pair[1]);
            }
        }
        for (source, target) in [(zone_id.as_str(), "corridor"), ("corridor", zone_id.as_str())] {
            let edge_id = format!("route_{source}_{target}");
            let points: Vec<Pt> = if source == zone_id { route_points.clone() } else { route_points.iter().rev().copied().collect() };
            let lengths: Vec<f64> = points.windows(2).map(|pair| hypot(&[pair[1].x - pair[0].x, pair[1].y - pair[0].y])).collect();
            let distance_m = py_sum(&lengths);
            edges.push(ZoneEdge {
                id: edge_id.clone(),
                from_zone_id: source.to_owned(),
                to_zone_id: target.to_owned(),
                travel_minutes: ceil_int(distance_m / 60.0).max(1),
            });
            routes.push(Route { edge_id, points, width_m: route_width, accessible: route_width >= ACCESSIBLE_ROUTE_MIN_WIDTH_M });
        }
    }
    edges.sort_by(|a, b| a.id.cmp(&b.id));
    routes.sort_by(|a, b| a.edge_id.cmp(&b.edge_id));
    let mut geometry = Geometry {
        width_m,
        height_m,
        source_kind,
        source_ids: sources,
        zone_source_ids: geometry_source_ids.clone(),
        zones: physical_zones,
        facility_points,
        service_points: Vec::new(),
        routes,
    };
    check_geometry(&geometry)?;
    let editable = standard_editable_geometry(width_m, height_m, corridor_depth_m, &design, &geometry.routes, geometry_source_ids.clone());
    check_editable(&editable)?;
    // with_legacy_service_points: every facility gets one, by ID.
    geometry.service_points = legacy_service_points(&geometry, &facilities);
    check_geometry(&geometry)?;
    validate_editable_routes(&geometry.routes, &editable, &geometry.zones)?;
    if let Some(message) = service_point_issues(&geometry, &facilities, &editable).into_iter().next() {
        return Err(invalid(message));
    }

    let mut observations = Vec::new();
    if request.gross_area_m2.is_some() {
        observations.push(map(vec![
            ("subject", s("facility_gross_area_m2")),
            ("value", V::Float(gross_area)),
            ("source_ids", V::List(area_source_ids.iter().map(|id| s(id)).collect())),
            ("basis", s("operator_entered")),
        ]));
    }
    let minimum_generated_route_width = geometry.routes.iter().map(|route| route.width_m).reduce(py_min).unwrap_or(f64::INFINITY);
    let area_basis = if area_source_ids.is_empty() { "planning_heuristic" } else { "operator_entered" };
    let assumptions = vec![
        map(vec![
            ("id", s(SYNTHETIC_FACILITY_SOURCE_ID)),
            ("subject", s("standard_shelter_layout")),
            (
                "value",
                map(vec![
                    ("gross_area_m2", V::Float(gross_area)),
                    ("area_basis", s(area_basis)),
                    ("sleeping_usable_area_m2", V::Float(sleeping_usable_area_m2)),
                    ("corridor_depth_m", V::Float(corridor_depth_m)),
                    ("aspect_ratio", V::Float(ASPECT_RATIO)),
                    ("zone_shares", V::Map(design.iter().map(|(id, _, share)| (Arc::from(id.as_str()), V::Float(*share))).collect())),
                    ("facility_conditions", V::List(request.facility_conditions.iter().map(|item| s(item)).collect())),
                    ("typed_zone_conditions", V::List(Vec::new())),
                    ("target_accessible_route_width_m", V::Float(1.8)),
                    ("minimum_zone_frontage_m", V::Float(ACCESSIBLE_ROUTE_MIN_WIDTH_M)),
                    ("frontage_widened_zones", V::List(widened_zones.iter().map(|id| s(id)).collect())),
                    ("minimum_generated_route_width_m", V::Float(minimum_generated_route_width)),
                    ("equipment_overrides", V::List(Vec::new())),
                    ("route_speed_m_per_minute", V::Int(60)),
                ]),
            ),
            ("reason", s("Generic rectangular synthetic model for editing; no official or real facility floor plan supplied.")),
            ("source_id", s(SYNTHETIC_FACILITY_SOURCE_ID)),
        ]),
        map(vec![
            ("id", s(SYNTHETIC_CAPACITY_SOURCE_ID)),
            ("subject", s("planning_occupancy_density")),
            (
                "value",
                map(vec![
                    ("area_density_m2_per_person", V::Float(3.5)),
                    ("sleeping_usable_area_m2", V::Float(sleeping_usable_area_m2)),
                    ("area_based_capacity", V::Int(area_capacity)),
                    ("capacity_used", V::Int(capacity_supported)),
                ]),
            ),
            (
                "reason",
                s("Usable sleeping area divided by a visible 3.5 m²/person planning heuristic; not a legal code or suitability determination. When a source capacity is supplied, the lower of the two values is used."),
            ),
            ("source_id", s(SYNTHETIC_CAPACITY_SOURCE_ID)),
        ]),
        shelter_staff_assumption(&staff, request.staff_count),
        map(vec![
            ("id", s(SYNTHETIC_TOILET_SOURCE_ID)),
            ("subject", s("synthetic_toilet_count")),
            (
                "value",
                map(vec![
                    ("accessible_toilets", V::Int(request.accessible_toilet_count)),
                    ("planning_standard_units", V::Int(counts.toilet_standard)),
                    ("demand_units", V::Int(counts.toilet_demand)),
                    ("total", V::Int(toilets)),
                    ("maximum", V::Int(20)),
                ]),
            ),
            (
                "reason",
                s("The larger of the entered accessible toilets plus one per 50 occupants (Cabinet Office initial planning figure) and the units the modeled daily use needs at the target load, capped at 20."),
            ),
            ("source_id", s(SYNTHETIC_TOILET_SOURCE_ID)),
        ]),
        map(vec![
            ("id", s(SERVICE_STATION_SOURCE_ID)),
            ("subject", s("synthetic_service_station_count")),
            ("value", map(vec![("water_station", V::Int(counts.water_station)), ("meal_distribution", V::Int(counts.meal_distribution))])),
            (
                "reason",
                s("Units that keep the modeled daily water and meal demand within the target load (at least one each); no public standard sets station counts."),
            ),
            ("source_id", s(SERVICE_STATION_SOURCE_ID)),
        ]),
    ];
    Ok(FacilityDesign {
        zones,
        zone_edges: edges,
        facilities,
        staff,
        geometry,
        editable_geometry: editable,
        capacity_supported,
        requested_population: population,
        observations,
        assumptions,
        warnings,
    })
}

/// `legacy_service_points` of `contracts/spatial.py` for facilities sorted by ID.
fn legacy_service_points(geometry: &Geometry, facilities: &[Facility]) -> Vec<ServicePoint> {
    let mut sorted: Vec<&Facility> = facilities.iter().collect();
    sorted.sort_by(|a, b| a.id.cmp(&b.id));
    sorted
        .into_iter()
        .filter_map(|item| {
            let point = geometry.facility_points.iter().find(|entry| entry.0 == item.id)?.1;
            Some(ServicePoint {
                id: format!("legacy-service:{}", &sha256_hex(item.id.as_bytes())[..24]),
                facility_id: item.id.clone(),
                point,
                service_capacity: item.working_units.unwrap_or(item.capacity),
            })
        })
        .collect()
}

/// `_standard_editable_geometry`.
fn standard_editable_geometry(width_m: f64, height_m: f64, corridor_depth_m: f64, design: &ZoneDesign, routes: &[Route], source_ids: Vec<String>) -> EditableGeometry {
    let zero = Pt { x: 0.0, y: 0.0 };
    let top_right = Pt { x: width_m, y: 0.0 };
    let bottom_right = Pt { x: width_m, y: height_m };
    let bottom_left = Pt { x: 0.0, y: height_m };
    let wall = |id: String, start: Pt, end: Pt| Wall { wall_id: id, start, end };
    let mut walls = vec![
        wall("exterior_north".to_owned(), zero, top_right),
        wall("exterior_east".to_owned(), top_right, bottom_right),
        wall("exterior_south".to_owned(), bottom_right, bottom_left),
        wall("exterior_west".to_owned(), bottom_left, zero),
        wall("wall_corridor_partition".to_owned(), Pt { x: 0.0, y: corridor_depth_m }, Pt { x: width_m, y: corridor_depth_m }),
    ];
    let mut openings = Vec::new();
    let mut route_openings = Vec::new();
    let mut x_cursor = 0.0;
    for (index, (zone_id, _, share)) in design.iter().enumerate() {
        let zone_width = width_m * share;
        if index > 0 {
            walls.push(wall(format!("partition_zone_{index:02}"), Pt { x: x_cursor, y: corridor_depth_m }, Pt { x: x_cursor, y: height_m }));
        }
        let center_x = x_cursor + zone_width / 2.0;
        let outward = format!("route_{zone_id}_corridor");
        let inward = format!("route_corridor_{zone_id}");
        let route_width = routes.iter().find(|route| route.edge_id == outward).map_or(0.0, |route| route.width_m);
        let opening_id = format!("opening_{zone_id}_corridor");
        let half_width = route_width / 2.0;
        openings.push(Opening {
            opening_id: opening_id.clone(),
            wall_id: "wall_corridor_partition".to_owned(),
            start: Pt { x: py_max(0.0, center_x - half_width), y: corridor_depth_m },
            end: Pt { x: py_min(width_m, center_x + half_width), y: corridor_depth_m },
            width_m: route_width,
        });
        for route in routes {
            if route.edge_id == outward || route.edge_id == inward {
                route_openings.push((route.edge_id.clone(), opening_id.clone()));
            }
        }
        x_cursor += zone_width;
    }
    EditableGeometry { width_m, height_m, source_ids, walls, openings, route_openings }
}

// ---- The checks -------------------------------------------------------------------------------

fn polygon_edges(polygon: &[Pt]) -> impl Iterator<Item = (Pt, Pt)> + '_ {
    polygon.iter().enumerate().map(move |(index, point)| (*point, polygon[(index + 1) % polygon.len()]))
}

fn shoelace(polygon: &[Pt]) -> f64 {
    let terms: Vec<f64> = polygon_edges(polygon).map(|(a, b)| a.x * b.y - b.x * a.y).collect();
    py_sum(&terms)
}

/// `FacilityGeometry.geometry_is_bounded` (no arrival routes).
fn check_geometry(geometry: &Geometry) -> Result<()> {
    let unique = |ids: Vec<&str>| {
        let mut sorted = ids.clone();
        sorted.sort_unstable();
        sorted.dedup();
        sorted.len() == ids.len()
    };
    if !unique(geometry.zones.iter().map(|zone| zone.zone_id.as_str()).collect()) || !unique(geometry.routes.iter().map(|route| route.edge_id.as_str()).collect()) {
        return Err(invalid("geometry IDs must be unique"));
    }
    let inside = |point: &Pt| 0.0 <= point.x && point.x <= geometry.width_m && 0.0 <= point.y && point.y <= geometry.height_m;
    let all_inside = geometry.zones.iter().all(|zone| zone.polygon.iter().all(inside))
        && geometry.routes.iter().all(|route| route.points.iter().all(inside))
        && geometry.facility_points.iter().all(|entry| inside(&entry.1))
        && geometry.service_points.iter().all(|point| inside(&point.point));
    if !all_inside {
        return Err(invalid("physical geometry is outside the building bounds"));
    }
    if !unique(geometry.service_points.iter().map(|point| point.id.as_str()).collect()) {
        return Err(invalid("facility service-point IDs must be unique"));
    }
    if !unique(geometry.service_points.iter().map(|point| point.facility_id.as_str()).collect()) {
        return Err(invalid("each service point must own a unique queue ID"));
    }
    for zone in &geometry.zones {
        let area = shoelace(&zone.polygon).abs() / 2.0;
        if area <= 0.0 || zone.usable_area_m2 > area + 1e-8 {
            return Err(invalid("usable area must not exceed a nondegenerate polygon area"));
        }
    }
    // The fields' own checks (Finite, gt=0).
    if !(geometry.width_m.is_finite() && geometry.width_m > 0.0 && geometry.height_m.is_finite() && geometry.height_m > 0.0)
        || geometry.zones.iter().any(|zone| !(zone.usable_area_m2.is_finite() && zone.usable_area_m2 > 0.0))
        || geometry.routes.iter().any(|route| !(route.width_m.is_finite() && route.width_m > 0.0))
    {
        return Err(invalid("geometry measures must be finite and positive"));
    }
    Ok(())
}

/// `_point_on_segment`.
fn point_on_segment(point: Pt, start: Pt, end: Pt) -> bool {
    let cross = (point.x - start.x) * (end.y - start.y) - (point.y - start.y) * (end.x - start.x);
    let within = py_min(start.x, end.x) - 1e-8 <= point.x
        && point.x <= py_max(start.x, end.x) + 1e-8
        && py_min(start.y, end.y) - 1e-8 <= point.y
        && point.y <= py_max(start.y, end.y) + 1e-8;
    cross.abs() <= 1e-8 && within
}

/// `EditableFacilityGeometry.validate_boundaries` (no obstacles or footprints).
fn check_editable(geometry: &EditableGeometry) -> Result<()> {
    let mut wall_ids: Vec<&str> = geometry.walls.iter().map(|wall| wall.wall_id.as_str()).collect();
    let mut opening_ids: Vec<&str> = geometry.openings.iter().map(|opening| opening.opening_id.as_str()).collect();
    wall_ids.sort_unstable();
    wall_ids.dedup();
    opening_ids.sort_unstable();
    opening_ids.dedup();
    if wall_ids.len() != geometry.walls.len() || opening_ids.len() != geometry.openings.len() {
        return Err(invalid("facility wall and opening IDs must be unique"));
    }
    let bounded = |point: Pt| 0.0 <= point.x && point.x <= geometry.width_m && 0.0 <= point.y && point.y <= geometry.height_m;
    if geometry.walls.iter().any(|wall| !bounded(wall.start) || !bounded(wall.end)) {
        return Err(invalid("facility wall segment is outside the building bounds"));
    }
    if geometry.walls.iter().any(|wall| wall.start == wall.end) {
        return Err(invalid("facility walls must have positive length"));
    }
    let wall_of = |id: &str| geometry.walls.iter().find(|wall| wall.wall_id == id);
    for opening in &geometry.openings {
        if !(opening.width_m.is_finite() && opening.width_m > 0.0 && opening.width_m <= 20.0) {
            return Err(invalid("facility opening width must be above 0 and at most 20"));
        }
        let Some(wall) = wall_of(&opening.wall_id) else {
            return Err(invalid(format!("opening '{}' references an unknown wall", opening.opening_id)));
        };
        if !bounded(opening.start) || !bounded(opening.end) {
            return Err(invalid("facility opening is outside the building bounds"));
        }
        if !point_on_segment(opening.start, wall.start, wall.end) || !point_on_segment(opening.end, wall.start, wall.end) {
            return Err(invalid("facility opening endpoints must lie on their host wall"));
        }
        let length = hypot(&[opening.end.x - opening.start.x, opening.end.y - opening.start.y]);
        if (length - opening.width_m).abs() > py_max(1e-6, opening.width_m * 1e-6) {
            return Err(invalid("facility opening width must match its drawn opening segment"));
        }
    }
    let interval = |opening: &Opening, wall: &Wall, axis: (f64, f64)| {
        let a = (opening.start.x - wall.start.x) * axis.0 + (opening.start.y - wall.start.y) * axis.1;
        let b = (opening.end.x - wall.start.x) * axis.0 + (opening.end.y - wall.start.y) * axis.1;
        if b < a { (b, a) } else { (a, b) }
    };
    for (index, first) in geometry.openings.iter().enumerate() {
        let Some(wall) = wall_of(&first.wall_id) else { continue };
        let axis_length = hypot(&[wall.end.x - wall.start.x, wall.end.y - wall.start.y]);
        let axis = ((wall.end.x - wall.start.x) / axis_length, (wall.end.y - wall.start.y) / axis_length);
        let first_interval = interval(first, wall, axis);
        for second in &geometry.openings[index + 1..] {
            if second.wall_id != first.wall_id {
                continue;
            }
            let second_interval = interval(second, wall, axis);
            if py_min(first_interval.1, second_interval.1) > py_max(first_interval.0, second_interval.0) + 1e-8 {
                return Err(invalid("facility openings on the same wall cannot overlap"));
            }
        }
    }
    let mut route_ids: Vec<&str> = geometry.route_openings.iter().map(|item| item.0.as_str()).collect();
    route_ids.sort_unstable();
    route_ids.dedup();
    if route_ids.len() != geometry.route_openings.len() {
        return Err(invalid("a physical route can reference only one opening in this draft"));
    }
    if geometry.route_openings.iter().any(|item| !geometry.openings.iter().any(|opening| opening.opening_id == item.1)) {
        return Err(invalid("route opening reference points to an unknown opening"));
    }
    Ok(())
}

/// `_segment_intersection_points`.
fn segment_intersection_points(first_start: Pt, first_end: Pt, second_start: Pt, second_end: Pt) -> Vec<Pt> {
    let cross = |ax: f64, ay: f64, bx: f64, by: f64| ax * by - ay * bx;
    let (rx, ry) = (first_end.x - first_start.x, first_end.y - first_start.y);
    let (sx, sy) = (second_end.x - second_start.x, second_end.y - second_start.y);
    let denominator = cross(rx, ry, sx, sy);
    let (qpx, qpy) = (second_start.x - first_start.x, second_start.y - first_start.y);
    if denominator.abs() <= 1e-9 {
        if cross(qpx, qpy, rx, ry).abs() > 1e-9 {
            return Vec::new();
        }
        let mut shared: Vec<Pt> = Vec::new();
        for point in [first_start, first_end, second_start, second_end] {
            if point_on_segment(point, first_start, first_end) && point_on_segment(point, second_start, second_end) && !shared.contains(&point) {
                shared.push(point);
            }
        }
        return shared;
    }
    let t = cross(qpx, qpy, sx, sy) / denominator;
    let u = cross(qpx, qpy, rx, ry) / denominator;
    if (-1e-9..=1.0 + 1e-9).contains(&t) && (-1e-9..=1.0 + 1e-9).contains(&u) {
        return vec![Pt { x: first_start.x + t * rx, y: first_start.y + t * ry }];
    }
    Vec::new()
}

/// `_point_segment_distance`.
fn point_segment_distance(point: Pt, start: Pt, end: Pt) -> f64 {
    let (dx, dy) = (end.x - start.x, end.y - start.y);
    let length_squared = dx * dx + dy * dy;
    if length_squared <= 1e-18 {
        return hypot(&[point.x - start.x, point.y - start.y]);
    }
    let fraction = py_max(0.0, py_min(1.0, ((point.x - start.x) * dx + (point.y - start.y) * dy) / length_squared));
    let closest = Pt { x: start.x + fraction * dx, y: start.y + fraction * dy };
    hypot(&[point.x - closest.x, point.y - closest.y])
}

/// `_segment_distance`.
fn segment_distance(start: Pt, end: Pt, other_start: Pt, other_end: Pt) -> f64 {
    if !segment_intersection_points(start, end, other_start, other_end).is_empty() {
        return 0.0;
    }
    [
        point_segment_distance(start, other_start, other_end),
        point_segment_distance(end, other_start, other_end),
        point_segment_distance(other_start, start, end),
        point_segment_distance(other_end, start, end),
    ]
    .into_iter()
    .reduce(py_min)
    .unwrap_or(0.0)
}

/// `_valid_simple_polygon`.
fn valid_simple_polygon(polygon: &[Pt]) -> bool {
    if polygon.len() < 3 || shoelace(polygon).abs() <= 1e-9 {
        return false;
    }
    let edges: Vec<(Pt, Pt)> = polygon_edges(polygon).collect();
    let count = edges.len();
    for (index, (start, end)) in edges.iter().enumerate() {
        for (other_index, (other_start, other_end)) in edges.iter().enumerate().skip(index + 1) {
            if other_index == index + 1 || other_index == (index + count - 1) % count {
                continue;
            }
            if !segment_intersection_points(*start, *end, *other_start, *other_end).is_empty() {
                return false;
            }
        }
    }
    true
}

fn point_on_polygon_boundary(point: Pt, polygon: &[Pt]) -> bool {
    polygon_edges(polygon).any(|(start, end)| point_on_segment(point, start, end))
}

fn point_strictly_inside_polygon(point: Pt, polygon: &[Pt]) -> bool {
    point_in_polygon(point, polygon) && !point_on_polygon_boundary(point, polygon)
}

/// `_properly_crosses`.
fn properly_crosses(first_start: Pt, first_end: Pt, second_start: Pt, second_end: Pt) -> bool {
    segment_intersection_points(first_start, first_end, second_start, second_end).into_iter().any(|point| {
        [first_start, first_end, second_start, second_end].iter().all(|endpoint| hypot(&[point.x - endpoint.x, point.y - endpoint.y]) > 1e-8)
    })
}

/// `_validate_physical_zone_polygons`.
fn validate_physical_zone_polygons(zones: &[PhysicalZone]) -> Result<()> {
    for zone in zones {
        if !valid_simple_polygon(&zone.polygon) {
            return Err(invalid(format!("walkable zone '{}' must have a simple positive-area polygon", zone.zone_id)));
        }
    }
    for (index, first) in zones.iter().enumerate() {
        for second in &zones[index + 1..] {
            let same_vertices = first.polygon.iter().all(|point| second.polygon.contains(point)) && second.polygon.iter().all(|point| first.polygon.contains(point));
            let contained = first.polygon.iter().any(|point| point_strictly_inside_polygon(*point, &second.polygon))
                || second.polygon.iter().any(|point| point_strictly_inside_polygon(*point, &first.polygon));
            let crossing = polygon_edges(&first.polygon)
                .any(|(start, end)| polygon_edges(&second.polygon).any(|(other_start, other_end)| properly_crosses(start, end, other_start, other_end)));
            if same_vertices || contained || crossing {
                return Err(invalid(format!("walkable zones '{}' and '{}' overlap in area", first.zone_id, second.zone_id)));
            }
        }
    }
    Ok(())
}

/// The parameters along `start`..`end` where it meets the given edges, as both checks below
/// collect them: 0, 1 and each crossing's fraction, clamped, rounded to 12 places, deduplicated,
/// sorted.
fn crossing_parameters<'a>(start: Pt, end: Pt, edges: impl Iterator<Item = (Pt, Pt)> + 'a) -> Vec<f64> {
    let (dx, dy) = (end.x - start.x, end.y - start.y);
    let mut parameters = vec![0.0, 1.0];
    for (first, second) in edges {
        for point in segment_intersection_points(start, end, first, second) {
            let fraction = if dx.abs() >= dy.abs() && dx.abs() > 1e-12 {
                (point.x - start.x) / dx
            } else if dy.abs() > 1e-12 {
                (point.y - start.y) / dy
            } else {
                0.0
            };
            if (-1e-9..=1.0 + 1e-9).contains(&fraction) {
                parameters.push(py_max(0.0, py_min(1.0, fraction)));
            }
        }
    }
    let mut ordered: Vec<f64> = parameters.into_iter().map(|value| py_round(value, 12).unwrap_or(value)).collect();
    ordered.sort_by(|a, b| a.total_cmp(b));
    ordered.dedup_by(|a, b| a == b);
    ordered
}

/// `_segment_inside_polygon_union`.
fn segment_inside_polygon_union(start: Pt, end: Pt, zones: &[PhysicalZone]) -> bool {
    let (dx, dy) = (end.x - start.x, end.y - start.y);
    if !zones.iter().any(|zone| point_in_polygon(start, &zone.polygon)) || !zones.iter().any(|zone| point_in_polygon(end, &zone.polygon)) {
        return false;
    }
    let ordered = crossing_parameters(start, end, zones.iter().flat_map(|zone| polygon_edges(&zone.polygon)));
    let midpoints: Vec<f64> = ordered.windows(2).map(|pair| (pair[0] + pair[1]) / 2.0).collect();
    ordered.iter().chain(midpoints.iter()).all(|t| {
        let point = Pt { x: start.x + t * dx, y: start.y + t * dy };
        zones.iter().any(|zone| point_in_polygon(point, &zone.polygon))
    })
}

/// `_segment_has_polygon_clearance`.
fn segment_has_polygon_clearance(start: Pt, end: Pt, clearance_m: f64, zones: &[PhysicalZone]) -> bool {
    for zone in zones {
        for (first, second) in polygon_edges(&zone.polygon) {
            let (dx, dy) = (second.x - first.x, second.y - first.y);
            let others = zones.iter().filter(|other| other.zone_id != zone.zone_id).flat_map(|other| polygon_edges(&other.polygon));
            let ordered = crossing_parameters(first, second, others);
            for pair in ordered.windows(2) {
                let (lower, upper) = (pair[0], pair[1]);
                let midpoint_t = (lower + upper) / 2.0;
                let midpoint = Pt { x: first.x + midpoint_t * dx, y: first.y + midpoint_t * dy };
                if zones.iter().any(|other| other.zone_id != zone.zone_id && point_in_polygon(midpoint, &other.polygon)) {
                    continue;
                }
                let boundary_start = Pt { x: first.x + lower * dx, y: first.y + lower * dy };
                let boundary_end = Pt { x: first.x + upper * dx, y: first.y + upper * dy };
                if segment_distance(start, end, boundary_start, boundary_end) < clearance_m - 1e-8 {
                    return false;
                }
            }
        }
    }
    true
}

/// `validate_editable_routes` (the generator's geometry has no obstacles or footprints).
fn validate_editable_routes(routes: &[Route], geometry: &EditableGeometry, zones: &[PhysicalZone]) -> Result<()> {
    if !zones.is_empty() {
        validate_physical_zone_polygons(zones)?;
    }
    for (route_id, opening_id) in &geometry.route_openings {
        if !routes.iter().any(|route| &route.edge_id == route_id) {
            return Err(invalid(format!("opening mapping references unknown route '{route_id}'")));
        }
        if !geometry.openings.iter().any(|opening| &opening.opening_id == opening_id) {
            return Err(invalid(format!("route opening mapping references unknown opening '{opening_id}'")));
        }
    }
    for route in routes {
        let uses: Vec<&Opening> = geometry
            .route_openings
            .iter()
            .filter(|item| item.0 == route.edge_id)
            .filter_map(|item| geometry.openings.iter().find(|opening| opening.opening_id == item.1))
            .collect();
        if route.points.iter().any(|point| !(0.0 <= point.x && point.x <= geometry.width_m) || !(0.0 <= point.y && point.y <= geometry.height_m)) {
            return Err(invalid(format!("route '{}' leaves the facility bounds", route.edge_id)));
        }
        if route.points.windows(2).any(|pair| pair[0] == pair[1]) {
            return Err(invalid(format!("route '{}' contains a zero-length segment", route.edge_id)));
        }
        if route.accessible && route.width_m < ACCESSIBLE_ROUTE_MIN_WIDTH_M {
            return Err(invalid(format!("accessible route '{}' is narrower than 1.8 m", route.edge_id)));
        }
        for opening in &uses {
            if route.width_m > opening.width_m + 1e-8 {
                return Err(invalid(format!("route '{}' is wider than opening '{}'", route.edge_id, opening.opening_id)));
            }
        }
        for pair in route.points.windows(2) {
            let (start, end) = (pair[0], pair[1]);
            if !zones.is_empty() && !segment_inside_polygon_union(start, end, zones) {
                return Err(invalid(format!("route '{}' leaves the facility's walkable zone polygons", route.edge_id)));
            }
            if !zones.is_empty() && !segment_has_polygon_clearance(start, end, route.width_m / 2.0, zones) {
                return Err(invalid(format!("route '{}' does not fit within the walkable polygon boundary clearance", route.edge_id)));
            }
            for wall in &geometry.walls {
                let intersections = segment_intersection_points(start, end, wall.start, wall.end);
                if intersections.is_empty() {
                    if segment_distance(start, end, wall.start, wall.end) < route.width_m / 2.0 - 1e-8 {
                        return Err(invalid(format!("route '{}' lacks width clearance from wall '{}'", route.edge_id, wall.wall_id)));
                    }
                    continue;
                }
                let valid: Vec<&&Opening> = uses
                    .iter()
                    .filter(|opening| opening.wall_id == wall.wall_id && intersections.iter().all(|point| point_on_segment(*point, opening.start, opening.end)))
                    .collect();
                if valid.is_empty() {
                    return Err(invalid(format!("route '{}' crosses closed wall '{}'", route.edge_id, wall.wall_id)));
                }
                let (route_dx, route_dy) = (end.x - start.x, end.y - start.y);
                let (wall_dx, wall_dy) = (wall.end.x - wall.start.x, wall.end.y - wall.start.y);
                let route_length = hypot(&[route_dx, route_dy]);
                let wall_length = hypot(&[wall_dx, wall_dy]);
                let crossing_sine = (route_dx * wall_dy - route_dy * wall_dx).abs() / (route_length * wall_length);
                if crossing_sine <= 1e-9 {
                    return Err(invalid(format!("route '{}' follows wall '{}' instead of crossing it", route.edge_id, wall.wall_id)));
                }
                let required_opening = route.width_m / crossing_sine;
                if !valid.iter().any(|opening| opening.width_m + 1e-8 >= required_opening) {
                    return Err(invalid(format!(
                        "route '{}' is too oblique for the available opening in wall '{}'",
                        route.edge_id, wall.wall_id
                    )));
                }
            }
        }
    }
    Ok(())
}

/// `validate_facility_service_points`' messages for the generator's geometry: every service point
/// is a legacy one of unknown access (so the accessible-approach check never runs), and there are
/// no obstacles or footprints.
fn service_point_issues(geometry: &Geometry, facilities: &[Facility], editable: &EditableGeometry) -> Vec<String> {
    let mut issues = Vec::new();
    let mut capacity: IndexMap<&str, i64> = IndexMap::new();
    for point in &geometry.service_points {
        let Some(facility) = facilities.iter().find(|item| item.id == point.facility_id) else {
            issues.push(format!("Service point '{}' references unknown facility '{}'.", point.id, point.facility_id));
            continue;
        };
        *capacity.entry(facility.id.as_str()).or_insert(0) += point.service_capacity;
        let zone = geometry.zones.iter().find(|zone| zone.zone_id == facility.zone_id);
        if zone.is_none_or(|zone| !point_in_polygon(point.point, &zone.polygon)) {
            issues.push(format!("Service point '{}' must be inside facility '{}''s zone '{}'.", point.id, facility.id, facility.zone_id));
        }
        if editable.walls.iter().any(|wall| point_segment_distance(point.point, wall.start, wall.end) <= 1e-8) {
            issues.push(format!("Service point '{}' lies on a wall; place the service location in walkable space.", point.id));
        }
    }
    for facility in facilities {
        let declared = facility.working_units.unwrap_or(facility.capacity);
        if capacity.get(facility.id.as_str()).copied().unwrap_or(0) > declared {
            issues.push(format!(
                "Service points for facility '{}' exceed its declared working-unit capacity {declared}.",
                facility.id
            ));
        }
    }
    issues
}

// ---- The dump ---------------------------------------------------------------------------------

fn s(text: &str) -> V {
    V::Str(Arc::from(text))
}

fn map(entries: Vec<(&str, V)>) -> V {
    V::Map(entries.into_iter().map(|(key, value)| (Arc::from(key), value)).collect())
}

fn point(point: Pt) -> V {
    map(vec![("x", V::Float(point.x)), ("y", V::Float(point.y))])
}

fn strings(values: &[String]) -> V {
    V::List(values.iter().map(|value| s(value)).collect())
}

impl Zone {
    pub fn dump(&self) -> V {
        map(vec![
            ("id", s(&self.id)),
            ("name", s(&self.name)),
            ("capacity", self.capacity.map_or(V::Null, V::Int)),
            ("display_x", V::Float(self.display_x)),
            ("display_y", V::Float(self.display_y)),
        ])
    }
}

impl ZoneEdge {
    pub fn dump(&self) -> V {
        map(vec![
            ("id", s(&self.id)),
            ("from_zone_id", s(&self.from_zone_id)),
            ("to_zone_id", s(&self.to_zone_id)),
            ("travel_minutes", V::Int(self.travel_minutes)),
            ("closed", V::Bool(false)),
        ])
    }
}

impl Facility {
    pub fn dump(&self) -> V {
        map(vec![
            ("id", s(&self.id)),
            ("kind", s(&self.kind)),
            ("zone_id", s(&self.zone_id)),
            ("capacity", V::Int(self.capacity)),
            ("working_units", self.working_units.map_or(V::Null, V::Int)),
        ])
    }
}

impl Staff {
    pub fn dump(&self) -> V {
        map(vec![("id", s(&self.id)), ("zone_id", s(&self.zone_id)), ("skills", V::List(self.skills.iter().map(|skill| s(skill)).collect()))])
    }
}

impl Geometry {
    /// `FacilityGeometry.model_dump(mode="json")`.
    pub fn dump(&self) -> V {
        let zone_sources = strings(&self.zone_source_ids);
        map(vec![
            ("schema_version", V::Int(1)),
            ("units", s("m")),
            ("origin", s("top_left")),
            ("width_m", V::Float(self.width_m)),
            ("height_m", V::Float(self.height_m)),
            ("source_kind", s(self.source_kind)),
            ("source_ids", strings(&self.source_ids)),
            (
                "zones",
                V::List(
                    self.zones
                        .iter()
                        .map(|zone| {
                            map(vec![
                                ("zone_id", s(&zone.zone_id)),
                                ("polygon", V::List(zone.polygon.iter().map(|p| point(*p)).collect())),
                                ("usable_area_m2", V::Float(zone.usable_area_m2)),
                                ("source_ids", zone_sources.clone()),
                            ])
                        })
                        .collect(),
                ),
            ),
            ("facility_points", V::Map(self.facility_points.iter().map(|(id, p)| (Arc::from(id.as_str()), point(*p))).collect())),
            (
                "service_points",
                V::List(
                    self.service_points
                        .iter()
                        .map(|item| {
                            map(vec![
                                ("schema_version", V::Int(1)),
                                ("id", s(&item.id)),
                                ("facility_id", s(&item.facility_id)),
                                ("point_m", point(item.point)),
                                (
                                    "access",
                                    map(vec![
                                        ("accessibility", s("unknown")),
                                        ("required_access_tags", V::List(Vec::new())),
                                        ("required_staff_skills", V::List(Vec::new())),
                                    ]),
                                ),
                                ("service_capacity", V::Int(item.service_capacity)),
                                ("queue_id", s(&item.facility_id)),
                            ])
                        })
                        .collect(),
                ),
            ),
            (
                "routes",
                V::List(
                    self.routes
                        .iter()
                        .map(|route| {
                            map(vec![
                                ("edge_id", s(&route.edge_id)),
                                ("points", V::List(route.points.iter().map(|p| point(*p)).collect())),
                                ("width_m", V::Float(route.width_m)),
                                ("accessible", V::Bool(route.accessible)),
                            ])
                        })
                        .collect(),
                ),
            ),
            ("arrival_approach_routes", V::List(Vec::new())),
        ])
    }
}

impl EditableGeometry {
    /// `EditableFacilityGeometry.model_dump(mode="json")`.
    pub fn dump(&self) -> V {
        let sources = strings(&self.source_ids);
        map(vec![
            ("schema_version", V::Int(1)),
            ("width_m", V::Float(self.width_m)),
            ("height_m", V::Float(self.height_m)),
            (
                "walls",
                V::List(
                    self.walls
                        .iter()
                        .map(|wall| map(vec![("wall_id", s(&wall.wall_id)), ("start", point(wall.start)), ("end", point(wall.end)), ("source_ids", sources.clone())]))
                        .collect(),
                ),
            ),
            (
                "openings",
                V::List(
                    self.openings
                        .iter()
                        .map(|opening| {
                            map(vec![
                                ("opening_id", s(&opening.opening_id)),
                                ("wall_id", s(&opening.wall_id)),
                                ("start", point(opening.start)),
                                ("end", point(opening.end)),
                                ("width_m", V::Float(opening.width_m)),
                                ("opening_kind", s("doorway")),
                                ("source_ids", sources.clone()),
                            ])
                        })
                        .collect(),
                ),
            ),
            ("obstacles", V::List(Vec::new())),
            ("route_openings", V::List(self.route_openings.iter().map(|(route, opening)| map(vec![("route_id", s(route)), ("opening_id", s(opening))])).collect())),
            ("equipment_footprints", V::List(Vec::new())),
        ])
    }
}

impl FacilityDesign {
    /// `FacilityDesign.model_dump(mode="json")`.
    pub fn dump(&self) -> V {
        map(vec![
            ("zones", V::List(self.zones.iter().map(Zone::dump).collect())),
            ("zone_edges", V::List(self.zone_edges.iter().map(ZoneEdge::dump).collect())),
            ("facilities", V::List(self.facilities.iter().map(Facility::dump).collect())),
            ("staff", V::List(self.staff.iter().map(Staff::dump).collect())),
            ("geometry", self.geometry.dump()),
            ("editable_geometry", self.editable_geometry.dump()),
            ("capacity_supported", V::Int(self.capacity_supported)),
            ("requested_population", V::Int(self.requested_population)),
            ("observations", V::List(self.observations.clone())),
            ("assumptions", V::List(self.assumptions.clone())),
            ("warnings", V::List(self.warnings.iter().map(|text| s(text)).collect())),
        ])
    }
}
