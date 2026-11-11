//! `zone_environment_view` (`domain/environment.py`) and the formulas it selects
//! (`ZoneEnvironmentFormula` in `policies/registry.py`): a zone's conditions, read from
//! `model_configs` and the modifiers of `zone_environment_v1`, as `V` (lane P8 a1).
//!
//! `rules/world_view.rs` keeps what this makes while its inputs stay the same. Values and formula
//! meaning are the Python rule's; the coefficients here are the listed defaults of its contracts and
//! the constants of its formulas (docs/rust-port/lanes/l5-read-rules.md).
//!
//! `read_config` is `ScenarioZoneEnvironmentConfig.model_validate` of a document that validation
//! accepts: anything it does not accept says `Unsupported`, and the Python rule raises its own text.
//! The same goes for a record the Python rule would raise on. The strictness of an exact float over
//! Python's arithmetic is what `rules/world_view.rs` has always asked of a view: an int coordinate
//! could not stay an int in a typed field, and `1` and `1.0` would hash differently.

use std::sync::Arc;

use indexmap::IndexMap;
use rustc_hash::FxHashMap;

use crate::error::{Error, Result};
use crate::events::is_stable_id;
use crate::mathx::{py_max, py_min};
use crate::value::V;

const SOURCE_ID: &str = "ASM-H19-ZONE-ENVIRONMENT-V2";
const LEGACY_SOURCE_ID: &str = "ASM-H19-ZONE-ENVIRONMENT-V1";

/// The key of `model_state` the modifiers are in.
pub const STATE: &str = "zone_environment_v1";

// ---- `ZoneEnvironmentView` -------------------------------------------------------------------------

/// `ZoneDemandView`.
#[derive(Clone, Debug, PartialEq)]
pub struct DemandModifier {
    pub action: Arc<str>,
    pub probability_multiplier: f64,
    pub duration_multiplier: f64,
    pub modifier_event_ids: Vec<Arc<str>>,
    pub assumption_ids: Vec<Arc<str>>,
}

/// `ZoneEnvironmentView`.
#[derive(Clone, Debug, PartialEq)]
pub struct ZoneConditions {
    pub ambient_noise_level: f64,
    pub rest_efficiency_multiplier: f64,
    pub noise_modifier_event_ids: Vec<Arc<str>>,
    pub noise_assumption_ids: Vec<Arc<str>>,
    pub demand_modifiers: Vec<DemandModifier>,
    pub min_rest_decision_minutes: i64,
    pub max_rest_decision_minutes: i64,
    pub ventilation_effectiveness: f64,
    pub lighting_level: f64,
    pub privacy_level: f64,
    pub temperature_c: f64,
    pub ventilation_hygiene_growth_per_minute: f64,
    pub privacy_discontent_per_minute: f64,
    pub temperature_stress_per_minute: f64,
    pub condition_modifier_event_ids: Vec<Arc<str>>,
    pub condition_assumption_ids: Vec<Arc<str>>,
}

impl ZoneConditions {
    /// `ZoneEnvironmentView()`: a world without the opt-in policy.
    fn neutral() -> Self {
        Self {
            ambient_noise_level: 0.0,
            rest_efficiency_multiplier: 1.0,
            noise_modifier_event_ids: Vec::new(),
            noise_assumption_ids: Vec::new(),
            demand_modifiers: Vec::new(),
            min_rest_decision_minutes: 1,
            max_rest_decision_minutes: 60,
            ventilation_effectiveness: 0.7,
            lighting_level: 0.5,
            privacy_level: 0.5,
            temperature_c: 20.0,
            ventilation_hygiene_growth_per_minute: 0.0001,
            privacy_discontent_per_minute: 0.0001,
            temperature_stress_per_minute: 0.0,
            condition_modifier_event_ids: Vec::new(),
            condition_assumption_ids: Vec::new(),
        }
    }
}

