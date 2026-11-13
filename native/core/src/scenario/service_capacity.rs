//! `service_capacity_diagnostic` of `experiments/service_capacity.py` (Issue #298): whether a
//! Scenario's toilet, water and meal units can serve the daily demand its coefficients imply. The
//! builder shows its warnings. It reads the Scenario's dump (`ScenarioSpec.model_dump(mode="json")`
//! or its canonical JSON read back): the profiles, facilities, staff and model configs.

use std::collections::BTreeSet;

use crate::mathx::{py_min, py_sum};
use crate::scenario::profiles::{map, text, texts};
use crate::value::V;

pub const SERVICE_CAPACITY_SOURCE_ID: &str = "ASM-SERVICE-CAPACITY-PLANNING-V1";
pub const TOILET_GUIDELINE_SOURCE_ID: &str = "SRC-CAO-TOILET-GUIDELINE-2024-12";
pub const OPERATING_MINUTES_PER_DAY: i64 = 960;
pub const TARGET_UTILIZATION: f64 = 0.8;
/// `PEOPLE_PER_TOILET["initial"]` of `experiments/sanitation_diagnostics.py`.
pub const PEOPLE_PER_TOILET_INITIAL: i64 = 50;
pub const PLANNING_TOILET_USES_PER_PERSON_DAY: i64 = 5;
/// `ToiletServiceConfig()`'s bounds and `PsychologyModelConfig().hunger_growth`, for a Scenario
/// without those configs (the builder always writes them).
const DEFAULT_TOILET_MINUTES: (i64, i64) = (4, 8);
const DEFAULT_HUNGER_GROWTH: f64 = 0.0005;

fn get<'a>(value: &'a V, name: &str) -> Option<&'a V> {
    match value {
        V::Map(entries) => entries.get(name),
        _ => None,
    }
}

fn need<'a>(value: &'a V, name: &str) -> Result<&'a V, String> {
    get(value, name).ok_or_else(|| format!("the Scenario has no {name}"))
}

fn items<'a>(value: &'a V, name: &str) -> Result<&'a [V], String> {
    match get(value, name) {
        Some(V::List(items)) => Ok(items),
        None => Ok(&[]),
        _ => Err(format!("the Scenario's {name} is not a list")),
    }
}

fn float(value: &V) -> Result<f64, String> {
    match value {
        V::Float(number) => Ok(*number),
        V::Int(number) => Ok(*number as f64),
        _ => Err("a coefficient is not a number".into()),
    }
}

fn int(value: &V) -> Result<i64, String> {
    match value {
        V::Int(number) => Ok(*number),
        _ => Err("a count is not an int".into()),
    }
}

fn string(value: &V) -> Result<&str, String> {
    match value {
        V::Str(text) => Ok(text),
        _ => Err("an ID is not a string".into()),
    }
}

/// `required_service_units`.
pub fn required_service_units(daily_service_minutes: f64) -> i64 {
    if daily_service_minutes <= 0.0 {
        return 0;
    }
    (daily_service_minutes / (OPERATING_MINUTES_PER_DAY as f64 * TARGET_UTILIZATION)).ceil() as i64
}

/// `uses_per_person_day`.
pub fn uses_per_person_day(growth_per_minute: f64, relief: f64, action_threshold: f64) -> f64 {
    let removed = if action_threshold > 0.0 { py_min(relief, action_threshold) } else { relief };
    growth_per_minute * 1440.0 / removed
}

/// A number the row writes as Python's int or float.
#[derive(Clone, Copy)]
enum Num {
    Int(i64),
    Float(f64),
}

impl Num {
    fn value(self) -> f64 {
        match self {
            Num::Int(number) => number as f64,
            Num::Float(number) => number,
        }
    }

    fn v(self) -> V {
        match self {
            Num::Int(number) => V::Int(number),
            Num::Float(number) => V::Float(number),
        }
    }

    /// `self * other` as Python multiplies an int or a float by an int or a float.
    fn times(self, other: Num) -> Num {
        match (self, other) {
            (Num::Int(first), Num::Int(second)) => Num::Int(first * second),
            _ => Num::Float(self.value() * other.value()),
        }
    }
}

struct Scenario<'a> {
    document: &'a V,
    configs: &'a V,
    population: i64,
}

