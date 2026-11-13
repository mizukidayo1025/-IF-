//! `InitialScenarioBuilder.build` of `experiments/initial_scenario.py` for a request of the
//! Scenario screen, and what `ProjectWorkspace.preview` does to its Scenario before it answers
//! (Issue #299): the provenance, the model configs, the Scenario's ID, the address removal, the
//! `scenario_input` record, the canonical JSON and its sha256 (`approved_hash`).
//!
//! `assemble` joins parts made elsewhere: the request's dump (#295), the regional research as
//! the server dumps it (#300), the standard shelter (`facility::generate`, #296), the synthetic
//! population's dump (#297) and the compiled operational conditions, the generated context and
//! the service-capacity warnings (#298). What no screen request can change is held as constants
//! (`constants.json`, written by `native/core/tools/scenario_constants.py` from Python's own
//! values): the model configs every such request gets and the IDs Python's modules define.
//!
//! Only the default policies are built (the browser refuses others before, #295); a plan, a
//! selected shelter, typed zone changes and coefficient overrides are not part of a screen
//! request. Python's code order is kept wherever it decides a list's order or which record wins.

use std::collections::BTreeMap;
use std::sync::{Arc, OnceLock};

use indexmap::IndexMap;

use crate::checkpoint::sha256_hex;
use crate::error::{Error, Result};
use crate::json::{python_float_repr, write_json_string};
use crate::mathx::py_round_int;
use crate::scenario::facility::{FacilityDesign, ACCESSIBLE_ROUTE_MIN_WIDTH_M};
use crate::text::casefold;
use crate::value::V;

const CONSTANTS_JSON: &str = include_str!("constants.json");

/// The builder's constants (`constants.json`).
pub fn constants() -> &'static V {
    static CONSTANTS: OnceLock<V> = OnceLock::new();
    CONSTANTS.get_or_init(|| {
        serde_json::from_str::<serde_json::Value>(CONSTANTS_JSON).ok().and_then(|value| V::from_json(&value)).unwrap_or(V::Null)
    })
}

fn id_of(name: &str) -> Result<String> {
    text(at(constants(), &["ids", name])?, name)
}

/// The compiled operational conditions (`with_standard_provisioning` and
/// `OperationalConditions.compile`) and what the request's own conditions say of their sources.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Conditions {
    /// `InitialStock.model_dump(mode="json")`, or None.
    pub initial_stock: Option<V>,
    /// Each `PlannedEventSpec.model_dump(mode="json")`, in the compiled order.
    pub scheduled_events: Vec<V>,
    /// The compiled model configs, in their order.
    pub model_configs: Vec<(String, V)>,
    pub blockers: Vec<String>,
    /// `provenance_sources()` of the conditions after provisioning.
    pub provenance_sources: Vec<V>,
    /// The provisioning assumption, if the standard provisioning was applied.
    pub provisioning: Option<V>,
    /// `request.operational_conditions.provenance_sources()`.
    pub request_provenance_sources: Vec<V>,
    /// `request.operational_conditions.referenced_source_ids()`.
    pub request_referenced_source_ids: Vec<String>,
}

/// What the screen's text or form was (`provenance.scenario_input`).
#[derive(Clone, Debug, PartialEq)]
pub struct ScenarioInput {
    pub sha256: String,
    pub extraction_source: String,
}

/// The parts `assemble` joins.
pub struct Parts<'a> {
    /// `InitialScenarioRequest.model_dump(mode="json")`.
    pub request: &'a V,
    /// `RegionResearchResult.model_dump(mode="json")`.
    pub research: &'a V,
    pub design: &'a FacilityDesign,
    /// `GeneratedPopulation.model_dump(mode="json")` (at least the keys the builder reads).
    pub population: &'a V,
    pub conditions: &'a Conditions,
    /// `connect_generated_context(population, editable, geometry)`, in its order.
    pub connections: &'a [(String, V)],
    pub input: &'a ScenarioInput,
    /// Whether the request's JSON gave `duration_days`. Python keeps an absent one at its
    /// default, the int 7 (pydantic does not validate defaults), which the defaults' assumption
    /// records as `7`; the request's dump writes `7.0` either way.
    pub duration_days_given: bool,
}

/// A built Scenario as the preview answers it.
#[derive(Clone, Debug, PartialEq)]
pub struct Built {
    /// The Scenario after `preview`'s changes, as `ScenarioSpec.model_dump(mode="json")`.
    pub scenario: V,
    /// Its canonical JSON (`ScenarioSpec.canonical_json()`); its sha256 is `approved_hash`.
    pub canonical: Vec<u8>,
    pub approved_hash: String,
    /// The design's warnings, the research's and then the service-capacity diagnostic's.
    pub warnings: Vec<String>,
    pub approval_blockers: Vec<String>,
    pub can_start: bool,
}

// ---- Reading values ---------------------------------------------------------------------------

fn at<'v>(value: &'v V, path: &[&str]) -> Result<&'v V> {
    let mut current = value;
    for key in path {
        current = match current {
            V::Map(map) => map.get(*key).ok_or_else(|| Error::Value(format!("missing {}", path.join("."))))?,
            _ => return Err(Error::Value(format!("{} is not a mapping", path.join(".")))),
        };
    }
    Ok(current)
}

fn opt<'v>(value: &'v V, key: &str) -> Option<&'v V> {
    match value {
        V::Map(map) => map.get(key).filter(|item| !matches!(item, V::Null)),
        _ => None,
    }
}

fn text(value: &V, what: &str) -> Result<String> {
    match value {
        V::Str(text) => Ok(text.to_string()),
        _ => Err(Error::Value(format!("{what} must be a string"))),
    }
}

fn int(value: &V, what: &str) -> Result<i64> {
    match value {
        V::Int(number) => Ok(*number),
        _ => Err(Error::Value(format!("{what} must be an int"))),
    }
}

fn list<'v>(value: &'v V, what: &str) -> Result<&'v [V]> {
    match value {
        V::List(items) => Ok(items),
        _ => Err(Error::Value(format!("{what} must be a list"))),
    }
}

fn texts(value: &V, what: &str) -> Result<Vec<String>> {
    list(value, what)?.iter().map(|item| text(item, what)).collect()
}

fn s(text: &str) -> V {
    V::Str(Arc::from(text))
}