// ---- `ScenarioZoneEnvironmentConfig` ---------------------------------------------------------------

/// `ZoneEnvironmentBaseline`.
#[derive(Clone, Copy)]
struct Baseline {
    ventilation_effectiveness: f64,
    lighting_level: f64,
    privacy_level: f64,
    temperature_c: f64,
}

const DEFAULT_BASELINE: Baseline =
    Baseline { ventilation_effectiveness: 0.7, lighting_level: 0.5, privacy_level: 0.5, temperature_c: 20.0 };

struct Config {
    is_v2: bool,
    source_id: Arc<str>,
    noise_rest_efficiency_loss: f64,
    min_probability_multiplier: f64,
    max_probability_multiplier: f64,
    min_rest_decision_minutes: i64,
    max_rest_decision_minutes: i64,
    lighting_sleep_efficiency_loss: f64,
    ventilation_hygiene_growth_per_minute: f64,
    privacy_discontent_per_minute: f64,
    temperature_stress_per_degree_minute: f64,
    comfortable_min_temperature_c: f64,
    comfortable_max_temperature_c: f64,
    baseline_by_zone: FxHashMap<Arc<str>, Baseline>,
}

pub const CONFIG_REFUSED: &str = "the zone environment config does not parse";

#[track_caller]
fn config_refused<T>() -> Result<T> {
    Err(Error::unsupported(CONFIG_REFUSED))
}

/// A field of `Field(strict=True, ge=low, le=high, allow_inf_nan=False)`, which takes an int as its
/// float and not a bool.
#[track_caller]
fn bounded_float(value: &V, low: f64, high: f64) -> Result<f64> {
    let number = match value {
        V::Float(number) => *number,
        V::Int(number) => *number as f64,
        _ => return config_refused(),
    };
    if number.is_finite() && number >= low && number <= high {
        Ok(number)
    } else {
        config_refused()
    }
}

#[track_caller]
fn bounded_int(value: &V, low: i64, high: i64) -> Result<i64> {
    match value {
        V::Int(number) if *number >= low && *number <= high => Ok(*number),
        _ => config_refused(),
    }
}

fn read_baseline(value: &V) -> Result<Baseline> {
    let V::Map(record) = value else {
        return config_refused();
    };
    let mut baseline = DEFAULT_BASELINE;
    for (name, item) in record {
        match &**name {
            "ventilation_effectiveness" => baseline.ventilation_effectiveness = bounded_float(item, 0.0, 1.0)?,
            "lighting_level" => baseline.lighting_level = bounded_float(item, 0.0, 1.0)?,
            "privacy_level" => baseline.privacy_level = bounded_float(item, 0.0, 1.0)?,
            "temperature_c" => baseline.temperature_c = bounded_float(item, -40.0, 60.0)?,
            _ => return config_refused(),
        }
    }
    Ok(baseline)
}

