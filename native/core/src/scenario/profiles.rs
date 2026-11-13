//! `SyntheticProfileGenerator.generate` of `experiments/profiles.py` (Issue #297): the synthetic
//! profiles of a seed, drawn with the core's `CounterRNG` (`rng::KeyedDraws`), and the manifest
//! beside them, as `ProfileSpec` and `ProfileGenerationManifest` dump them.

use std::sync::Arc;

use indexmap::IndexMap;

use crate::mathx::py_sum;
use crate::rng::{Key, KeyedDraws};
use crate::value::V;

/// `ProfileGenerationManifest.generator`.
pub const PROFILE_GENERATOR_ID: &str = "counter-rng-profile-generator-v1";
/// `CounterRNG.algorithm_id`.
pub const RNG_ALGORITHM: &str = "sha256-keyed-v1";

pub(crate) fn text(value: &str) -> V {
    V::Str(Arc::from(value))
}

pub(crate) fn map(entries: Vec<(&str, V)>) -> V {
    V::Map(entries.into_iter().map(|(key, value)| (Arc::from(key), value)).collect::<IndexMap<_, _>>())
}

pub(crate) fn texts<S: AsRef<str>>(values: &[S]) -> V {
    V::List(values.iter().map(|value| text(value.as_ref())).collect())
}

/// The draws of `CounterRNG(seed)` for a seed the request validated (from 0, an i64).
pub fn draws_of_seed(seed: i64) -> KeyedDraws {
    KeyedDraws::new(itoa::Buffer::new().format(seed))
}

/// `rng.random(*parts)` of text parts.
pub(crate) fn random(draws: &KeyedDraws, parts: &[&str]) -> f64 {
    let mut key = Key::new();
    for part in parts {
        key.text(part);
    }
    key.draw(draws)
}

/// `Mobility` of `domain/models.py`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mobility {
    Independent,
    Assisted,
    Wheelchair,
}

impl Mobility {
    pub fn text(self) -> &'static str {
        match self {
            Mobility::Independent => "independent",
            Mobility::Assisted => "assisted",
            Mobility::Wheelchair => "wheelchair",
        }
    }
}

/// `ProfileTemplate`: the generator's weights and ranges.
#[derive(Clone, Debug, PartialEq)]
pub struct ProfileTemplate {
    pub template_id: String,
    pub agent_id_prefix: String,
    pub group_id_prefix: String,
    pub id_width: usize,
    pub group_size: i64,
    pub initial_zone_ids: Vec<String>,
    pub independent_weight: f64,
    pub assisted_weight: f64,
    pub wheelchair_weight: f64,
    pub support_need_min: f64,
    pub support_need_max: f64,
    pub stress_sensitivity_min: f64,
    pub stress_sensitivity_max: f64,
    pub toilet_urgency_min: f64,
    pub toilet_urgency_max: f64,
    pub hunger_min: f64,
    pub hunger_max: f64,
    pub fatigue_min: f64,
    pub fatigue_max: f64,
}

impl Default for ProfileTemplate {
    /// `ProfileTemplate()`.
    fn default() -> Self {
        Self {
            template_id: "synthetic-balanced-v1".into(),
            agent_id_prefix: "A".into(),
            group_id_prefix: "G".into(),
            id_width: 3,
            group_size: 4,
            initial_zone_ids: Vec::new(),
            independent_weight: 0.80,
            assisted_weight: 0.15,
            wheelchair_weight: 0.05,
            support_need_min: 0.0,
            support_need_max: 0.6,
            stress_sensitivity_min: 0.0,
            stress_sensitivity_max: 1.0,
            toilet_urgency_min: 0.0,
            toilet_urgency_max: 1.0,
            hunger_min: 0.0,
            hunger_max: 1.0,
            fatigue_min: 0.0,
            fatigue_max: 1.0,
        }
    }
}