fn map(entries: Vec<(&str, V)>) -> V {
    V::Map(entries.into_iter().map(|(key, value)| (Arc::from(key), value)).collect())
}

fn insert(target: &mut V, key: &str, value: V) {
    if let V::Map(map) = target {
        map.insert(Arc::from(key), value);
    }
}

fn record_id(record: &V) -> String {
    opt(record, "id").map(|id| match id {
        V::Str(text) => text.to_string(),
        other => format!("{other:?}"),
    })
    .unwrap_or_default()
}

/// An assumption-kind source record of an assumption (`{"id", "title": subject, ...}`).
fn assumption_record(assumption: &V) -> Result<V> {
    Ok(map(vec![
        ("id", at(assumption, &["source_id"])?.clone()),
        ("title", s(&text(at(assumption, &["subject"])?, "subject")?)),
        ("source_kind", s("assumption")),
        ("description", s(&text(at(assumption, &["reason"])?, "reason")?)),
    ]))
}

// ---- Python's text forms ----------------------------------------------------------------------

/// `json.dumps(value, sort_keys=True, ensure_ascii=False)`: the default separators `", "` and
/// `": "`, keys sorted by code point (the scenario ID's hash).
pub fn spaced_json(value: &V) -> Result<String> {
    let mut out: Vec<u8> = Vec::with_capacity(4096);
    write_spaced(value, &mut out)?;
    String::from_utf8(out).map_err(|_| Error::Value("not UTF-8".to_owned()))
}

fn write_spaced(value: &V, out: &mut Vec<u8>) -> Result<()> {
    match value {
        V::Null => out.extend_from_slice(b"null"),
        V::Bool(flag) => out.extend_from_slice(if *flag { b"true" } else { b"false" }),
        V::Int(number) => out.extend_from_slice(number.to_string().as_bytes()),
        V::Big(digits) => out.extend_from_slice(digits.as_bytes()),
        V::Float(number) => {
            if !number.is_finite() {
                return Err(Error::Value("a float of the Scenario is not finite".to_owned()));
            }
            out.extend_from_slice(python_float_repr(*number).as_bytes());
        }
        V::Str(text) => write_json_string(out, text),
        V::List(items) => {
            out.push(b'[');
            for (index, item) in items.iter().enumerate() {
                if index > 0 {
                    out.extend_from_slice(b", ");
                }
                write_spaced(item, out)?;
            }
            out.push(b']');
        }
        V::Map(entries) => {
            let mut sorted: Vec<(&Arc<str>, &V)> = entries.iter().collect();
            sorted.sort_by(|a, b| a.0.as_bytes().cmp(b.0.as_bytes()));
            out.push(b'{');
            for (index, (key, item)) in sorted.into_iter().enumerate() {
                if index > 0 {
                    out.extend_from_slice(b", ");
                }
                write_json_string(out, key);
                out.extend_from_slice(b": ");
                write_spaced(item, out)?;
            }
            out.push(b'}');
        }
    }
    Ok(())
}

/// `datetime.isoformat()` of a datetime pydantic dumped (`...Z` for UTC).
fn isoformat(dumped: &str) -> String {
    match dumped.strip_suffix('Z') {
        Some(rest) => format!("{rest}+00:00"),
        None => dumped.to_owned(),
    }
}

/// `_disaster_families`.
fn disaster_families(value: &str) -> Vec<String> {
    const GROUPS: [(&str, &[&str]); 8] = [
        ("flood", &["flood", "水害", "洪水", "浸水", "氾濫"]),
        ("earthquake", &["earthquake", "地震", "震災"]),
        ("tsunami", &["tsunami", "津波"]),
        ("landslide", &["landslide", "土砂", "崖崩れ", "がけ崩れ"]),
        ("storm_surge", &["storm_surge", "storm surge", "高潮"]),
        ("typhoon", &["typhoon", "台風"]),
        ("heavy_snow", &["heavy_snow", "heavy snow", "大雪", "豪雪"]),
        ("heatwave", &["heatwave", "heat wave", "猛暑", "熱波"]),
    ];
    let folded = casefold(value).into_owned();
    let mut families: Vec<String> =
        GROUPS.iter().filter(|(_, terms)| terms.iter().any(|term| folded.contains(term))).map(|(family, _)| (*family).to_owned()).collect();
    if families.is_empty() {
        families.push(py_strip(&folded).to_owned());
    }
    families.sort();
    families
}

/// `str.strip()` with no argument: Python's whitespace (what `char::is_whitespace` and the
/// separators U+001C..U+001F take).
fn py_strip(text: &str) -> &str {
    text.trim_matches(|c: char| c.is_whitespace() || ('\u{1c}'..='\u{1f}').contains(&c))
}

/// `_inaccessible_service_zones`.
fn inaccessible_service_zones(design: &FacilityDesign) -> Vec<String> {
    let accessible: Vec<&str> = design
        .geometry
        .routes
        .iter()
        .filter(|route| route.accessible && route.width_m >= ACCESSIBLE_ROUTE_MIN_WIDTH_M)
        .map(|route| route.edge_id.as_str())
        .collect();
    let edges: Vec<_> = design.zone_edges.iter().filter(|edge| accessible.contains(&edge.id.as_str())).collect();
    let spread = |forward: bool| {
        let mut reached: Vec<&str> = vec!["sleeping"];
        loop {
            let mut grew = false;
            for edge in &edges {
                let (from, to) = if forward { (&edge.from_zone_id, &edge.to_zone_id) } else { (&edge.to_zone_id, &edge.from_zone_id) };
                if reached.contains(&from.as_str()) && !reached.contains(&to.as_str()) {
                    reached.push(to);
                    grew = true;
                }
            }
            if !grew {
                return reached;
            }
        }
    };
    let reachable = spread(true);
    let returnable = spread(false);
    let mut core: Vec<String> = design
        .facilities
        .iter()
        .filter(|facility| matches!(facility.kind.as_str(), "toilet" | "water_station" | "meal_distribution" | "medical_support" | "intake"))
        .map(|facility| facility.zone_id.clone())
        .filter(|zone| !(reachable.contains(&zone.as_str()) && returnable.contains(&zone.as_str())))
        .collect();
    core.sort();
    core.dedup();
    core
}

// ---- The builder ------------------------------------------------------------------------------