/// The model's fields as `keep_legacy_environment_assumptions_neutral` and the contract's defaults
/// leave them, then the checks of `multipliers_are_ordered`. A stable ID with spaces around it, which
/// the contract would strip, is refused: it could not be the canonical document validation asks for.
fn read_config(raw: &IndexMap<Arc<str>, V>) -> Result<Config> {
    let legacy = match raw.get("policy_version") {
        Some(V::Str(version)) if &**version == "zone-environment-v1" => true,
        Some(V::Str(version)) if &**version == "zone-environment-v2" => false,
        Some(_) => return config_refused(),
        None => [
            "lighting_sleep_efficiency_loss",
            "ventilation_hygiene_growth_per_minute",
            "privacy_discontent_per_minute",
            "temperature_stress_per_degree_minute",
            "baseline_by_zone",
        ]
        .iter()
        .all(|name| !raw.contains_key(*name)),
    };
    let neutral_or = |coefficient: f64| if legacy { 0.0 } else { coefficient };
    let mut config = Config {
        is_v2: !legacy,
        source_id: Arc::from(if legacy { LEGACY_SOURCE_ID } else { SOURCE_ID }),
        noise_rest_efficiency_loss: 0.5,
        min_probability_multiplier: 0.5,
        max_probability_multiplier: 2.0,
        min_rest_decision_minutes: 1,
        max_rest_decision_minutes: 60,
        lighting_sleep_efficiency_loss: neutral_or(0.2),
        ventilation_hygiene_growth_per_minute: neutral_or(0.0001),
        privacy_discontent_per_minute: neutral_or(0.0001),
        temperature_stress_per_degree_minute: neutral_or(0.0001),
        comfortable_min_temperature_c: 18.0,
        comfortable_max_temperature_c: 26.0,
        baseline_by_zone: FxHashMap::default(),
    };
    for (name, value) in raw {
        match &**name {
            // `Literal[1]` looks the value up, so what equals 1 passes.
            "schema_version" => {
                if !matches!(value, V::Bool(true) | V::Int(1)) && !matches!(value, V::Float(number) if *number == 1.0) {
                    return config_refused();
                }
            }
            "policy_version" => {}
            "source_type" => {
                if !matches!(value, V::Str(text) if &**text == "assumption") {
                    return config_refused();
                }
            }
            "source_id" => {
                let V::Str(text) = value else {
                    return config_refused();
                };
                if !is_stable_id(text) {
                    return config_refused();
                }
                config.source_id = text.clone();
            }
            "noise_rest_efficiency_loss" => config.noise_rest_efficiency_loss = bounded_float(value, 0.0, 0.9)?,
            "min_probability_multiplier" => config.min_probability_multiplier = bounded_float(value, 0.1, 1.0)?,
            "max_probability_multiplier" => config.max_probability_multiplier = bounded_float(value, 1.0, 2.0)?,
            "min_rest_decision_minutes" => config.min_rest_decision_minutes = bounded_int(value, 1, 60)?,
            "max_rest_decision_minutes" => config.max_rest_decision_minutes = bounded_int(value, 1, 240)?,
            "max_active_modifiers_per_zone" => {
                bounded_int(value, 1, 16)?;
            }
            "lighting_sleep_efficiency_loss" => config.lighting_sleep_efficiency_loss = bounded_float(value, 0.0, 0.9)?,
            "ventilation_hygiene_growth_per_minute" => {
                config.ventilation_hygiene_growth_per_minute = bounded_float(value, 0.0, 0.01)?;
            }
            "privacy_discontent_per_minute" => config.privacy_discontent_per_minute = bounded_float(value, 0.0, 0.01)?,
            "temperature_stress_per_degree_minute" => {
                config.temperature_stress_per_degree_minute = bounded_float(value, 0.0, 0.01)?;
            }
            "comfortable_min_temperature_c" => config.comfortable_min_temperature_c = bounded_float(value, -10.0, 40.0)?,
            "comfortable_max_temperature_c" => config.comfortable_max_temperature_c = bounded_float(value, -10.0, 50.0)?,
            "baseline_by_zone" => {
                let V::Map(baselines) = value else {
                    return config_refused();
                };
                for (zone, baseline) in baselines {
                    if !is_stable_id(zone) {
                        return config_refused();
                    }
                    config.baseline_by_zone.insert(zone.clone(), read_baseline(baseline)?);
                }
            }
            _ => return config_refused(),
        }
    }
    if (legacy
        && (config.lighting_sleep_efficiency_loss != 0.0
            || config.ventilation_hygiene_growth_per_minute != 0.0
            || config.privacy_discontent_per_minute != 0.0
            || config.temperature_stress_per_degree_minute != 0.0
            || !config.baseline_by_zone.is_empty()))
        || config.min_probability_multiplier > config.max_probability_multiplier
        || config.min_rest_decision_minutes > config.max_rest_decision_minutes
        || config.comfortable_min_temperature_c >= config.comfortable_max_temperature_c
    {
        return config_refused();
    }
    Ok(config)
}