impl ProfileTemplate {
    pub fn dump(&self) -> V {
        map(vec![
            ("template_id", text(&self.template_id)),
            ("agent_id_prefix", text(&self.agent_id_prefix)),
            ("group_id_prefix", text(&self.group_id_prefix)),
            ("id_width", V::Int(self.id_width as i64)),
            ("group_size", V::Int(self.group_size)),
            ("initial_zone_ids", texts(&self.initial_zone_ids)),
            ("independent_weight", V::Float(self.independent_weight)),
            ("assisted_weight", V::Float(self.assisted_weight)),
            ("wheelchair_weight", V::Float(self.wheelchair_weight)),
            ("support_need_min", V::Float(self.support_need_min)),
            ("support_need_max", V::Float(self.support_need_max)),
            ("stress_sensitivity_min", V::Float(self.stress_sensitivity_min)),
            ("stress_sensitivity_max", V::Float(self.stress_sensitivity_max)),
            ("toilet_urgency_min", V::Float(self.toilet_urgency_min)),
            ("toilet_urgency_max", V::Float(self.toilet_urgency_max)),
            ("hunger_min", V::Float(self.hunger_min)),
            ("hunger_max", V::Float(self.hunger_max)),
            ("fatigue_min", V::Float(self.fatigue_min)),
            ("fatigue_max", V::Float(self.fatigue_max)),
        ])
    }
}

/// `InitialNeedsSpec`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct InitialNeeds {
    pub toilet_urgency: f64,
    pub hunger: f64,
    pub fatigue: f64,
}

/// `ProfileSpec` of a synthetic profile (`provenance.source_kind` is `"synthetic"`).
#[derive(Clone, Debug, PartialEq)]
pub struct Profile {
    pub id: String,
    pub group_id: Option<String>,
    pub initial_zone_id: String,
    pub mobility: Mobility,
    pub support_need: f64,
    pub stress_sensitivity: f64,
    pub initial_needs: InitialNeeds,
    pub rng_key: String,
    /// `provenance.generator`: the template's ID.
    pub generator: String,
    /// `provenance.seed`.
    pub seed: i64,
}

impl Profile {
    /// `ProfileSpec.model_dump(mode="json")`.
    pub fn dump(&self) -> V {
        map(vec![
            ("schema_version", V::Int(2)),
            ("id", text(&self.id)),
            ("group_id", self.group_id.as_deref().map_or(V::Null, text)),
            ("initial_zone_id", text(&self.initial_zone_id)),
            ("mobility", text(self.mobility.text())),
            ("support_need", V::Float(self.support_need)),
            ("stress_sensitivity", V::Float(self.stress_sensitivity)),
            (
                "initial_needs",
                map(vec![
                    ("toilet_urgency", V::Float(self.initial_needs.toilet_urgency)),
                    ("hunger", V::Float(self.initial_needs.hunger)),
                    ("fatigue", V::Float(self.initial_needs.fatigue)),
                ]),
            ),
            ("rng_key", text(&self.rng_key)),
            (
                "provenance",
                map(vec![
                    ("source_kind", text("synthetic")),
                    ("generator", text(&self.generator)),
                    ("seed", V::Int(self.seed)),
                ]),
            ),
        ])
    }
}

/// `ProfileGenerationManifest` (no manual overrides).
#[derive(Clone, Debug, PartialEq)]
pub struct ProfileManifest {
    pub seed: i64,
    pub template: ProfileTemplate,
    pub profile_count: i64,
    pub zone_ids: Vec<String>,
}

impl ProfileManifest {
    pub fn dump(&self) -> V {
        map(vec![
            ("schema_version", V::Int(2)),
            ("generator", text(PROFILE_GENERATOR_ID)),
            ("rng_algorithm", text(RNG_ALGORITHM)),
            ("seed", V::Int(self.seed)),
            ("template", self.template.dump()),
            ("profile_count", V::Int(self.profile_count)),
            ("zone_ids", texts(&self.zone_ids)),
            ("manual_overrides", V::List(Vec::new())),
        ])
    }
}