/// The (source ID, title) of each default model the selected-policy loop names, in its order
/// (`selected_policy_sources` of `build`); the source IDs Python takes from a class are read from
/// the constants.
fn selected_policy_sources() -> Result<Vec<(String, String, String)>> {
    let policies = at(constants(), &["policy_ids"])?;
    let policy = |field: &str| text(at(policies, &[field])?, field);
    let rows: [(&str, Option<&str>, &str, &str); 13] = [
        ("behavior_policy_id", Some("ASM-SLEEP-AWARE-BEHAVIOR-V2"), "", "Sleep-debt-aware utility softmax Agent behavior"),
        ("queue_policy_id", None, "NEED_WEIGHTED_QUEUE_SOURCE_ID", "Facility queue priority and abandonment"),
        ("relationship_policy_id", None, "DISTANCE_TRUST_SOURCE_ID", "Distance and repeated-contact relationship trust"),
        ("body_dynamics_policy_id", Some("ASM-SLEEP-DEBT-BALANCE-V1"), "", "Body state complete feedback with balanced sleep-debt accounting"),
        ("staff_operations_policy_id", Some("ASM-STAFF-URGENCY-FIRST-V1"), "", "Urgency-first routed staff assignment"),
        ("zone_temperature_policy_id", Some("ASM-ZONE-TEMP-COMFORT-BAND-V1"), "", "Temperature comfort-band response"),
        ("staff_fatigue_policy_id", Some("ASM-STAFF-FATIGUE-LINEAR-V1"), "", "Linear staff duty fatigue equation"),
        ("zone_rest_policy_id", Some("ASM-ZONE-REST-MULTIPLICATIVE-V1"), "", "Combined noise and lighting rest response"),
        ("zone_hygiene_policy_id", Some("ASM-ZONE-HYGIENE-LINEAR-V1"), "", "Linear ventilation and hygiene response"),
        ("zone_privacy_policy_id", Some("ASM-ZONE-PRIVACY-THRESHOLD-V1"), "", "Threshold privacy and discontent response"),
        ("care_outcome_policy_id", Some("ASM-CARE-TYPED-SUPPORT-RESPONSE-V1"), "", "Subtype-specific experimental psychological care response"),
        ("social_policy_id", Some("ASM-BOSSE-AFFECT-CONTAGION-V1"), "", "Expressiveness and channel openness affect contagion"),
        ("information_policy_id", Some("ASM-EVIDENCE-AWARE-RUMOR-V1"), "", "Evidence-aware rumor belief and correction"),
    ];
    rows.iter()
        .map(|(field, source, source_name, title)| {
            let source = match source {
                Some(source) => (*source).to_owned(),
                None => id_of(source_name)?,
            };
            Ok((policy(field)?, source, (*title).to_owned()))
        })
        .collect()
}

