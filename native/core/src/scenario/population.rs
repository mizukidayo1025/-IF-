//! `SyntheticPopulationGenerator.generate` of `region/population.py` (Issue #297): the synthetic
//! households, ages, traits and person requirements of a seed, and the assumptions and sources
//! that describe them, as `GeneratedPopulation.model_dump(mode="json")` gives them.
//!
//! The browser's builder calls it as `InitialScenarioBuilder.build` does: one starting zone
//! (`"sleeping"`), the age bands and household mix of the region research (none without e-Stat:
//! the generic assumptions are used), and the generic household templates
//! (`default_household_composition`). The evacuation selection and the population conditions are
//! the request's defaults, which is all the request contract lets through (`request.rs`): no
//! selection multipliers, rates, weights, rhythms or pets, one language (`ja`).

use std::collections::{BTreeMap, BTreeSet};

use crate::mathx::{py_max, py_sum};
use crate::rng::{Key, KeyedDraws};
use crate::scenario::profiles::{
    draws_of_seed, generate_profiles, map, random, text, texts, Mobility, Profile, ProfileSet, ProfileTemplate,
};
use crate::value::V;

pub const SYNTHETIC_AGE_ASSUMPTION_ID: &str = "ASM-SYNTHETIC-AGE-BANDS-V1";
pub const SYNTHETIC_HOUSEHOLD_ASSUMPTION_ID: &str = "ASM-SYNTHETIC-HOUSEHOLD-SIZES-V1";
pub const SYNTHETIC_LANGUAGE_ASSUMPTION_ID: &str = "ASM-SYNTHETIC-LANGUAGE-V1";
pub const SYNTHETIC_TRAIT_ASSUMPTION_ID: &str = "ASM-SYNTHETIC-PERSON-TRAITS-V1";
pub const SYNTHETIC_EVACUATION_SELECTION_ID: &str = "ASM-EVACUATION-SELECTION-UNIFORM-V1";
pub const SYNTHETIC_HOUSEHOLD_CONSTRUCTION_ID: &str = "ASM-HOUSEHOLD-CARE-RELATIONSHIPS-V1";
pub const OPERATOR_POPULATION_SOURCE_ID: &str = "SRC-OPERATOR-POPULATION-CONDITIONS";
pub const SYNTHETIC_HOUSEHOLD_TEMPLATE_ID: &str = "ASM-SYNTHETIC-HOUSEHOLD-TEMPLATES-V1";
/// `GeneratedPopulation.generator`.
pub const POPULATION_GENERATOR_ID: &str = "synthetic-household-profile-v1";
/// The profile template the population generator gives `SyntheticProfileGenerator`.
pub const REGIONAL_PROFILE_TEMPLATE_ID: &str = "synthetic-regional-profile-v1";

/// `AgeBand`: an observed or assumed count of residents in an age range.
#[derive(Clone, Debug, PartialEq)]
pub struct AgeBand {
    pub label: String,
    pub age_min: i64,
    pub age_max: i64,
    pub population_count: i64,
    pub source_ids: Vec<String>,
}

impl AgeBand {
    pub fn dump(&self) -> V {
        map(vec![
            ("label", text(&self.label)),
            ("age_min", V::Int(self.age_min)),
            ("age_max", V::Int(self.age_max)),
            ("population_count", V::Int(self.population_count)),
            ("source_ids", texts(&self.source_ids)),
        ])
    }
}

/// `HouseholdSizeMix`: the weights in the order they were given.
#[derive(Clone, Debug, PartialEq)]
pub struct HouseholdSizeMix {
    pub size_weights: Vec<(i64, f64)>,
    pub source_ids: Vec<String>,
}

impl HouseholdSizeMix {
    pub fn dump(&self) -> V {
        map(vec![
            ("size_weights", size_weights(&self.size_weights)),
            ("source_ids", texts(&self.source_ids)),
        ])
    }
}

fn size_weights(weights: &[(i64, f64)]) -> V {
    V::Map(weights.iter().map(|(size, weight)| (size.to_string().into(), V::Float(*weight))).collect())
}

/// `HouseholdMemberPattern.role`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MemberRole {
    Unspecified,
    Adult,
    Child,
    Caregiver,
    Dependent,
}

impl MemberRole {
    pub fn text(self) -> &'static str {
        match self {
            MemberRole::Unspecified => "unspecified",
            MemberRole::Adult => "adult",
            MemberRole::Child => "child",
            MemberRole::Caregiver => "caregiver",
            MemberRole::Dependent => "dependent",
        }
    }
}

/// `HouseholdMemberPattern`.
#[derive(Clone, Debug, PartialEq)]
pub struct MemberPattern {
    pub role: MemberRole,
    pub age_band_label: Option<String>,
}

/// The kinds of household links.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Relationship {
    ParentChild,
    CaregiverChild,
    CaregiverSupport,
    Partner,
    Relative,
}

impl Relationship {
    pub fn text(self) -> &'static str {
        match self {
            Relationship::ParentChild => "parent_child",
            Relationship::CaregiverChild => "caregiver_child",
            Relationship::CaregiverSupport => "caregiver_support",
            Relationship::Partner => "partner",
            Relationship::Relative => "relative",
        }
    }

    fn is_guardian(self) -> bool {
        matches!(self, Relationship::ParentChild | Relationship::CaregiverChild)
    }
}

/// `HouseholdRelationshipPattern`.
#[derive(Clone, Debug, PartialEq)]
pub struct RelationshipPattern {
    pub caregiver_member_index: usize,
    pub dependent_member_index: usize,
    pub relationship: Relationship,
    pub source_id: Option<String>,
}

/// `HouseholdTemplate`. The caller gives templates whose validator passes.
#[derive(Clone, Debug, PartialEq)]
pub struct HouseholdTemplate {
    pub template_id: String,
    pub weight: f64,
    pub members: Vec<MemberPattern>,
    pub relationships: Vec<RelationshipPattern>,
    pub source_ids: Vec<String>,
}