impl Scenario<'_> {
    /// `_working_units`.
    fn working_units(&self, facility: &V) -> Result<i64, String> {
        let units = match get(facility, "working_units") {
            Some(V::Null) | None => int(need(facility, "capacity")?)?,
            Some(value) => int(value)?,
        };
        let id = string(need(facility, "id")?)?;
        let surveyed = get(self.configs, "movement_model").is_some()
            && matches!(get(self.configs, "facility_geometry"), Some(geometry @ V::Map(_))
                if matches!(get(geometry, "facility_points"), Some(V::Map(points)) if points.contains_key(id)));
        Ok(if surveyed { units.min(1) } else { units })
    }

    /// `_row`.
    fn row(
        &self,
        service: &str,
        facility_kind: &str,
        uses: Num,
        demand_basis: &str,
        service_minutes: Num,
        staff_skill: Option<&str>,
        standard_units: Option<i64>,
    ) -> Result<V, String> {
        let mut facilities = Vec::new();
        for facility in items(self.document, "facilities")? {
            if string(need(facility, "kind")?)? == facility_kind {
                facilities.push(facility);
            }
        }
        let mut units = 0;
        for facility in &facilities {
            units += self.working_units(facility)?;
        }
        let serving_units = match staff_skill {
            None => units,
            Some(skill) => {
                let mut zones = BTreeSet::new();
                for facility in &facilities {
                    zones.insert(string(need(facility, "zone_id")?)?);
                }
                let mut serving = 0;
                for zone in zones {
                    let mut zone_units = 0;
                    for facility in &facilities {
                        if string(need(facility, "zone_id")?)? == zone {
                            zone_units += self.working_units(facility)?;
                        }
                    }
                    let mut zone_staff = 0;
                    for member in items(self.document, "staff")? {
                        let skilled = match get(member, "skills") {
                            Some(V::List(skills)) => skills.iter().any(|item| matches!(item, V::Str(text) if &**text == skill)),
                            _ => false,
                        };
                        if string(need(member, "zone_id")?)? == zone && skilled {
                            zone_staff += 1;
                        }
                    }
                    serving += zone_units.min(zone_staff);
                }
                serving
            }
        };
        let daily_uses = Num::Int(self.population).times(uses);
        let daily_minutes = daily_uses.times(service_minutes).value();
        let available = (serving_units * OPERATING_MINUTES_PER_DAY) as f64;
        let utilization = (available != 0.0).then(|| daily_minutes / available);
        let required_units = required_service_units(daily_minutes);
        let label = match service {
            "toilet" => "トイレ",
            "water" => "給水",
            _ => "配食",
        };
        let mut warnings = Vec::new();
        let status = if daily_minutes > 0.0 && units == 0 {
            warnings.push(format!("{label}の設備がありません。"));
            "no_units"
        } else if daily_minutes > 0.0 && serving_units == 0 {
            warnings.push(format!(
                "{label}設備のzoneに担当技能（{}）を持つ職員がいないため、{label}は一度も始まりません。",
                staff_skill.map_or_else(|| "None".to_string(), str::to_string)
            ));
            "no_staff"
        } else if let Some(rate) = utilization.filter(|rate| *rate >= 1.0) {
            warnings.push(format!(
                "{label}は休みなく稼働しても1日の需要を処理できません（利用率{rate:.2}、稼働{serving_units}、目安{required_units}）。"
            ));
            "structurally_short"
        } else if let Some(rate) = utilization.filter(|rate| *rate > TARGET_UTILIZATION) {
            warnings.push(format!(
                "{label}の利用率が目標0.8を超え、長い待ち行列が起きやすい状態です（利用率{rate:.2}、稼働{serving_units}、目安{required_units}）。"
            ));
            "above_target"
        } else {
            "ok"
        };
        if let Some(standard) = standard_units.filter(|standard| units < *standard) {
            warnings.push(format!("{label}が公的な計画目安を下回ります（{units}基、目安{standard}基）。"));
        }
        let source_ids: &[&str] = if service == "toilet" {
            &[SERVICE_CAPACITY_SOURCE_ID, TOILET_GUIDELINE_SOURCE_ID]
        } else {
            &[SERVICE_CAPACITY_SOURCE_ID]
        };
        Ok(map(vec![
            ("service", text(service)),
            ("facility_kind", text(facility_kind)),
            ("status", text(status)),
            ("units", V::Int(units)),
            ("serving_units", V::Int(serving_units)),
            ("uses_per_person_day", uses.v()),
            ("demand_basis", text(demand_basis)),
            ("service_minutes", service_minutes.v()),
            ("daily_uses", daily_uses.v()),
            ("daily_service_minutes", V::Float(daily_minutes)),
            ("utilization", utilization.map_or(V::Null, V::Float)),
            ("required_units", V::Int(required_units)),
            ("standard_units", standard_units.map_or(V::Null, V::Int)),
            ("source_ids", texts(source_ids)),
            ("warnings", texts(&warnings)),
        ]))
    }
}

fn not_modeled(service: &str, facility_kind: &str) -> V {
    map(vec![
        ("service", text(service)),
        ("facility_kind", text(facility_kind)),
        ("status", text("not_modeled")),
        ("reason", text("resource_service is not enabled; no stock, demand or service is simulated")),
        ("warnings", V::List(Vec::new())),
    ])
}