/// `InitialScenarioBuilder.build(request, research=research, age_bands=..., household_mix=...)`
/// for a screen request, then `ProjectWorkspace.preview`'s changes and hash. `service_warnings`
/// is `service_capacity_diagnostic(scenario)["warnings"]` of the built Scenario (the document
/// before `preview`'s changes, which do not touch what it reads).
pub fn assemble(parts: &Parts, service_warnings: impl FnOnce(&V) -> Result<Vec<String>>) -> Result<Built> {
    let request = parts.request;
    let research = parts.research;
    let design = parts.design;
    let population = parts.population;
    let conditions = parts.conditions;
    let policies = at(constants(), &["policy_ids"])?;
    if let V::Map(defaults) = policies {
        for (field, default) in defaults {
            if at(request, &[field])? != default {
                return Err(Error::Unsupported(format!("{field} other than its default").into()));
            }
        }
    }
    if opt(request, "model_parameter_overrides").is_some_and(|value| !matches!(value, V::Map(map) if map.is_empty())) {
        return Err(Error::Unsupported("model_parameter_overrides".into()));
    }
    for key in ["facility_zones", "zone_area_shares"] {
        if opt(request, key).is_some_and(|value| match value {
            V::List(items) => !items.is_empty(),
            V::Map(map) => !map.is_empty(),
            _ => true,
        }) {
            return Err(Error::Unsupported(key.to_owned().into()));
        }
    }
    let municipality = text(at(request, &["municipality"])?, "municipality")?;
    let disaster = text(at(request, &["disaster"])?, "disaster")?;
    let evacuees = int(at(request, &["evacuees"])?, "evacuees")?;
    let seed = at(request, &["seed"])?.clone();
    let research_request = at(research, &["request"])?;
    if casefold(&text(at(research_request, &["municipality_query"])?, "municipality_query")?) != casefold(&municipality) {
        return Err(Error::Value("regional research query does not match the scenario municipality".to_owned()));
    }
    if opt(research_request, "municipality_code") != opt(request, "municipality_code") {
        return Err(Error::Value("regional research municipality code does not match the scenario request".to_owned()));
    }
    if disaster_families(&text(at(research_request, &["disaster"])?, "disaster")?) != disaster_families(&disaster) {
        return Err(Error::Value("regional research disaster does not match the scenario disaster".to_owned()));
    }
    if opt(research_request, "estat_area_code").is_some() || !list(at(research, &["population_observations"])?, "observations")?.is_empty() {
        return Err(Error::Unsupported("population statistics in the research".into()));
    }

    let mut design_warnings: Vec<String> = design.warnings.clone();
    let mut approval_blockers: Vec<String> = Vec::new();
    if design.capacity_supported < evacuees {
        approval_blockers.push(format!(
            "入力人数{evacuees}人が標準モデルの避難スペース容量{}人を超えています。人数または面積を調整してください。",
            design.capacity_supported
        ));
    }
    let requirements = list(at(population, &["person_requirements"])?, "person_requirements")?;
    let mut unaccompanied: Vec<V> = Vec::new();
    for item in requirements {
        if opt(item, "guardian_status").is_some_and(|status| matches!(status, V::Str(text) if &**text == "unaccompanied")) {
            unaccompanied.push(at(item, &["person_id"])?.clone());
        }
    }
    if !unaccompanied.is_empty() {
        let blocker = format!(
            "同居世帯内の年齢・支援情報から保護者を確認できない未成年者が{}人います。家族関係を確認するまでシミュレーションを開始できません。",
            unaccompanied.len()
        );
        design_warnings.push(blocker.clone());
        approval_blockers.push(blocker);
    }
    let profiles = list(at(population, &["profile_set", "profiles"])?, "profiles")?;
    let mut any_assisted = false;
    for profile in profiles {
        if text(at(profile, &["mobility"])?, "mobility")? != "independent" {
            any_assisted = true;
        }
    }
    if any_assisted {
        let inaccessible = inaccessible_service_zones(design);
        let missing_toilet = !design.facilities.iter().any(|facility| facility.kind == "toilet" && facility.id.starts_with("accessible_toilet_"));
        if !inaccessible.is_empty() || missing_toilet {
            let mut reasons = Vec::new();
            if !inaccessible.is_empty() {
                reasons.push(format!("アクセシブルな往復経路を確認できない設備ゾーン: {}", inaccessible.join("、")));
            }
            if missing_toilet {
                reasons.push("アクセシブルトイレとして識別された設備がありません".to_owned());
            }
            let blocker = format!(
                "移動支援ニーズのある避難者が生成されています。{}。経路幅と設備を確認するまでシミュレーションを開始できません。",
                reasons.join("。")
            );
            design_warnings.push(blocker.clone());
            approval_blockers.push(blocker);
        }
    }

    // source_records: research | extra, then the request's and the conditions' records.
    let mut extra: IndexMap<String, V> = IndexMap::new();
    for source in list(at(population, &["sources"])?, "population sources")? {
        extra.insert(record_id(source), source.clone());
    }
    let facility_assumptions: Vec<V> = design.assumptions.clone();
    let behavior_source = "ASM-SLEEP-AWARE-BEHAVIOR-V2";
    let behavior_policy = text(at(request, &["behavior_policy_id"])?, "behavior_policy_id")?;
    let toilet_source = id_of("TOILET_SERVICE_SOURCE_ID")?;
    let mut model_assumptions: Vec<V> = vec![
        map(vec![
            ("id", s(behavior_source)),
            ("subject", s("behavior_model_defaults")),
            (
                "value",
                map(vec![
                    ("decision_interval_minutes", V::Int(15)),
                    ("mode", s("offline")),
                    ("policy", s(&behavior_policy)),
                    ("sleep_debt_utility_weight", V::Float(1.0)),
                ]),
            ),
            ("reason", s("Default simulation behavior configuration; not calibrated to local observations.")),
            ("source_id", s(behavior_source)),
        ]),
        map(vec![
            ("id", s("ASM-STAFF-SUPPORT-LIFECYCLE-V1")),
            ("subject", s("staff_support_lifecycle")),
            (
                "value",
                map(vec![
                    ("support_task_minutes", V::Int(10)),
                    ("unresolved_after_minutes", V::Int(90)),
                    ("support_cooldown_minutes", V::Int(30)),
                    ("support_fairness_minutes", V::Int(30)),
                ]),
            ),
            (
                "reason",
                s("Generic support timing and fairness-aging delay used by the simulation model; not an operational staffing standard."),
            ),
            ("source_id", s("ASM-STAFF-SUPPORT-LIFECYCLE-V1")),
        ]),
        map(vec![
            ("id", s("ASM-TOILET-SERVICE-DURATION-V1")),
            ("subject", s("toilet_service_duration")),
            ("value", map(vec![("distribution", s("discrete_uniform_inclusive")), ("minimum_minutes", V::Int(4)), ("maximum_minutes", V::Int(8))])),
            ("reason", s("Generic toilet service timing used by the simulation model; not measured at the selected facility.")),
            ("source_id", s(&toilet_source)),
        ]),
    ];
    for assumption in facility_assumptions.iter().chain(model_assumptions.iter()) {
        extra.insert(text(at(assumption, &["source_id"])?, "source_id")?, assumption_record(assumption)?);
    }
    let mut source_records: IndexMap<String, V> = IndexMap::new();
    for source in list(at(research, &["sources"])?, "research sources")? {
        source_records.insert(record_id(source), source.clone());
    }
    for (key, value) in extra {
        source_records.insert(key, value);
    }
    let mut operator_population_sources = texts(at(request, &["evacuation_selection", "source_ids"])?, "source_ids")?;
    operator_population_sources.extend(texts(at(request, &["population_conditions", "source_ids"])?, "source_ids")?);
    for source_id in operator_population_sources {
        source_records.entry(source_id.clone()).or_insert_with(|| {
            map(vec![
                ("id", s(&source_id)),
                ("title", s("Operator-declared population input reference")),
                ("source_kind", s("operator")),
                (
                    "description",
                    s("Identifier supplied with operator population/selection input; external citation and applicability have not been verified."),
                ),
            ])
        });
    }
    let population_source_ids = texts(at(population, &["population", "source_ids"])?, "population source_ids")?;
    let operator_population_id = id_of("OPERATOR_POPULATION_SOURCE_ID")?;
    if population_source_ids.contains(&operator_population_id) {
        source_records.entry(operator_population_id.clone()).or_insert_with(|| {
            map(vec![
                ("id", s(&operator_population_id)),
                ("title", s("Operator-entered population generation conditions")),
                ("source_kind", s("operator")),
                ("description", s("Rates and categories were entered by the scenario operator and are not regional statistics.")),
            ])
        });
    }
    for record in &conditions.request_provenance_sources {
        source_records.insert(record_id(record), record.clone());
    }
    let mut missing: Vec<&String> = population_source_ids.iter().filter(|id| !source_records.contains_key(*id)).collect();
    missing.sort();
    missing.dedup();
    if !missing.is_empty() {
        approval_blockers.push(format!(
            "人口・世帯入力の出典が調査結果に見つかりません。実データと仮定を確認してください: {}",
            missing.iter().map(|id| id.as_str()).collect::<Vec<_>>().join(", ")
        ));
    }
    let mut missing: Vec<&String> = conditions.request_referenced_source_ids.iter().filter(|id| !source_records.contains_key(*id)).collect();
    missing.sort();
    missing.dedup();
    if !missing.is_empty() {
        approval_blockers.push(format!(
            "運営条件の出典IDが調査結果または入力者・仮定の出典記録にありません: {}",
            missing.iter().map(|id| id.as_str()).collect::<Vec<_>>().join(", ")
        ));
    }
    let population_assumptions = list(at(population, &["assumptions"])?, "population assumptions")?.to_vec();
    let duration_days = match at(request, &["duration_days"])? {
        V::Float(days) => *days,
        V::Int(days) => *days as f64,
        _ => return Err(Error::Value("duration_days must be a number".to_owned())),
    };
    let duration_minutes = py_round_int(duration_days * 1440.0).ok_or_else(|| Error::Value("duration_days".to_owned()))?.max(1);
    approval_blockers.extend(conditions.blockers.iter().cloned());
    for record in &conditions.provenance_sources {
        source_records.insert(record_id(record), record.clone());
    }
    if let Some(provisioning) = &conditions.provisioning {
        let id = id_of("STANDARD_PROVISIONING_SOURCE_ID")?;
        source_records.insert(
            id.clone(),
            map(vec![
                ("id", s(&id)),
                ("title", s("Standard food and water provisioning")),
                ("source_kind", s("assumption")),
                (
                    "description",
                    s("Drinking water of 3 L per person per day and three days of stock follow national household guidance; outside supply starts on the chosen day."),
                ),
            ]),
        );
        model_assumptions.push(provisioning.clone());
    }
    let compiled: IndexMap<&str, &V> = conditions.model_configs.iter().map(|(key, value)| (key.as_str(), value)).collect();
    if compiled.contains_key("resource_service") {
        let id = id_of("RESOURCE_SERVICE_ASSUMPTION_ID")?;
        source_records.insert(
            id.clone(),
            map(vec![
                ("id", s(&id)),
                ("title", s("Generic water and meal service conversion")),
                ("source_kind", s("assumption")),
                (
                    "description",
                    s("The opt-in service model uses five-minute service duration, 250 ml water, and one meal unit per completed service; calibrate these assumptions before operational use."),
                ),
            ]),
        );
        model_assumptions.push(map(vec![
            ("id", s(&id)),
            ("subject", s("resource_service_conversion")),
            ("value", map(vec![("service_minutes", V::Int(5)), ("water_ml_per_service", V::Int(250)), ("food_meal_units_per_service", V::Int(1))])),
            ("reason", s("Generic service conversion coefficients are assumptions, not measured staffing or consumption data.")),
            ("source_id", s(&id)),
        ]));
    }
    if compiled.contains_key("infrastructure") {
        return Err(Error::Unsupported("infrastructure conditions".into()));
    }
    let facility_source_ids: Vec<String> = facility_assumptions.iter().map(|item| at(item, &["source_id"]).and_then(|id| text(id, "source_id"))).collect::<Result<_>>()?;
    for assumption in &model_assumptions {
        let source_id = text(at(assumption, &["source_id"])?, "source_id")?;
        if !facility_source_ids.contains(&source_id) {
            if !source_records.contains_key(&source_id) {
                source_records.insert(source_id, assumption_record(assumption)?);
            }
        }
    }

    let initial_status = text(at(request, &["operational_conditions", "initial_resources", "status"])?, "status")?;
    let mut notes = Vec::new();
    if initial_status == "unknown" {
        notes.push(s("No starting food or water inventory was entered; the standard provisioning assumption was applied."));
    }
    notes.push(s("Synthetic profiles do not represent identifiable real people."));
    let age_bands = at(population, &["age_bands"])?.clone();
    let mut assumptions: Vec<V> = population_assumptions;
    assumptions.extend(facility_assumptions.iter().cloned());
    assumptions.extend(model_assumptions.iter().cloned());
    let blockers_value = || V::List(approval_blockers.iter().map(|text| s(text)).collect());
    let mut provenance = map(vec![
        ("generator", s("region-initial-scenario-v1")),
        ("natural_language_input", request.clone()),
        ("sources", V::Null),
        ("observations", V::List(design.observations.clone())),
        ("assumptions", V::Null),
        ("profile_generation", at(population, &["profile_set", "manifest"])?.clone()),
        (
            "population_generation",
            map(vec![
                ("generator", at(population, &["generator"])?.clone()),
                ("household_sizes", at(population, &["household_sizes"])?.clone()),
                ("age_bands", age_bands.clone()),
                ("household_mix", at(population, &["household_mix"])?.clone()),
                ("source_ids", at(population, &["population", "source_ids"])?.clone()),
                ("resident_distribution", age_bands),
                ("resident_age_distribution", at(population, &["resident_age_distribution"])?.clone()),
                ("evacuee_age_distribution", at(population, &["evacuee_age_distribution"])?.clone()),
                ("evacuation_selection", at(population, &["evacuation_selection"])?.clone()),
                ("household_construction", at(population, &["household_relationships"])?.clone()),
                ("person_requirements_preview", V::List(requirements.to_vec())),
                ("unaccompanied_minor_ids", V::List(unaccompanied)),
                (
                    "shared_population_contract_note",
                    s("Requirements persist in model_configs.population_context alongside PopulationSpec v1; recorded requirements do not imply that every corresponding behavior is simulated."),
                ),
            ]),
        ),
        ("operational_conditions", at(request, &["operational_conditions"])?.clone()),
        ("editable_facility_geometry", design.editable_geometry.dump()),
        ("review_required", V::Bool(true)),
        ("approval_blockers", blockers_value()),
        ("unknown_initial_stock", V::Bool(initial_status == "unknown")),
        ("notes", V::List(notes)),
        ("region_research", research.clone()),
    ]);
    let origins = at(request, &["field_origins"])?;
    let mut defaulted: Vec<(&str, V)> = Vec::new();
    for field in ["duration_days", "seed", "accessible_toilet_count"] {
        let origin = opt(origins, field).and_then(|value| match value {
            V::Str(text) => Some(text.to_string()),
            _ => None,
        });
        if !matches!(origin.as_deref(), Some("user" | "llm_extracted")) {
            let value = match at(request, &[field])? {
                V::Float(days) if field == "duration_days" && !parts.duration_days_given && *days == 7.0 => V::Int(7),
                other => other.clone(),
            };
            defaulted.push((field, value));
        }
    }
    if !defaulted.is_empty() {
        assumptions.push(map(vec![
            ("id", s("ASM-INITIAL-SCENARIO-DEFAULTS-V1")),
            ("subject", s("scenario_default_fields")),
            ("value", map(defaulted)),
            ("reason", s("These setup values were not explicitly entered in the initial scenario text.")),
            ("source_id", s("ASM-INITIAL-SCENARIO-DEFAULTS-V1")),
        ]));
        source_records.insert(
            "ASM-INITIAL-SCENARIO-DEFAULTS-V1".to_owned(),
            map(vec![
                ("id", s("ASM-INITIAL-SCENARIO-DEFAULTS-V1")),
                ("title", s("Initial scenario builder defaults")),
                ("source_kind", s("assumption")),
                ("description", s("Recorded defaults for duration, seed, and accessible toilet count.")),
            ]),
        );
    }
    if let Some(area) = opt(request, "gross_area_m2") {
        source_records.insert(
            "SRC-OPERATOR-AREA".to_owned(),
            map(vec![
                ("id", s("SRC-OPERATOR-AREA")),
                ("title", s("Operator-entered gross shelter area")),
                ("source_kind", s("operator")),
                ("area_m2", area.clone()),
                ("description", s("Entered by the scenario operator; not independently verified.")),
            ]),
        );
    }
    let sorted_sources: BTreeMap<&String, &V> = source_records.iter().collect();
    let mut sources: Vec<V> = sorted_sources.into_values().cloned().collect();

    let geometry_dump = design.geometry.dump();
    let zones: Vec<V> = design.zones.iter().map(|zone| zone.dump()).collect();
    let edges: Vec<V> = design.zone_edges.iter().map(|edge| edge.dump()).collect();
    let facilities: Vec<V> = design.facilities.iter().map(|facility| facility.dump()).collect();
    let staff: Vec<V> = design.staff.iter().map(|member| member.dump()).collect();
    let researched_at = isoformat(&text(at(research, &["researched_at"])?, "researched_at")?);
    let identity = map(vec![
        ("request", request.clone()),
        ("research_time", s(&researched_at)),
        ("geometry", geometry_dump.clone()),
        ("zones", V::List(zones.clone())),
        ("zone_edges", V::List(edges.clone())),
        ("facilities", V::List(facilities.clone())),
        ("staff", V::List(staff.clone())),
        ("population", at(population, &["population"])?.clone()),
        ("profiles", V::List(profiles.to_vec())),
        ("selected_shelter_id", V::Null),
        ("selected_shelter", V::Null),
    ]);
    let scenario_id = format!("region-{}", &sha256_hex(spaced_json(&identity)?.as_bytes())[..16]);

    // model_configs: the constants, the geometry and population, the generated context, the
    // compiled conditions; zone_environment only if the conditions gave none.
    let constant_configs = at(constants(), &["model_configs"])?;
    let mut model_configs: IndexMap<Arc<str>, V> = IndexMap::new();
    if let V::Map(entries) = constant_configs {
        for (key, value) in entries {
            if &**key != "zone_environment" {
                model_configs.insert(key.clone(), value.clone());
            }
        }
    }
    model_configs.insert(Arc::from("facility_geometry"), geometry_dump);
    model_configs.insert(Arc::from("population"), at(population, &["population"])?.clone());
    for (key, value) in parts.connections {
        model_configs.insert(Arc::from(key.as_str()), value.clone());
    }
    for (key, value) in &conditions.model_configs {
        model_configs.insert(Arc::from(key.as_str()), value.clone());
    }
    if !model_configs.contains_key("zone_environment") {
        model_configs.insert(Arc::from("zone_environment"), at(constant_configs, &["zone_environment"])?.clone());
    }

    let has_source = |sources: &[V], id: &str| sources.iter().any(|source| record_id(source) == id);
    let installed = |id: &str, title: &str, description: &str| {
        map(vec![("id", s(id)), ("title", s(title)), ("source_kind", s("assumption")), ("description", s(description))])
    };
    for (name, title) in [
        ("BODY_SOURCE_ID", "Experimental life-state feedback"),
        ("STAFF_OPERATIONS_SOURCE_ID", "Experimental staff operations"),
        ("MOVEMENT_SOURCE_ID", "Experimental spatial movement"),
        ("TEMPORARY_ROOM_BED_SOURCE_ID", "Exclusive temporary sleeping places"),
        ("TEMPORARY_ROOM_INTAKE_SOURCE_ID", "Physical temporary room intake"),
        ("INSTALLED_FACILITY_PLACEMENT_SOURCE_ID", "Floor-plan placement of installed facilities"),
        ("TEMPORARY_SPACES_SOURCE_ID", "Zone-only temporary capacity"),
    ] {
        let id = id_of(name)?;
        if !has_source(&sources, &id) {
            sources.push(installed(&id, title, "Installed synthetic coefficients; not empirically calibrated."));
        }
        assumptions.push(map(vec![
            ("id", s(&id)),
            ("subject", s(title)),
            ("source_id", s(&id)),
            ("reason", s("New generated Scenario explicitly enables the installed experimental model.")),
        ]));
    }
    let kinds = at(constants(), &["orientation_facility_kinds"])?.clone();
    let orientation = id_of("ORIENTATION_FACILITY_SOURCE_ID")?;
    sources.push(installed(
        &orientation,
        "Intake orientation to shared shelter facilities",
        "Every Agent is assumed to be shown the shared service facilities at intake; not observed at a real shelter.",
    ));
    assumptions.push(map(vec![
        ("id", s(&orientation)),
        ("subject", s("initial_facility_knowledge")),
        ("value", map(vec![("known_facility_kinds", kinds.clone())])),
        ("source_id", s(&orientation)),
        (
            "reason",
            s("Agents choose only facilities they know; shared services are known from the start, group-specific spaces are learned on arrival."),
        ),
    ]));
    let announcement = id_of("INSTALLED_FACILITY_ANNOUNCEMENT_SOURCE_ID")?;
    sources.push(installed(
        &announcement,
        "Announcement of shared facilities installed during a Run",
        "Every present Agent is assumed to be told at once about a shared service facility installed during the Run; not observed at a real shelter.",
    ));
    assumptions.push(map(vec![
        ("id", s(&announcement)),
        ("subject", s("installed_facility_knowledge")),
        ("value", map(vec![("announced_facility_kinds", kinds), ("delay_minutes", V::Int(0)), ("reach", s("all_present_agents"))])),
        ("source_id", s(&announcement)),
        (
            "reason",
            s("There is no information-spread path for facility knowledge, so an installed shared facility would otherwise stay unknown to everyone already admitted."),
        ),
    ]));
    let psychology = id_of("PSYCHOLOGY_SOURCE_ID")?;
    sources.push(installed(
        &psychology,
        "Mean-reverting acute distress with life-state feedback",
        "Literature-motivated mean-reversion structure (Kuppens et al. 2010) with uncalibrated coefficients; not fitted to shelter data.",
    ));
    assumptions.push(map(vec![
        ("id", s(&psychology)),
        ("subject", s("psychology_model")),
        ("value", map(vec![("policy_version", s(&id_of("INTEGRATED_PSYCHOLOGY_POLICY_VERSION")?))])),
        ("source_id", s(&psychology)),
        (
            "reason",
            s("New generated Scenarios select the research psychology model; legacy models remain only for saved inputs and explicit comparisons."),
        ),
    ]));
    for (model_id, source_id, title) in selected_policy_sources()? {
        if !has_source(&sources, &source_id) {
            sources.push(installed(&source_id, &title, "Installed experimental operator; coefficients are not empirically calibrated."));
        }
        assumptions.push(map(vec![
            ("id", s(&source_id)),
            ("subject", s(&title)),
            ("source_id", s(&source_id)),
            ("reason", s(&format!("Scenario selected installed model {model_id}; coefficients remain experimental assumptions."))),
        ]));
    }
    if text(at(request, &["social_proximity_policy_id"])?, "social_proximity_policy_id")? == "distance-sampled-proximity-v1" {
        return Err(Error::Unsupported("distance-sampled proximity".into()));
    }
    for key in ["zone_environment", "staff_dynamics"] {
        if let Some(V::Str(source_id)) = model_configs.get(key).and_then(|config| opt(config, "source_id")) {
            if !has_source(&sources, source_id) {
                let mut record = map(vec![("id", s(source_id)), ("source_kind", s("assumption"))]);
                insert(&mut record, "title", s(&format!("Installed experimental {key}")));
                insert(&mut record, "description", s("Uncalibrated operator coefficients; no empirical validity claim."));
                sources.push(record);
            }
        }
    }
    let suite = at(constants(), &["core_suite_models"])?;
    let mut core_suite_selected = true;
    for (domain, field) in [
        ("behavior", "behavior_policy_id"),
        ("social", "social_policy_id"),
        ("information", "information_policy_id"),
        ("queue", "queue_policy_id"),
        ("relationship", "relationship_policy_id"),
        ("social_proximity", "social_proximity_policy_id"),
        ("body_dynamics", "body_dynamics_policy_id"),
    ] {
        core_suite_selected &= at(request, &[field])? == at(suite, &[domain])?;
    }
    if core_suite_selected {
        for (source_id, title, description) in [
            (
                id_of("CORE_MODEL_SUITE_SOURCE_ID")?,
                id_of("CORE_MODEL_SUITE_V2")?,
                "Versioned combination of installed operators; internally tested, not empirically validated.",
            ),
            (
                id_of("SLEEP_DEBT_BALANCE_SOURCE_ID")?,
                "Balanced sleep-debt accounting assumption".to_owned(),
                "Required-sleep balance and no-recovery-during-rest coefficients are synthetic assumptions, not clinical estimates.",
            ),
        ] {
            if !has_source(&sources, &source_id) {
                sources.push(map(vec![("id", s(&source_id)), ("source_kind", s("assumption")), ("title", s(&title)), ("description", s(description))]));
            }
            if !assumptions.iter().any(|item| record_id(item) == source_id) {
                assumptions.push(map(vec![("id", s(&source_id)), ("subject", s(&title)), ("source_id", s(&source_id)), ("reason", s(description))]));
            }
        }
    }
    sources.push(map(vec![
        ("id", s("ASM-AFFECT-TRANSFER-V1")),
        ("source_kind", s("assumption")),
        ("title", s("Installed affect transfer")),
        ("description", s("Synthetic contact transfer equation, not calibrated to measured shelter outcomes.")),
    ]));
    insert(&mut provenance, "sources", V::List(sources));
    insert(&mut provenance, "assumptions", V::List(assumptions));

    let mut scenario = map(vec![
        ("schema_version", V::Int(3)),
        ("id", s(&scenario_id)),
        ("duration_minutes", V::Int(duration_minutes)),
        ("seed", seed),
        ("zones", V::List(zones)),
        ("zone_edges", V::List(edges)),
        ("facilities", V::List(facilities)),
        ("staff", V::List(staff)),
        ("profiles", V::List(profiles.to_vec())),
        ("scheduled_events", V::List(conditions.scheduled_events.clone())),
        ("initial_stock", conditions.initial_stock.clone().unwrap_or(V::Null)),
        ("model_configs", V::Map(model_configs)),
        ("provenance", provenance),
    ]);
    let mut warnings = design_warnings;
    warnings.extend(texts(at(research, &["unknowns"])?, "unknowns")?);
    warnings.extend(texts(at(research, &["warnings"])?, "warnings")?);
    warnings.extend(service_warnings(&scenario)?);

    // ProjectWorkspace.preview: no public building's address in a stored experiment, and the input.
    if let V::Map(top) = &mut scenario {
        if let Some(V::Map(provenance)) = top.get_mut("provenance") {
            if let Some(V::Map(region)) = provenance.get_mut("region_research") {
                if let Some(V::List(shelters)) = region.get_mut("shelters") {
                    for shelter in shelters {
                        if let V::Map(fields) = shelter {
                            fields.shift_remove("address");
                        }
                    }
                }
            }
            provenance.insert(
                Arc::from("scenario_input"),
                map(vec![("sha256", s(&parts.input.sha256)), ("extraction_source", s(&parts.input.extraction_source))]),
            );
        }
    }
    let canonical = canonical_json(&scenario)?;
    let approved_hash = sha256_hex(&canonical);
    let can_start = approval_blockers.is_empty();
    Ok(Built { scenario, canonical, approved_hash, warnings, approval_blockers, can_start })
}