impl HouseholdTemplate {
    pub fn dump(&self) -> V {
        map(vec![
            ("template_id", text(&self.template_id)),
            ("weight", V::Float(self.weight)),
            (
                "members",
                V::List(
                    self.members
                        .iter()
                        .map(|member| {
                            map(vec![
                                ("role", text(member.role.text())),
                                ("age_band_label", member.age_band_label.as_deref().map_or(V::Null, text)),
                            ])
                        })
                        .collect(),
                ),
            ),
            (
                "relationships",
                V::List(
                    self.relationships
                        .iter()
                        .map(|link| {
                            map(vec![
                                ("caregiver_member_index", V::Int(link.caregiver_member_index as i64)),
                                ("dependent_member_index", V::Int(link.dependent_member_index as i64)),
                                ("relationship", text(link.relationship.text())),
                                ("source_id", link.source_id.as_deref().map_or(V::Null, text)),
                            ])
                        })
                        .collect(),
                ),
            ),
            ("source_ids", texts(&self.source_ids)),
        ])
    }
}

/// Who declared a household composition.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CompositionKind {
    Operator,
    Assumption,
}

impl CompositionKind {
    pub fn text(self) -> &'static str {
        match self {
            CompositionKind::Operator => "operator",
            CompositionKind::Assumption => "assumption",
        }
    }
}

/// `HouseholdCompositionInput`.
#[derive(Clone, Debug, PartialEq)]
pub struct HouseholdComposition {
    pub templates: Vec<HouseholdTemplate>,
    pub household_size_weights: Vec<(i64, f64)>,
    pub source_ids: Vec<String>,
    pub source_kind: CompositionKind,
}

impl HouseholdComposition {
    pub fn dump(&self) -> V {
        map(vec![
            ("templates", V::List(self.templates.iter().map(HouseholdTemplate::dump).collect())),
            ("household_size_weights", size_weights(&self.household_size_weights)),
            ("source_ids", texts(&self.source_ids)),
            ("source_kind", text(self.source_kind.text())),
        ])
    }
}

/// `DEFAULT_SYNTHETIC_HOUSEHOLD_COMPOSITION`: generic templates linking one adult to each child.
pub fn default_household_composition() -> HouseholdComposition {
    fn template(template_id: String, adults: usize, children: usize, weight: f64) -> HouseholdTemplate {
        let mut members = vec![MemberPattern { role: MemberRole::Adult, age_band_label: None }; adults];
        members.extend(vec![MemberPattern { role: MemberRole::Child, age_band_label: None }; children]);
        HouseholdTemplate {
            template_id,
            weight,
            members,
            relationships: (0..children)
                .map(|child| RelationshipPattern {
                    caregiver_member_index: 0,
                    dependent_member_index: adults + child,
                    relationship: Relationship::ParentChild,
                    source_id: None,
                })
                .collect(),
            source_ids: Vec::new(),
        }
    }
    let mut templates = vec![
        template("adult-1".into(), 1, 0, 1.0),
        template("adults-2".into(), 2, 0, 0.9),
        template("adult-1-children-1".into(), 1, 1, 0.1),
        template("adults-3".into(), 3, 0, 0.6),
        template("adults-2-children-1".into(), 2, 1, 0.4),
        template("adults-4".into(), 4, 0, 0.4),
        template("adults-3-children-1".into(), 3, 1, 0.2),
        template("adults-2-children-2".into(), 2, 2, 0.4),
    ];
    for size in 5..11 {
        templates.push(template(format!("adults-{size}"), size, 0, 0.3));
        templates.push(template(format!("adults-3-children-{}", size - 3), 3, size - 3, 0.4));
        templates.push(template(format!("adults-2-children-{}", size - 2), 2, size - 2, 0.3));
    }
    HouseholdComposition {
        templates,
        household_size_weights: Vec::new(),
        source_ids: vec![SYNTHETIC_HOUSEHOLD_TEMPLATE_ID.into()],
        source_kind: CompositionKind::Assumption,
    }
}

/// What `generate` takes, as the builder passes it.
#[derive(Clone, Debug, PartialEq)]
pub struct PopulationInput {
    pub count: i64,
    pub seed: i64,
    pub zone_ids: Vec<String>,
    /// Empty: the generic equal-weight bands (an assumption).
    pub age_bands: Vec<AgeBand>,
    /// None: the generic household sizes (an assumption).
    pub household_mix: Option<HouseholdSizeMix>,
    pub household_composition: Option<HouseholdComposition>,
}

impl PopulationInput {
    /// What `InitialScenarioBuilder.build` gives for a request without research statistics.
    pub fn for_request(count: i64, seed: i64) -> Self {
        Self {
            count,
            seed,
            zone_ids: vec!["sleeping".into()],
            age_bands: Vec::new(),
            household_mix: None,
            household_composition: Some(default_household_composition()),
        }
    }
}

/// `PersonTraits` (one language, `ja`).
#[derive(Clone, Debug, PartialEq)]
pub struct PersonTraits {
    pub age_years: i64,
    pub household_id: String,
    pub household_role: &'static str,
    pub languages: Vec<String>,
    pub accessibility_needs: Vec<String>,
    pub medical_support: bool,
    pub source_ids: Vec<String>,
}

impl PersonTraits {
    pub fn dump(&self) -> V {
        map(vec![
            ("age_years", V::Int(self.age_years)),
            ("household_id", text(&self.household_id)),
            ("household_role", text(self.household_role)),
            ("languages", texts(&self.languages)),
            ("accessibility_needs", texts(&self.accessibility_needs)),
            ("medical_support", V::Bool(self.medical_support)),
            ("source_ids", texts(&self.source_ids)),
        ])
    }
}

/// `HouseholdRelationship`.
#[derive(Clone, Debug, PartialEq)]
pub struct HouseholdRelationship {
    pub household_id: String,
    pub caregiver_id: String,
    pub dependent_id: String,
    pub relationship: Relationship,
    pub source_id: String,
}

impl HouseholdRelationship {
    pub fn dump(&self) -> V {
        map(vec![
            ("household_id", text(&self.household_id)),
            ("caregiver_id", text(&self.caregiver_id)),
            ("dependent_id", text(&self.dependent_id)),
            ("relationship", text(self.relationship.text())),
            ("source_id", text(&self.source_id)),
        ])
    }
}