// ---- `ZoneEnvironmentFormula` ----------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq)]
enum Temperature {
    ComfortBand,
    Midpoint,
}

#[derive(Clone, Copy, PartialEq)]
enum Rest {
    Multiplicative,
    Limiting,
}

#[derive(Clone, Copy, PartialEq)]
enum Hygiene {
    Linear,
    Quadratic,
}

#[derive(Clone, Copy, PartialEq)]
enum Privacy {
    Threshold,
    Proportional,
}

fn temperature_deviation(model: Temperature, temperature_c: f64, config: &Config) -> f64 {
    let minimum = config.comfortable_min_temperature_c;
    let maximum = config.comfortable_max_temperature_c;
    match model {
        Temperature::Midpoint => (temperature_c - (minimum + maximum) / 2.0).abs(),
        Temperature::ComfortBand => py_max(py_max(0.0, minimum - temperature_c), temperature_c - maximum),
    }
}

fn rest_efficiency(model: Rest, noise_level: f64, lighting_level: f64, config: &Config) -> f64 {
    let noise_loss = noise_level * config.noise_rest_efficiency_loss;
    let lighting_loss = lighting_level * config.lighting_sleep_efficiency_loss;
    match model {
        Rest::Limiting => py_max(0.1, 1.0 - py_max(noise_loss, lighting_loss)),
        Rest::Multiplicative => py_max(0.1, (1.0 - noise_loss) * (1.0 - lighting_loss)),
    }
}

fn ventilation_hygiene_growth(model: Hygiene, ventilation: f64, config: &Config) -> f64 {
    let mut deficit = 1.0 - ventilation;
    if model == Hygiene::Quadratic {
        deficit *= deficit;
    }
    deficit * config.ventilation_hygiene_growth_per_minute
}

fn privacy_discontent(model: Privacy, privacy: f64, config: &Config) -> f64 {
    let deficit = match model {
        Privacy::Proportional => 1.0 - privacy,
        Privacy::Threshold => py_max(0.0, 0.4 - privacy),
    };
    deficit * config.privacy_discontent_per_minute
}

/// The selected formulas, and the source IDs a selection adds. `create_zone_*_formula` of an ID the
/// registry does not hold raises; a formula another module registered is not one of these.
struct Formulas {
    temperature: Temperature,
    rest: Rest,
    hygiene: Hygiene,
    privacy: Privacy,
    sources: Vec<Arc<str>>,
}

const UNKNOWN: &str = "a zone formula of another module";