/// `ScenarioSpec.canonical_json()` of a Scenario's dump: `initial_stock` left out when None (its
/// `source_ids` sorted otherwise), zones, edges, facilities, staff and profiles by ID, events by
/// (due minute, priority, sequence, ID), keys sorted, compact.
pub fn canonical_json(scenario: &V) -> Result<Vec<u8>> {
    let mut document = scenario.clone();
    if let V::Map(top) = &mut document {
        match top.get_mut("initial_stock") {
            Some(V::Null) => {
                top.shift_remove("initial_stock");
            }
            Some(V::Map(stock)) => {
                if let Some(V::List(ids)) = stock.get_mut("source_ids") {
                    ids.sort_by(|a, b| sort_text(a).cmp(sort_text(b)));
                }
            }
            _ => {}
        }
        for key in ["zones", "zone_edges", "facilities", "staff", "profiles"] {
            if let Some(V::List(items)) = top.get_mut(key) {
                items.sort_by(|a, b| sort_text(opt(a, "id").unwrap_or(&V::Null)).cmp(sort_text(opt(b, "id").unwrap_or(&V::Null))));
            }
        }
        if let Some(V::List(events)) = top.get_mut("scheduled_events") {
            let key = |event: &V| {
                let number = |name: &str| match opt(event, name) {
                    Some(V::Int(value)) => *value,
                    _ => 0,
                };
                (number("due_minute"), number("priority"), number("sequence"), sort_text(opt(event, "id").unwrap_or(&V::Null)).to_owned())
            };
            events.sort_by_key(key);
        }
    }
    document.to_json()
}