/// `GeneratedPersonRequirements` without information access, pets or rhythms (no condition
/// asks for them).
#[derive(Clone, Debug, PartialEq)]
pub struct PersonRequirements {
    pub person_id: String,
    pub household_id: String,
    pub household_role: &'static str,
    pub guardian_status: &'static str,
    pub caregiver_ids: Vec<String>,
    pub dependent_ids: Vec<String>,
    pub medical_support_required: Option<bool>,
    pub care_support_needs: Vec<&'static str>,
    pub review_reasons: Vec<&'static str>,
}

impl PersonRequirements {
    pub fn dump(&self) -> V {
        map(vec![
            ("person_id", text(&self.person_id)),
            ("household_id", text(&self.household_id)),
            ("household_role", text(self.household_role)),
            ("guardian_status", text(self.guardian_status)),
            ("caregiver_ids", texts(&self.caregiver_ids)),
            ("dependent_ids", texts(&self.dependent_ids)),
            ("medical_support_required", self.medical_support_required.map_or(V::Null, V::Bool)),
            ("medication_support_required", V::Null),
            ("care_support_needs", texts(&self.care_support_needs)),
            ("information_access", V::Null),
            ("pet_count", V::Null),
            ("daily_rhythm_id", V::Null),
            ("sleep_start_minute", V::Null),
            ("wake_minute", V::Null),
            ("requires_review", V::Bool(!self.review_reasons.is_empty())),
            ("review_reasons", texts(&self.review_reasons)),
        ])
    }
}

/// `GeneratedPopulation`.
#[derive(Clone, Debug, PartialEq)]
pub struct GeneratedPopulation {
    /// The profiles with their household as `group_id`, and the profile generator's manifest.
    pub profile_set: ProfileSet,
    /// `PopulationSpec.people`, by profile in order.
    pub people: Vec<(String, PersonTraits)>,
    /// `PopulationSpec.source_ids`, sorted.
    pub source_ids: Vec<String>,
    pub household_sizes: Vec<(String, i64)>,
    pub age_bands: Vec<AgeBand>,
    pub resident_age_distribution: Vec<(String, f64)>,
    pub evacuee_age_distribution: Vec<(String, f64)>,
    pub household_mix: HouseholdSizeMix,
    pub household_relationships: Vec<HouseholdRelationship>,
    pub household_template_ids: Vec<(String, String)>,
    pub unselected_template_ids: Vec<String>,
    pub household_composition: Option<HouseholdComposition>,
    pub person_requirements: Vec<PersonRequirements>,
    /// Free-form records, as Python writes them.
    pub assumptions: Vec<V>,
    pub sources: Vec<V>,
}

fn distribution(values: &[(String, f64)]) -> V {
    V::Map(values.iter().map(|(label, share)| (label.as_str().into(), V::Float(*share))).collect())
}

/// `EvacuationSelection()`'s dump.
pub fn default_evacuation_selection() -> V {
    map(vec![("age_band_multipliers", V::Map(Default::default())), ("source_ids", V::List(Vec::new()))])
}

/// `PopulationConditionInput()`'s dump.
pub fn default_population_conditions() -> V {
    let empty_map = || V::Map(Default::default());
    map(vec![
        ("medical_support_rate", V::Null),
        ("medication_support_rate", V::Null),
        ("care_support_need_rates", empty_map()),
        ("pet_household_rate", V::Null),
        ("pet_count_weights", empty_map()),
        ("information_access_weights", empty_map()),
        ("language_weights", empty_map()),
        ("additional_access_need_rates", empty_map()),
        ("daily_rhythms", V::List(Vec::new())),
        ("household_composition", V::Null),
        ("source_ids", V::List(Vec::new())),
    ])
}

impl GeneratedPopulation {
    /// `PopulationSpec.model_dump(mode="json")` (`model_configs.population`).
    pub fn population_dump(&self) -> V {
        map(vec![
            ("schema_version", V::Int(1)),
            ("people", V::Map(self.people.iter().map(|(id, traits)| (id.as_str().into(), traits.dump())).collect())),
            ("source_ids", texts(&self.source_ids)),
        ])
    }

    pub fn resident_age_distribution_dump(&self) -> V {
        distribution(&self.resident_age_distribution)
    }

    pub fn evacuee_age_distribution_dump(&self) -> V {
        distribution(&self.evacuee_age_distribution)
    }

    pub fn household_sizes_dump(&self) -> V {
        V::Map(self.household_sizes.iter().map(|(id, size)| (id.as_str().into(), V::Int(*size))).collect())
    }

    /// `GeneratedPopulation.model_dump(mode="json")`.
    pub fn dump(&self) -> V {
        map(vec![
            ("profile_set", self.profile_set.dump()),
            ("population", self.population_dump()),
            ("household_sizes", self.household_sizes_dump()),
            ("age_bands", V::List(self.age_bands.iter().map(AgeBand::dump).collect())),
            ("resident_age_distribution", self.resident_age_distribution_dump()),
            ("evacuee_age_distribution", self.evacuee_age_distribution_dump()),
            ("household_mix", self.household_mix.dump()),
            ("evacuation_selection", default_evacuation_selection()),
            (
                "household_relationships",
                V::List(self.household_relationships.iter().map(HouseholdRelationship::dump).collect()),
            ),
            (
                "household_template_ids",
                V::Map(self.household_template_ids.iter().map(|(id, template)| (id.as_str().into(), text(template))).collect()),
            ),
            ("unselected_template_ids", texts(&self.unselected_template_ids)),
            ("household_composition", self.household_composition.as_ref().map_or(V::Null, HouseholdComposition::dump)),
            ("person_requirements", V::List(self.person_requirements.iter().map(PersonRequirements::dump).collect())),
            ("trait_input", default_population_conditions()),
            ("generator", text(POPULATION_GENERATOR_ID)),
            ("assumptions", V::List(self.assumptions.clone())),
            ("sources", V::List(self.sources.clone())),
        ])
    }
}

/// `_normalize_weights`: the positive weights over their total, by key.
fn normalize_weights(weights: &[(String, f64)]) -> Vec<(String, f64)> {
    let total = py_sum(&weights.iter().map(|(_, weight)| *weight).collect::<Vec<_>>());
    if weights.is_empty() || !total.is_finite() || total <= 0.0 {
        return Vec::new();
    }
    let mut sorted: Vec<&(String, f64)> = weights.iter().collect();
    sorted.sort_by(|first, second| first.0.cmp(&second.0));
    sorted.into_iter().filter(|(_, weight)| *weight > 0.0).map(|(key, weight)| (key.clone(), weight / total)).collect()
}