fn read_formulas(registry: Option<&V>) -> Result<Formulas> {
    let registry = match registry {
        Some(V::Map(registry)) => Some(registry),
        Some(_) => return Err(Error::unsupported("the policy registry is not a plain dict")),
        None => None,
    };
    let mut sources = Vec::new();
    let select = |domain: &str| -> Result<Option<&Arc<str>>> {
        let Some(registry) = registry else { return Ok(None) };
        match registry.get(domain) {
            None => Ok(None),
            Some(V::Str(text)) => Ok(Some(text)),
            Some(_) => Err(Error::unsupported("a zone formula's ID is not a str")),
        }
    };
    let temperature = match select("zone_environment")?.map(|text| &**text) {
        None => Temperature::ComfortBand,
        Some("zone-temperature-comfort-band-v1") => {
            sources.push(Arc::from("ASM-ZONE-TEMP-COMFORT-BAND-V1"));
            Temperature::ComfortBand
        }
        Some("zone-temperature-midpoint-v1") => {
            sources.push(Arc::from("ASM-ZONE-TEMP-MIDPOINT-V1"));
            Temperature::Midpoint
        }
        Some(_) => return Err(Error::unsupported(UNKNOWN)),
    };
    let rest = match select("zone_rest")?.map(|text| &**text) {
        None => Rest::Multiplicative,
        Some("zone-rest-multiplicative-v1") => {
            sources.push(Arc::from("ASM-ZONE-REST-MULTIPLICATIVE-V1"));
            Rest::Multiplicative
        }
        Some("zone-rest-limiting-v1") => {
            sources.push(Arc::from("ASM-ZONE-REST-LIMITING-V1"));
            Rest::Limiting
        }
        Some(_) => return Err(Error::unsupported(UNKNOWN)),
    };
    let hygiene = match select("zone_hygiene")?.map(|text| &**text) {
        None => Hygiene::Linear,
        Some("zone-hygiene-linear-v1") => {
            sources.push(Arc::from("ASM-ZONE-HYGIENE-LINEAR-V1"));
            Hygiene::Linear
        }
        Some("zone-hygiene-quadratic-v1") => {
            sources.push(Arc::from("ASM-ZONE-HYGIENE-QUADRATIC-V1"));
            Hygiene::Quadratic
        }
        Some(_) => return Err(Error::unsupported(UNKNOWN)),
    };
    let privacy = match select("zone_privacy")?.map(|text| &**text) {
        None => Privacy::Threshold,
        Some("zone-privacy-threshold-v1") => {
            sources.push(Arc::from("ASM-ZONE-PRIVACY-THRESHOLD-V1"));
            Privacy::Threshold
        }
        Some("zone-privacy-proportional-v1") => {
            sources.push(Arc::from("ASM-ZONE-PRIVACY-PROPORTIONAL-V1"));
            Privacy::Proportional
        }
        Some(_) => return Err(Error::unsupported(UNKNOWN)),
    };
    Ok(Formulas { temperature, rest, hygiene, privacy, sources })
}

// ---- The modifiers ---------------------------------------------------------------------------------

pub const RECORD_REFUSED: &str = "a zone modifier is not what the rule reads";

#[track_caller]
fn record_refused<T>() -> Result<T> {
    Err(Error::unsupported(RECORD_REFUSED))
}

/// A modifier record: its ID, and the document in `zone_environment_v1.modifiers`.
struct Modifier {
    id: Arc<str>,
    zone_id: Option<Arc<str>>,
    record: IndexMap<Arc<str>, V>,
}

impl Modifier {
    #[track_caller]
    fn field(&self, name: &str) -> Result<&V> {
        match self.record.get(name) {
            Some(value) => Ok(value),
            None => record_refused(),
        }
    }

    #[track_caller]
    fn text(&self, name: &str) -> Result<Arc<str>> {
        match self.field(name)? {
            V::Str(text) => Ok(text.clone()),
            _ => record_refused(),
        }
    }

    #[track_caller]
    fn parameter(&self, name: &str) -> Result<&V> {
        match self.field("parameters")? {
            V::Map(parameters) => match parameters.get(name) {
                Some(value) => Ok(value),
                None => record_refused(),
            },
            _ => record_refused(),
        }
    }

    #[track_caller]
    fn number(&self, name: &str) -> Result<f64> {
        match self.parameter(name)? {
            V::Float(number) => Ok(*number),
            _ => record_refused(),
        }
    }

    #[track_caller]
    fn start_minute(&self) -> Result<i64> {
        match self.field("start_minute")? {
            V::Int(minute) => Ok(*minute),
            _ => record_refused(),
        }
    }

    /// `record["op"]`; only a str is one of the operations the rule compares it with.
    #[track_caller]
    fn op(&self) -> Result<Option<&str>> {
        Ok(match self.field("op")? {
            V::Str(text) => Some(text),
            _ => None,
        })
    }
}

fn sorted_set(mut items: Vec<Arc<str>>) -> Vec<Arc<str>> {
    items.sort_unstable();
    items.dedup();
    items
}

/// What every zone's conditions read: None when the world has no zone environment.
pub struct ZoneInputs {
    parts: Option<(Config, Formulas, Vec<Modifier>)>,
}