/// `GeneratedProfileSet`.
#[derive(Clone, Debug, PartialEq)]
pub struct ProfileSet {
    pub profiles: Vec<Profile>,
    pub manifest: ProfileManifest,
}

impl ProfileSet {
    /// `{"profiles": [...], "manifest": {...}}`, as pydantic dumps the dataclass in a model.
    pub fn dump(&self) -> V {
        map(vec![
            ("profiles", V::List(self.profiles.iter().map(Profile::dump).collect())),
            ("manifest", self.manifest.dump()),
        ])
    }
}

/// `SyntheticProfileGenerator().generate(count=count, seed=seed, zone_ids=zone_ids,
/// template=template)`. The caller gives a template whose checks pass (`ProfileTemplate`'s
/// validator) and a count from 1.
pub fn generate_profiles(count: i64, seed: i64, zone_ids: &[String], template: &ProfileTemplate) -> Result<ProfileSet, String> {
    if count <= 0 {
        return Err("profile count must be a positive integer".into());
    }
    if seed < 0 {
        return Err("profile generation seed cannot be negative".into());
    }
    let mut available: Vec<String> = zone_ids.to_vec();
    available.sort();
    available.dedup();
    if available.is_empty() {
        return Err("profile generation requires at least one valid zone ID".into());
    }
    let targets: &[String] = if template.initial_zone_ids.is_empty() { &available } else { &template.initial_zone_ids };
    if targets.iter().any(|zone| !available.contains(zone)) {
        return Err("profile template references unknown zones".into());
    }
    let draws = draws_of_seed(seed);
    let mobility_total = py_sum(&[template.independent_weight, template.assisted_weight, template.wheelchair_weight]);
    let mut profiles = Vec::with_capacity(count as usize);
    for number in 1..=count {
        let agent_id = format!("{}{:0width$}", template.agent_id_prefix, number, width = template.id_width);
        let group_number = (number - 1) / template.group_size + 1;
        let group_id = format!("{}{:0width$}", template.group_id_prefix, group_number, width = template.id_width);
        let draw = |name: &str| random(&draws, &["profile", &agent_id, name]);
        let mut threshold = draw("mobility") * mobility_total;
        let mut mobility = Mobility::Wheelchair;
        for (choice, weight) in [
            (Mobility::Independent, template.independent_weight),
            (Mobility::Assisted, template.assisted_weight),
            (Mobility::Wheelchair, template.wheelchair_weight),
        ] {
            threshold -= weight;
            if threshold < 0.0 {
                mobility = choice;
                break;
            }
        }
        let range = |value: f64, low: f64, high: f64| low + value * (high - low);
        let support_need = range(draw("support_need"), template.support_need_min, template.support_need_max);
        let stress_sensitivity =
            range(draw("stress_sensitivity"), template.stress_sensitivity_min, template.stress_sensitivity_max);
        let initial_needs = InitialNeeds {
            toilet_urgency: range(draw("toilet_urgency"), template.toilet_urgency_min, template.toilet_urgency_max),
            hunger: range(draw("hunger"), template.hunger_min, template.hunger_max),
            fatigue: range(draw("fatigue"), template.fatigue_min, template.fatigue_max),
        };
        let zone_index = ((draw("initial_zone") * targets.len() as f64) as usize).min(targets.len() - 1);
        profiles.push(Profile {
            rng_key: format!("profile:{agent_id}"),
            id: agent_id,
            group_id: Some(group_id),
            initial_zone_id: targets[zone_index].clone(),
            mobility,
            support_need,
            stress_sensitivity,
            initial_needs,
            generator: template.template_id.clone(),
            seed,
        });
    }
    Ok(ProfileSet {
        profiles,
        manifest: ProfileManifest { seed, template: template.clone(), profile_count: count, zone_ids: available },
    })
}