/// `_draw_weighted_key`.
fn draw_weighted_key(draw: f64, weights: &[(String, f64)]) -> Result<String, String> {
    let Some(last) = weights.last() else {
        return Err("a weighted choice requires at least one positive option".into());
    };
    let mut threshold = draw;
    for (key, weight) in weights {
        threshold -= weight;
        if threshold < 0.0 {
            return Ok(key.clone());
        }
    }
    Ok(last.0.clone())
}

/// `_draw_weighted_size`.
fn draw_weighted_size(draw: f64, mix: &HouseholdSizeMix) -> i64 {
    let total = py_sum(&mix.size_weights.iter().map(|(_, weight)| *weight).collect::<Vec<_>>());
    let mut threshold = draw * total;
    let mut sorted = mix.size_weights.clone();
    sorted.sort_by_key(|(size, _)| *size);
    for (size, weight) in &sorted {
        threshold -= weight;
        if threshold < 0.0 {
            return *size;
        }
    }
    sorted.last().map_or(1, |(size, _)| *size)
}

fn share(distribution: &[(String, f64)], label: &str) -> f64 {
    distribution.iter().find(|(key, _)| key == label).map_or(0.0, |(_, value)| *value)
}

/// `_maximum_supported_age`.
fn maximum_supported_age(
    bands: &[AgeBand],
    selected: &[(String, f64)],
    minimum_age: i64,
    maximum_age: i64,
    label: Option<&str>,
) -> Option<i64> {
    bands
        .iter()
        .filter(|band| {
            label.is_none_or(|label| band.label == label)
                && share(selected, &band.label) > 0.0
                && band.age_min.max(minimum_age) <= band.age_max.min(maximum_age)
        })
        .map(|band| band.age_max.min(maximum_age))
        .max()
}

fn no_support(profile_id: &str, minimum_age: i64, maximum_age: i64) -> String {
    format!(
        "household template has no selected age support for person slot {} within ages {minimum_age}..{maximum_age}",
        crate::text::py_repr(profile_id)
    )
}

/// `_draw_age_with_constraints`.
fn draw_age_with_constraints(
    profile_id: &str,
    bands: &[AgeBand],
    selected: &[(String, f64)],
    draws: &KeyedDraws,
    minimum_age: i64,
    maximum_age: i64,
    label: Option<&str>,
) -> Result<i64, String> {
    let mut candidates: Vec<(&AgeBand, i64, i64, f64)> = Vec::new();
    for band in bands {
        let lower = band.age_min.max(minimum_age);
        let upper = band.age_max.min(maximum_age);
        if label.is_some_and(|label| band.label != label) || lower > upper {
            continue;
        }
        let weight = share(selected, &band.label);
        if weight <= 0.0 {
            continue;
        }
        let eligible = (upper - lower + 1) as f64 / (band.age_max - band.age_min + 1) as f64;
        candidates.push((band, lower, upper, weight * eligible));
    }
    if candidates.is_empty() {
        return Err(no_support(profile_id, minimum_age, maximum_age));
    }
    // A dict keyed by label: a later band of the same label replaces the weight.
    let mut weights: Vec<(String, f64)> = Vec::new();
    for (band, _, _, weight) in &candidates {
        match weights.iter_mut().find(|(key, _)| *key == band.label) {
            Some(entry) => entry.1 = *weight,
            None => weights.push((band.label.clone(), *weight)),
        }
    }
    let chosen = draw_weighted_key(random(draws, &["population", profile_id, "age_band"]), &normalize_weights(&weights))?;
    let (_, lower, upper, _) = candidates.iter().find(|item| item.0.label == chosen).expect("a chosen candidate");
    let span = upper - lower + 1;
    let offset = (random(draws, &["population", profile_id, "age_years"]) * span as f64) as i64;
    Ok(lower + (span - 1).min(offset))
}

/// `_draw_age_from_distribution`.
fn draw_age_from_distribution(profile_id: &str, bands: &[AgeBand], selected: &[(String, f64)], draws: &KeyedDraws) -> Result<i64, String> {
    let mut threshold = random(draws, &["population", profile_id, "age_band"]);
    let mut chosen = bands
        .iter()
        .rev()
        .find(|band| share(selected, &band.label) > 0.0)
        .ok_or_else(|| "no age band has a positive share".to_string())?;
    for band in bands {
        threshold -= share(selected, &band.label);
        if threshold < 0.0 {
            chosen = band;
            break;
        }
    }
    let width = chosen.age_max - chosen.age_min + 1;
    let offset = (random(draws, &["population", profile_id, "age"]) * width as f64) as i64;
    Ok((chosen.age_min + offset).min(chosen.age_max))
}