fn sort_text(value: &V) -> &str {
    match value {
        V::Str(text) => text,
        _ => "",
    }
}

// ---- The whole build --------------------------------------------------------------------------

/// Why `build` makes no Scenario: what the server answers 400 (`Invalid`), or what the browser
/// does not compute (`Unsupported`, the browser's `browser_unsupported`).
pub use crate::scenario::request::Refusal;

/// A screen request's Scenario as `scenario-preview` answers it, but the text parser's warnings,
/// the research and the IDs (the route adds them).
#[derive(Clone, Debug, PartialEq)]
pub struct Preview {
    pub built: Built,
    /// `InitialScenarioRequest.model_dump(mode="json")` (the answer's `request`).
    pub request: V,
    /// `FacilityDesign.model_dump(mode="json")` (the answer's `facility_design`).
    pub facility_design: V,
    /// sha256 of the design's geometry's canonical JSON (the answer's `geometry_hash`).
    pub geometry_hash: String,
}

fn refusal(error: Error) -> Refusal {
    match error {
        Error::Unsupported(reason) => Refusal::Unsupported(reason.into_owned()),
        Error::Value(text) | Error::MarkedValue(text) => Refusal::Invalid(text),
        other => Refusal::Invalid(format!("{other:?}")),
    }
}

/// `scenario-preview`'s build of a request (`InitialScenarioRequest`'s JSON, as the screen or the
/// text parser gives it) with the research the route made (its JSON dump), `research` and
/// `input` given as bytes and values the route read. The parts are made in the core: the
/// standard shelter (#296), the population (#297), the conditions, the generated context and the
/// service-capacity warnings (#298).
pub fn build(request_json: &[u8], research_json: &[u8], input: &ScenarioInput) -> std::result::Result<Preview, Refusal> {
    use crate::scenario::conditions::{with_standard_provisioning, Conditions as Compilable, FacilityKind};
    use crate::scenario::connections::connect_generated_context;
    use crate::scenario::facility::{generate, FacilityRequest};
    use crate::scenario::population::{generate_population, PopulationInput};
    use crate::scenario::request::InitialScenarioRequest;
    use crate::scenario::service_capacity::service_capacity_warnings;

    let request = InitialScenarioRequest::of_json(request_json)?;
    let research: serde_json::Value = serde_json::from_slice(research_json).map_err(|error| Refusal::Invalid(format!("research is not JSON: {error}")))?;
    let research = V::from_json(&research).ok_or_else(|| Refusal::Invalid("research holds a number Python cannot read".to_owned()))?;
    let design = generate(&FacilityRequest {
        population: request.evacuees,
        gross_area_m2: request.gross_area_m2,
        accessible_toilet_count: request.accessible_toilet_count,
        staff_count: request.staff_count,
        facility_conditions: request.facility_conditions.clone(),
    })
    .map_err(refusal)?;
    let population = generate_population(&PopulationInput::for_request(request.evacuees, request.seed)).map_err(Refusal::Invalid)?;
    let duration_minutes = request.duration_minutes();
    let (conditions, provisioning) =
        with_standard_provisioning(&request.operational_conditions, request.evacuees, duration_minutes, request.default_supply_start_day);
    let kinds: Vec<FacilityKind> = design.facilities.iter().map(|item| FacilityKind { id: &item.id, kind: &item.kind }).collect();
    let compiled = conditions.compile(duration_minutes, &kinds).map_err(Refusal::Invalid)?;
    let entered = Compilable::of_request(&request.operational_conditions);
    let compiled = Conditions {
        initial_stock: compiled.initial_stock.as_ref().map(|stock| stock.dump()),
        scheduled_events: compiled.scheduled_events,
        model_configs: compiled.model_configs,
        blockers: compiled.blockers,
        provenance_sources: conditions.provenance_sources(),
        provisioning,
        request_provenance_sources: entered.provenance_sources(),
        request_referenced_source_ids: entered.referenced_source_ids(),
    };
    let geometry = design.geometry.dump();
    let editable = design.editable_geometry.dump();
    let connections = connect_generated_context(&population, Some(&editable), &geometry).map_err(Refusal::Invalid)?;
    let request_dump = request.dump();
    let duration_days_given = matches!(
        serde_json::from_slice::<serde_json::Value>(request_json),
        Ok(serde_json::Value::Object(fields)) if fields.contains_key("duration_days")
    );
    let population_dump = population.dump();
    let parts = Parts {
        request: &request_dump,
        research: &research,
        design: &design,
        population: &population_dump,
        conditions: &compiled,
        connections: &connections,
        input,
        duration_days_given,
    };
    let built = assemble(&parts, |scenario| service_capacity_warnings(scenario).map_err(Error::Value)).map_err(refusal)?;
    let geometry_hash = sha256_hex(&geometry.to_json().map_err(refusal)?);
    Ok(Preview { built, request: request_dump, facility_design: design.dump(), geometry_hash })
}