impl ZoneInputs {
    /// The head of `zone_environment_view`: `model_configs` (a map) and `model_state.get(STATE)`
    /// as it finds them, and what it would raise on for any zone.
    pub fn read(configs: &IndexMap<Arc<str>, V>, state: Option<&V>) -> Result<Self> {
        let (Some(V::Map(config)), Some(V::Map(state))) = (configs.get("zone_environment"), state) else {
            return Ok(Self { parts: None });
        };
        let config = read_config(config)?;
        let formulas = read_formulas(configs.get("policy_registry"))?;
        let Some(documents) = state.get("modifiers") else {
            return Err(Error::unsupported("the zone environment state has no modifiers"));
        };
        let V::Map(documents) = documents else {
            return Err(Error::unsupported("the zone modifiers are not a mapping"));
        };
        let mut modifiers = Vec::with_capacity(documents.len());
        for (id, document) in documents {
            let V::Map(record) = document else {
                return record_refused();
            };
            let zone_id = match record.get("zone_id") {
                Some(V::Str(zone)) => Some(zone.clone()),
                Some(_) => None,
                None => return record_refused(),
            };
            modifiers.push(Modifier { id: id.clone(), zone_id, record: record.clone() });
        }
        Ok(Self { parts: Some((config, formulas, modifiers)) })
    }