/// `_sample_household_ages`: the age of each member (index of the profile).
fn sample_household_ages(
    members: &[usize],
    profiles: &[Profile],
    template: Option<&HouseholdTemplate>,
    bands: &[AgeBand],
    selected: &[(String, f64)],
    draws: &KeyedDraws,
) -> Result<Vec<(usize, i64)>, String> {
    let Some(template) = template else {
        return members
            .iter()
            .map(|&index| Ok((index, draw_age_from_distribution(&profiles[index].id, bands, selected, draws)?)))
            .collect();
    };
    let slots = template.members.len();
    let mut children_by_caregiver: BTreeMap<usize, BTreeSet<usize>> = BTreeMap::new();
    let mut adult_slots = BTreeSet::new();
    let mut child_slots = BTreeSet::new();
    for link in &template.relationships {
        let (caregiver, dependent) = (link.caregiver_member_index, link.dependent_member_index);
        match link.relationship {
            Relationship::ParentChild | Relationship::CaregiverChild => {
                children_by_caregiver.entry(caregiver).or_default().insert(dependent);
                adult_slots.insert(caregiver);
                child_slots.insert(dependent);
            }
            Relationship::CaregiverSupport => {
                adult_slots.insert(caregiver);
            }
            Relationship::Partner => {
                adult_slots.insert(caregiver);
                adult_slots.insert(dependent);
            }
            Relationship::Relative => {}
        }
    }
    let cycles = || "parent/caregiver-child template relationships cannot contain cycles".to_string();
    // Children before their caregivers.
    let mut indegree = vec![0usize; slots];
    let mut caregivers_of: BTreeMap<usize, BTreeSet<usize>> = BTreeMap::new();
    for (caregiver, dependents) in &children_by_caregiver {
        for dependent in dependents {
            caregivers_of.entry(*dependent).or_default().insert(*caregiver);
            indegree[*caregiver] += 1;
        }
    }
    let mut ready: std::collections::VecDeque<usize> = (0..slots).filter(|slot| indegree[*slot] == 0).collect();
    let mut order = Vec::with_capacity(slots);
    while let Some(current) = ready.pop_front() {
        order.push(current);
        for caregiver in caregivers_of.get(&current).into_iter().flatten() {
            indegree[*caregiver] -= 1;
            if indegree[*caregiver] == 0 {
                ready.push_back(*caregiver);
            }
        }
    }
    if order.len() != slots {
        return Err(cycles());
    }
    let mut minimum_by_slot = vec![0i64; slots];
    let mut maximum_by_slot = vec![0i64; slots];
    for (slot, pattern) in template.members.iter().enumerate() {
        let minimum_age =
            if matches!(pattern.role, MemberRole::Adult | MemberRole::Caregiver) || adult_slots.contains(&slot) { 18 } else { 0 };
        let maximum_age = if pattern.role == MemberRole::Child || child_slots.contains(&slot) { 17 } else { 120 };
        let Some(supported) = maximum_supported_age(bands, selected, minimum_age, maximum_age, pattern.age_band_label.as_deref())
        else {
            return Err(no_support(&profiles[members[slot]].id, minimum_age, maximum_age));
        };
        minimum_by_slot[slot] = minimum_age;
        maximum_by_slot[slot] = supported;
    }
    // Caregivers' ceilings, from older generations toward dependents.
    let mut remaining: Vec<usize> = (0..slots).map(|slot| caregivers_of.get(&slot).map_or(0, BTreeSet::len)).collect();
    let mut forward: std::collections::VecDeque<usize> = (0..slots).filter(|slot| remaining[*slot] == 0).collect();
    let mut propagated = maximum_by_slot.clone();
    let mut visited = 0;
    while let Some(caregiver) = forward.pop_front() {
        visited += 1;
        for dependent in children_by_caregiver.get(&caregiver).into_iter().flatten() {
            propagated[*dependent] = propagated[*dependent].min(propagated[caregiver] - 16);
            remaining[*dependent] -= 1;
            if remaining[*dependent] == 0 {
                forward.push_back(*dependent);
            }
        }
    }
    if visited != slots {
        return Err(cycles());
    }
    let mut ages: Vec<Option<i64>> = vec![None; slots];
    for &slot in &order {
        let pattern = &template.members[slot];
        let mut minimum_age = minimum_by_slot[slot];
        let dependent_ages: Vec<i64> =
            children_by_caregiver.get(&slot).into_iter().flatten().filter_map(|dependent| ages[*dependent]).collect();
        if let Some(oldest) = dependent_ages.iter().max() {
            minimum_age = minimum_age.max(oldest + 16).max(18);
        }
        ages[slot] = Some(draw_age_with_constraints(
            &profiles[members[slot]].id,
            bands,
            selected,
            draws,
            minimum_age,
            propagated[slot],
            pattern.age_band_label.as_deref(),
        )?);
    }
    Ok(order.iter().map(|slot| (members[*slot], ages[*slot].unwrap_or_default())).collect())
}

fn assumption(id: &str, subject: &str, value: V, reason: &str) -> V {
    map(vec![("id", text(id)), ("subject", text(subject)), ("value", value), ("reason", text(reason)), ("source_id", text(id))])
}

fn source(id: &str, title: &str, kind: &str, description: &str) -> V {
    map(vec![("id", text(id)), ("title", text(title)), ("source_kind", text(kind)), ("description", text(description))])
}

const NOT_OFFICIAL: &str = "Not an official statistic and not calibrated to the requested municipality.";