/// `service_capacity_diagnostic(scenario)` (phase `"initial"`) of a Scenario's dump.
pub fn service_capacity_diagnostic(scenario: &V) -> Result<V, String> {
    let profiles = items(scenario, "profiles")?;
    let empty = V::Map(Default::default());
    let configs = get(scenario, "model_configs").unwrap_or(&empty);
    let context = Scenario { document: scenario, configs, population: profiles.len() as i64 };
    let toilet = get(configs, "toilet_service").unwrap_or(&empty);
    let minimum = get(toilet, "minimum_minutes").map_or(Ok(DEFAULT_TOILET_MINUTES.0), int)?;
    let maximum = get(toilet, "maximum_minutes").map_or(Ok(DEFAULT_TOILET_MINUTES.1), int)?;
    let population = context.population;
    let mut rows = vec![context.row(
        "toilet",
        "toilet",
        Num::Int(PLANNING_TOILET_USES_PER_PERSON_DAY),
        "Cabinet Office planning figure: 5 uses per person per day",
        Num::Float((minimum + maximum) as f64 / 2.0),
        None,
        Some(if population > 0 { (population + PEOPLE_PER_TOILET_INITIAL - 1) / PEOPLE_PER_TOILET_INITIAL } else { 0 }),
    )?];
    match get(configs, "resource_service") {
        None | Some(V::Null) => {
            rows.push(not_modeled("water", "water_station"));
            rows.push(not_modeled("meal", "meal_distribution"));
        }
        Some(resource) => {
            let coefficient = |name: &str| need(resource, name).and_then(float);
            let psychology = get(configs, "psychology").unwrap_or(&empty);
            let hunger_growth = get(psychology, "hunger_growth").map_or(Ok(DEFAULT_HUNGER_GROWTH), float)?;
            let by_agent = get(psychology, "hunger_growth_by_agent");
            let mut rates = Vec::with_capacity(profiles.len());
            for profile in profiles {
                let id = string(need(profile, "id")?)?;
                rates.push(match by_agent.and_then(|table| get(table, id)) {
                    Some(rate) => float(rate)?,
                    None => hunger_growth,
                });
            }
            let mean = if rates.is_empty() { 0.0 } else { py_sum(&rates) / rates.len() as f64 };
            let service_minutes = Num::Int(int(need(resource, "service_minutes")?)?);
            let water_skill = string(need(resource, "water_staff_skill")?)?;
            let meal_skill = string(need(resource, "meal_staff_skill")?)?;
            rows.push(context.row(
                "water",
                "water_station",
                Num::Float(uses_per_person_day(
                    coefficient("hydration_growth_per_minute")?,
                    coefficient("hydration_relief")?,
                    coefficient("hydration_action_threshold")?,
                )),
                "hydration_growth_per_minute x 1440 / min(hydration_relief, hydration_action_threshold)",
                service_minutes,
                Some(water_skill),
                None,
            )?);
            rows.push(context.row(
                "meal",
                "meal_distribution",
                Num::Float(uses_per_person_day(mean, coefficient("hunger_relief")?, coefficient("hunger_action_threshold")?)),
                "mean hunger_growth x 1440 / min(hunger_relief, hunger_action_threshold)",
                service_minutes,
                Some(meal_skill),
                None,
            )?);
        }
    }
    let warnings: Vec<V> =
        rows.iter().flat_map(|row| match get(row, "warnings") { Some(V::List(items)) => items.clone(), _ => Vec::new() }).collect();
    Ok(map(vec![
        ("scenario_id", get(scenario, "id").cloned().unwrap_or(V::Null)),
        ("phase", text("initial")),
        ("population", V::Int(population)),
        ("operating_minutes_per_day", V::Int(OPERATING_MINUTES_PER_DAY)),
        ("target_utilization", V::Float(TARGET_UTILIZATION)),
        ("services", V::List(rows)),
        ("warnings", V::List(warnings)),
        ("source_id", text(SERVICE_CAPACITY_SOURCE_ID)),
        (
            "interpretation",
            text(
                "設備計画の確認値です。需要はScenarioの係数からの近似で、時間帯の集中や故障を含みません。\
                 利用率が1以上なら、休みなく稼働しても1日の需要を処理できません。",
            ),
        ),
    ]))
}

/// The diagnostic's warnings, which the builder adds to the preview's.
pub fn service_capacity_warnings(scenario: &V) -> Result<Vec<String>, String> {
    let diagnostic = service_capacity_diagnostic(scenario)?;
    Ok(match get(&diagnostic, "warnings") {
        Some(V::List(items)) => items.iter().filter_map(|item| string(item).ok().map(str::to_string)).collect(),
        _ => Vec::new(),
    })
}