    /// `zone_environment_view(world, zone_id)`.
    pub fn conditions(&self, zone_id: &str) -> Result<ZoneConditions> {
        let Some((config, formulas, modifiers)) = &self.parts else {
            return Ok(ZoneConditions::neutral());
        };
        let mut entries: Vec<(i64, &Modifier)> = Vec::new();
        for modifier in modifiers {
            if modifier.zone_id.as_deref() == Some(zone_id) {
                entries.push((modifier.start_minute()?, modifier));
            }
        }
        entries.sort_unstable_by(|first, second| (first.0, &first.1.id).cmp(&(second.0, &second.1.id)));
        let of_op = |op: &str| -> Result<Vec<&Modifier>> {
            let mut found = Vec::new();
            for (_, modifier) in &entries {
                if modifier.op()? == Some(op) {
                    found.push(*modifier);
                }
            }
            Ok(found)
        };
        let baseline = config.baseline_by_zone.get(zone_id).copied().unwrap_or(DEFAULT_BASELINE);
        let ventilation_records = of_op("set_ventilation")?;
        let lighting_records = of_op("set_lighting")?;
        let privacy_records = of_op("set_privacy")?;
        let temperature_records = of_op("set_temperature")?;
        let ventilation = match ventilation_records.last() {
            Some(record) => record.number("effectiveness")?,
            None => baseline.ventilation_effectiveness,
        };
        let lighting = match lighting_records.last() {
            Some(record) => record.number("level")?,
            None => baseline.lighting_level,
        };
        let privacy = match privacy_records.last() {
            Some(record) => record.number("level")?,
            None => baseline.privacy_level,
        };
        let temperature = match temperature_records.last() {
            Some(record) => record.number("temperature_c")?,
            None => baseline.temperature_c,
        };
        let mut condition_event_ids = Vec::new();
        let mut condition_sources = vec![config.source_id.clone()];
        let mut has_condition_records = false;
        for records in [&ventilation_records, &lighting_records, &privacy_records, &temperature_records] {
            for record in records {
                has_condition_records = true;
                condition_event_ids.push(record.text("start_event_id")?);
                condition_sources.push(record.text("source_id")?);
            }
        }
        // Not a set in the Python rule: an event that started two records shows twice.
        condition_event_ids.sort_unstable();
        condition_sources.extend(formulas.sources.iter().cloned());

        let degree_deviation = temperature_deviation(formulas.temperature, temperature, config);
        let noise_entries = of_op("set_ambient_noise")?;
        let mut noise_level: Option<f64> = None;
        for record in &noise_entries {
            let level = record.number("level")?;
            noise_level = Some(noise_level.map_or(level, |largest| py_max(largest, level)));
        }
        let noise_level = noise_level.unwrap_or(0.0);
        let rest = rest_efficiency(formulas.rest, noise_level, lighting, config);

        let mut by_action: Vec<(Arc<str>, Vec<&Modifier>)> = Vec::new();
        for modifier in of_op("modify_demand_rate")? {
            let V::Str(action) = modifier.parameter("action")? else {
                return record_refused();
            };
            match by_action.iter_mut().find(|(known, _)| known == action) {
                Some((_, records)) => records.push(modifier),
                None => by_action.push((action.clone(), vec![modifier])),
            }
        }
        by_action.sort_unstable_by(|first, second| first.0.cmp(&second.0));
        let mut demand_modifiers = Vec::with_capacity(by_action.len());
        for (action, records) in by_action {
            // `math.prod`: in order, from 1.
            let (mut probability, mut duration) = (1.0_f64, 1.0_f64);
            for record in &records {
                probability *= record.number("probability_multiplier")?;
            }
            for record in &records {
                duration *= record.number("duration_multiplier")?;
            }
            let mut sources = vec![config.source_id.clone()];
            let mut event_ids = Vec::with_capacity(records.len());
            for record in &records {
                event_ids.push(record.text("start_event_id")?);
                sources.extend(assumption_ids(record)?);
                sources.push(record.text("source_id")?);
            }
            let duration_multiplier = if matches!(&*action, "rest" | "sleep") {
                py_min(2.0, py_max(0.5, duration))
            } else {
                1.0
            };
            demand_modifiers.push(DemandModifier {
                action,
                probability_multiplier: py_min(
                    config.max_probability_multiplier,
                    py_max(config.min_probability_multiplier, probability),
                ),
                duration_multiplier,
                modifier_event_ids: sorted_set(event_ids),
                assumption_ids: sorted_set(sources),
            });
        }

        let mut noise_sources = vec![config.source_id.clone()];
        let mut noise_event_ids = Vec::with_capacity(noise_entries.len());
        for record in &noise_entries {
            noise_event_ids.push(record.text("start_event_id")?);
            noise_sources.push(record.text("source_id")?);
            noise_sources.extend(assumption_ids(record)?);
        }
        Ok(ZoneConditions {
            ambient_noise_level: noise_level,
            rest_efficiency_multiplier: rest,
            noise_modifier_event_ids: sorted_set(noise_event_ids),
            noise_assumption_ids: if noise_entries.is_empty() { Vec::new() } else { sorted_set(noise_sources) },
            demand_modifiers,
            min_rest_decision_minutes: config.min_rest_decision_minutes,
            max_rest_decision_minutes: config.max_rest_decision_minutes,
            ventilation_effectiveness: ventilation,
            lighting_level: lighting,
            privacy_level: privacy,
            temperature_c: temperature,
            ventilation_hygiene_growth_per_minute: ventilation_hygiene_growth(formulas.hygiene, ventilation, config),
            privacy_discontent_per_minute: privacy_discontent(formulas.privacy, privacy, config),
            temperature_stress_per_minute: degree_deviation * config.temperature_stress_per_degree_minute,
            condition_modifier_event_ids: condition_event_ids,
            condition_assumption_ids: if has_condition_records || config.is_v2 {
                sorted_set(condition_sources)
            } else {
                Vec::new()
            },
        })
    }
}

#[track_caller]
fn assumption_ids(record: &Modifier) -> Result<Vec<Arc<str>>> {
    let V::List(items) = record.field("assumption_ids")? else {
        return record_refused();
    };
    items
        .iter()
        .map(|item| match item {
            V::Str(text) => Ok(text.clone()),
            _ => record_refused(),
        })
        .collect()
}