/// `SyntheticPopulationGenerator().generate(...)` with the default selection and conditions and
/// one language. The texts of the errors are Python's.
pub fn generate_population(input: &PopulationInput) -> Result<GeneratedPopulation, String> {
    let mut zones = input.zone_ids.clone();
    zones.sort();
    zones.dedup();
    if zones.is_empty() {
        return Err("synthetic population needs at least one starting zone".into());
    }
    if !(1..=10_000).contains(&input.count) {
        return Err("synthetic population count must be between 1 and 10,000".into());
    }
    if input.seed < 0 {
        return Err("seed must be a nonnegative integer".into());
    }
    let mut household_composition = input.household_composition.clone();
    let mut bands = input.age_bands.clone();
    let mut mix = input.household_mix.clone();
    if mix.is_none() {
        if let Some(composition) = household_composition.as_ref().filter(|item| !item.household_size_weights.is_empty()) {
            mix = Some(HouseholdSizeMix {
                size_weights: composition.household_size_weights.clone(),
                source_ids: if composition.source_ids.is_empty() {
                    vec![OPERATOR_POPULATION_SOURCE_ID.into()]
                } else {
                    composition.source_ids.clone()
                },
            });
        }
    }
    let age_assumed = bands.is_empty();
    let household_assumed = mix.is_none();
    if age_assumed {
        let band = |label: &str, age_min, age_max| AgeBand {
            label: label.into(),
            age_min,
            age_max,
            population_count: 1,
            source_ids: vec![SYNTHETIC_AGE_ASSUMPTION_ID.into()],
        };
        bands = vec![band("child", 0, 17), band("adult", 18, 64), band("older_adult", 65, 90)];
    }
    let population_total: i64 = bands.iter().map(|band| band.population_count).sum();
    if population_total <= 0 {
        return Err("resident age bands must contain a positive observed or assumed population total".into());
    }
    let mut ordered: Vec<&AgeBand> = bands.iter().collect();
    ordered.sort_by_key(|band| band.age_min);
    if ordered.windows(2).any(|pair| pair[1].age_min <= pair[0].age_max) {
        return Err("age bands must not overlap".into());
    }
    let mix = mix.unwrap_or_else(|| HouseholdSizeMix {
        size_weights: vec![(1, 0.20), (2, 0.25), (3, 0.25), (4, 0.20), (5, 0.10)],
        source_ids: vec![SYNTHETIC_HOUSEHOLD_ASSUMPTION_ID.into()],
    });

    // `_resident_age_distribution` and `_evacuee_age_distribution` (no multipliers).
    let resident: Vec<(String, f64)> =
        bands.iter().map(|band| (band.label.clone(), band.population_count as f64 / population_total as f64)).collect();
    let evacuee_total = py_sum(&resident.iter().map(|(_, value)| *value).collect::<Vec<_>>());
    if evacuee_total <= 0.0 || !evacuee_total.is_finite() {
        return Err("resident distribution and evacuation selection have no positive age-band weight".into());
    }
    let evacuee: Vec<(String, f64)> = resident.iter().map(|(label, value)| (label.clone(), value / evacuee_total)).collect();
    let language_weights = vec![("ja".to_string(), 1.0)];

    let template = ProfileTemplate {
        template_id: REGIONAL_PROFILE_TEMPLATE_ID.into(),
        group_size: 1,
        initial_zone_ids: zones.clone(),
        ..ProfileTemplate::default()
    };
    let profile_set = generate_profiles(input.count, input.seed, &zones, &template)?;
    let profiles = &profile_set.profiles;
    if let Some(composition) = household_composition.as_mut() {
        if composition.source_kind == CompositionKind::Assumption
            && maximum_supported_age(&bands, &evacuee, 0, 17, None).is_none()
        {
            composition.templates.retain(|template| template.members.iter().all(|member| member.role != MemberRole::Child));
        }
    }
    let draws = draws_of_seed(input.seed);

    // `HouseholdGenerator.group_indices`.
    let count = input.count as usize;
    let mut household_sizes: Vec<(String, i64)> = Vec::new();
    let mut household_of: Vec<usize> = Vec::with_capacity(count);
    let mut index = 0usize;
    while index < count {
        let mut key = Key::new();
        key.text("population").text("household").int(index as i64);
        let size = (draw_weighted_size(key.draw(&draws), &mix) as usize).min(count - index).max(1);
        household_of.extend(std::iter::repeat_n(household_sizes.len(), size));
        household_sizes.push((format!("H{:06}", household_sizes.len() + 1), size as i64));
        index += size;
    }
    // `HouseholdGenerator.choose_templates`: households are already in the order of their IDs.
    let mut chosen: Vec<Option<&HouseholdTemplate>> = vec![None; household_sizes.len()];
    if let Some(composition) = household_composition.as_ref() {
        for (number, (_, size)) in household_sizes.iter().enumerate() {
            let mut candidates: Vec<&HouseholdTemplate> =
                composition.templates.iter().filter(|template| template.members.len() as i64 == *size).collect();
            if candidates.is_empty() {
                continue;
            }
            candidates.sort_by(|first, second| first.template_id.cmp(&second.template_id));
            let weights = normalize_weights(&candidates.iter().map(|item| (item.template_id.clone(), item.weight)).collect::<Vec<_>>());
            let template_id =
                draw_weighted_key(random(&draws, &["population", "household_template", &(number + 1).to_string()]), &weights)?;
            chosen[number] = candidates.into_iter().find(|item| item.template_id == template_id);
        }
    }
    let band_labels: BTreeSet<&str> = bands.iter().map(|band| band.label.as_str()).collect();
    let unknown: BTreeSet<&str> = chosen
        .iter()
        .flatten()
        .flat_map(|template| template.members.iter())
        .filter_map(|member| member.age_band_label.as_deref())
        .filter(|label| !band_labels.contains(label))
        .collect();
    if !unknown.is_empty() {
        return Err(format!(
            "household template references unknown age bands: {}",
            unknown.into_iter().collect::<Vec<_>>().join(", ")
        ));
    }

    let mut source_id_set: BTreeSet<String> = bands.iter().flat_map(|band| band.source_ids.iter().cloned()).collect();
    source_id_set.extend(mix.source_ids.iter().cloned());
    source_id_set.insert(SYNTHETIC_TRAIT_ASSUMPTION_ID.into());
    source_id_set.insert(SYNTHETIC_LANGUAGE_ASSUMPTION_ID.into());
    let mut declared_household_source_ids: BTreeSet<String> = BTreeSet::new();
    if let Some(composition) = household_composition.as_ref() {
        declared_household_source_ids.extend(composition.source_ids.iter().cloned());
        for template in &composition.templates {
            declared_household_source_ids.extend(template.source_ids.iter().cloned());
            declared_household_source_ids.extend(template.relationships.iter().filter_map(|link| link.source_id.clone()));
        }
        if composition.source_kind == CompositionKind::Operator && composition.source_ids.is_empty() {
            declared_household_source_ids.insert(OPERATOR_POPULATION_SOURCE_ID.into());
        }
    }
    source_id_set.extend(declared_household_source_ids.iter().cloned());
    let operator_composition = household_composition.as_ref().is_some_and(|item| item.source_kind == CompositionKind::Operator);
    // Only an operator's composition is explicit condition input here (no rates, weights,
    // rhythms or selection multipliers); it then cites the operator's conditions.
    if operator_composition {
        source_id_set.insert(OPERATOR_POPULATION_SOURCE_ID.into());
    }
    let source_ids: Vec<String> = source_id_set.into_iter().collect();

    let mut household_members: Vec<Vec<usize>> = vec![Vec::new(); household_sizes.len()];
    for (index, household) in household_of.iter().enumerate() {
        household_members[*household].push(index);
    }
    let mut pattern_of: Vec<Option<&MemberPattern>> = vec![None; count];
    for (household, template) in chosen.iter().enumerate() {
        if let Some(template) = template {
            for (member, pattern) in household_members[household].iter().zip(&template.members) {
                pattern_of[*member] = Some(pattern);
            }
        }
    }
    let mut relationships: Vec<HouseholdRelationship> = Vec::new();
    let mut child_caregivers: BTreeMap<usize, BTreeSet<usize>> = BTreeMap::new();
    let mut support_caregivers: BTreeMap<usize, BTreeSet<usize>> = BTreeMap::new();
    let mut support_required_by_relationship: BTreeSet<usize> = BTreeSet::new();
    let explicit_dependents: BTreeSet<usize> =
        (0..count).filter(|index| pattern_of[*index].is_some_and(|pattern| pattern.role == MemberRole::Dependent)).collect();
    for (household, template) in chosen.iter().enumerate() {
        let Some(template) = template else { continue };
        let members = &household_members[household];
        for link in &template.relationships {
            let caregiver = members[link.caregiver_member_index];
            let dependent = members[link.dependent_member_index];
            let source_id = link
                .source_id
                .clone()
                .or_else(|| template.source_ids.first().cloned())
                .or_else(|| household_composition.as_ref().and_then(|item| item.source_ids.first().cloned()))
                .unwrap_or_else(|| OPERATOR_POPULATION_SOURCE_ID.into());
            relationships.push(HouseholdRelationship {
                household_id: household_sizes[household].0.clone(),
                caregiver_id: profiles[caregiver].id.clone(),
                dependent_id: profiles[dependent].id.clone(),
                relationship: link.relationship,
                source_id,
            });
            if link.relationship.is_guardian() {
                child_caregivers.entry(dependent).or_default().insert(caregiver);
            }
            if link.relationship == Relationship::CaregiverSupport {
                support_required_by_relationship.insert(dependent);
                support_caregivers.entry(dependent).or_default().insert(caregiver);
            }
        }
    }

    let mut ages = vec![0i64; count];
    for (household, members) in household_members.iter().enumerate() {
        for (index, age) in sample_household_ages(members, profiles, chosen[household], &bands, &evacuee, &draws)? {
            ages[index] = age;
        }
    }

    let mut updated_profiles = Vec::with_capacity(count);
    let mut people = Vec::with_capacity(count);
    let mut requirements = Vec::with_capacity(count);
    for (index, profile) in profiles.iter().enumerate() {
        let household = household_of[index];
        let (household_id, household_size) = &household_sizes[household];
        let age = ages[index];
        let medical_support =
            support_required_by_relationship.contains(&index) || pattern_of[index].is_some_and(|pattern| pattern.role == MemberRole::Dependent);
        let care_support_needs: Vec<&'static str> = if medical_support { vec!["medical_assessment"] } else { Vec::new() };
        let support_required = medical_support;
        let role = if age < 18 {
            "child"
        } else if explicit_dependents.contains(&index) || (support_required && support_caregivers.contains_key(&index)) {
            "dependent"
        } else if *household_size == 1 {
            "alone"
        } else {
            "adult"
        };
        let mut updated = profile.clone();
        updated.group_id = Some(household_id.clone());
        if support_required || !care_support_needs.is_empty() {
            updated.support_need = py_max(updated.support_need, 0.5);
        }
        // One language with all the weight: the draw always picks it.
        let language = draw_weighted_key(0.0, &normalize_weights(&language_weights))?;
        people.push((
            updated.id.clone(),
            PersonTraits {
                age_years: age,
                household_id: household_id.clone(),
                household_role: role,
                languages: vec![language],
                accessibility_needs: if updated.mobility == Mobility::Independent {
                    Vec::new()
                } else {
                    vec!["mobility_support".into()]
                },
                medical_support: support_required,
                source_ids: source_ids.clone(),
            },
        ));
        let caregiver_indexes: BTreeSet<usize> = child_caregivers
            .get(&index)
            .into_iter()
            .chain(support_caregivers.get(&index))
            .flatten()
            .copied()
            .collect();
        let caregivers: Vec<String> = caregiver_indexes.iter().map(|item| profiles[*item].id.clone()).collect();
        let dependent_indexes: BTreeSet<usize> = child_caregivers
            .iter()
            .chain(support_caregivers.iter())
            .filter(|(_, caregivers)| caregivers.contains(&index))
            .map(|(dependent, _)| *dependent)
            .collect();
        let mut review_reasons = Vec::new();
        if age < 18 && !child_caregivers.contains_key(&index) {
            review_reasons.push("unaccompanied_minor");
        }
        if support_required && caregivers.is_empty() {
            review_reasons.push("support_need_without_explicit_household_caregiver");
        }
        requirements.push(PersonRequirements {
            person_id: updated.id.clone(),
            household_id: household_id.clone(),
            household_role: role,
            guardian_status: if age >= 18 {
                "not_applicable"
            } else if child_caregivers.contains_key(&index) {
                "accompanied"
            } else {
                "unaccompanied"
            },
            caregiver_ids: caregivers,
            dependent_ids: dependent_indexes.iter().map(|item| profiles[*item].id.clone()).collect(),
            medical_support_required: (support_required_by_relationship.contains(&index) || explicit_dependents.contains(&index))
                .then_some(medical_support),
            care_support_needs,
            review_reasons,
        });
        updated_profiles.push(updated);
    }

    let mut assumptions = Vec::new();
    let mut sources = Vec::new();
    if age_assumed {
        assumptions.push(assumption(
            SYNTHETIC_AGE_ASSUMPTION_ID,
            "synthetic_age_band_mix",
            map(vec![("child", V::Int(1)), ("adult", V::Int(1)), ("older_adult", V::Int(1))]),
            "No selected regional age table was supplied; equal band weights are only a generic simulation default.",
        ));
        sources.push(source(SYNTHETIC_AGE_ASSUMPTION_ID, "Generic synthetic age-band assumption", "assumption", NOT_OFFICIAL));
    }
    if household_assumed {
        assumptions.push(assumption(
            SYNTHETIC_HOUSEHOLD_ASSUMPTION_ID,
            "synthetic_household_size_mix",
            size_weights(&mix.size_weights),
            "No selected regional household table was supplied; weights are a generator heuristic.",
        ));
        sources.push(source(
            SYNTHETIC_HOUSEHOLD_ASSUMPTION_ID,
            "Generic synthetic household-size assumption",
            "assumption",
            NOT_OFFICIAL,
        ));
    }
    assumptions.push(assumption(
        SYNTHETIC_LANGUAGE_ASSUMPTION_ID,
        "synthetic_language_mix",
        V::List(
            normalize_weights(&language_weights)
                .into_iter()
                .map(|(language, weight)| V::List(vec![text(&language), V::Float(weight)]))
                .collect(),
        ),
        "No source was supplied for the language assignment weights; one primary language is sampled per synthetic \
         profile and the weights are not calibrated to the requested region.",
    ));
    sources.push(source(
        SYNTHETIC_LANGUAGE_ASSUMPTION_ID,
        "Synthetic language assignment assumption",
        "assumption",
        "One primary language is sampled per synthetic profile; not calibrated to census, visa, or municipal language data.",
    ));
    assumptions.push(assumption(
        SYNTHETIC_EVACUATION_SELECTION_ID,
        "evacuation_selection_by_age",
        V::Map(bands.iter().map(|band| (band.label.as_str().into(), V::Float(1.0))).collect()),
        "No evacuation-selection data was supplied; selection is uniform across the observed resident distribution and \
         is not an official evacuation rate.",
    ));
    sources.push(source(
        SYNTHETIC_EVACUATION_SELECTION_ID,
        "Uniform synthetic evacuation-selection assumption",
        "assumption",
        "No evacuation-rate data was supplied; the resident distribution is used without an age-specific selection \
         adjustment.",
    ));
    assumptions.push(assumption(
        SYNTHETIC_HOUSEHOLD_CONSTRUCTION_ID,
        "household_relationship_policy",
        map(vec![
            ("minor_age_threshold", V::Int(18)),
            ("minimum_parent_age_gap", V::Int(16)),
            ("relationships_created_from_age_or_co_residence", V::Bool(false)),
            ("unlinked_minor_requires_review", V::Bool(true)),
        ]),
        "A shared household ID is not evidence of a family relationship. Parent, partner, and care links are created \
         only from explicit household templates; age checks only reject implausible declared links. Unlinked minors \
         remain flagged for operator review.",
    ));
    let selected_ids: BTreeSet<&str> = chosen.iter().flatten().map(|template| template.template_id.as_str()).collect();
    let unselected_template_ids: Vec<String> = household_composition
        .as_ref()
        .map(|composition| {
            composition
                .templates
                .iter()
                .map(|template| template.template_id.as_str())
                .filter(|id| !selected_ids.contains(id))
                .collect::<BTreeSet<_>>()
                .into_iter()
                .map(String::from)
                .collect()
        })
        .unwrap_or_default();
    if operator_composition && !unselected_template_ids.is_empty() {
        assumptions.push(assumption(
            OPERATOR_POPULATION_SOURCE_ID,
            "unselected_household_templates",
            texts(&unselected_template_ids),
            "These operator templates did not match a generated household size in this seed and were not applied.",
        ));
    }
    if let Some(composition) = household_composition.as_ref().filter(|item| item.source_kind == CompositionKind::Assumption) {
        let first = composition.source_ids.first().ok_or("assumed household composition templates must cite an assumption ID")?;
        assumptions.push(assumption(
            first,
            "synthetic_household_composition_templates",
            map(vec![
                ("applied_template_ids", texts(&selected_ids.iter().collect::<Vec<_>>())),
                ("minor_count", V::Int(people.iter().filter(|(_, traits)| traits.age_years < 18).count() as i64)),
            ]),
            "No household composition was entered, so generic synthetic templates place each generated minor with a \
             linked adult. They describe no real family and are not regional statistics.",
        ));
        for id in &composition.source_ids {
            sources.push(source(
                id,
                "Generic synthetic household composition templates",
                "assumption",
                "Template shapes and weights are a generator heuristic; operator-entered compositions replace them.",
            ));
        }
    }
    sources.push(source(
        SYNTHETIC_HOUSEHOLD_CONSTRUCTION_ID,
        "Synthetic household relationship policy",
        "assumption",
        "Household membership never creates family or guardian links by itself.",
    ));
    assumptions.push(assumption(
        SYNTHETIC_TRAIT_ASSUMPTION_ID,
        "synthetic_person_traits",
        map(vec![
            ("medical_support", text("unknown unless an explicit rate is supplied")),
            (
                "support_need_rule",
                text("explicit medical, medication, or additional access need raises support_need to at least 0.5"),
            ),
            ("mobility_support_from_profile", V::Bool(true)),
            (
                "household_role_rule",
                text(
                    "child below 18; alone for a one-person adult household; otherwise adult or dependent when \
                     explicit support is linked",
                ),
            ),
            (
                "mobility_weights",
                map(vec![
                    ("independent", V::Float(template.independent_weight)),
                    ("assisted", V::Float(template.assisted_weight)),
                    ("wheelchair", V::Float(template.wheelchair_weight)),
                ]),
            ),
        ]),
        "Medical support, information access, and pet status remain unknown unless an explicit input rate or \
         distribution is supplied. Mobility weights and caregiver links are generic generation rules, not local data.",
    ));
    sources.push(source(
        SYNTHETIC_TRAIT_ASSUMPTION_ID,
        "Synthetic person-trait mapping",
        "assumption",
        "Traits are generated from the documented profile template; mobility, medical support, and household \
         relations are not inferred from age or identity.",
    ));
    if operator_composition {
        sources.push(source(
            OPERATOR_POPULATION_SOURCE_ID,
            "Operator-entered population generation conditions",
            "operator",
            "Input rates and categories were entered by the scenario operator and are not regional statistics.",
        ));
    }
    let described: BTreeSet<String> = sources
        .iter()
        .filter_map(|item| match item {
            V::Map(entries) => match entries.get("id") {
                Some(V::Str(id)) => Some(id.to_string()),
                _ => None,
            },
            _ => None,
        })
        .collect();
    let kind = household_composition.as_ref().map_or("operator", |item| item.source_kind.text());
    for id in declared_household_source_ids.difference(&described) {
        sources.push(source(
            id,
            "Operator-declared household composition reference",
            kind,
            "This is a user-supplied source ID declaration. The generator does not verify the citation or its \
             applicability.",
        ));
    }

    let household_template_ids = chosen
        .iter()
        .enumerate()
        .filter_map(|(household, template)| template.map(|item| (household_sizes[household].0.clone(), item.template_id.clone())))
        .collect();
    Ok(GeneratedPopulation {
        profile_set: ProfileSet { profiles: updated_profiles, manifest: profile_set.manifest.clone() },
        people,
        source_ids,
        household_sizes,
        age_bands: bands,
        resident_age_distribution: resident,
        evacuee_age_distribution: evacuee,
        household_mix: mix,
        household_relationships: relationships,
        household_template_ids,
        unselected_template_ids,
        household_composition,
        person_requirements: requirements,
        assumptions,
        sources,
    })
}
