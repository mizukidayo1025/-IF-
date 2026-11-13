//! `connect_generated_context` of `experiments/scenario_connections.py` (Issue #298): the
//! `population_context` and `spatial_constraints` model configs of a generated Scenario, from the
//! generated population and the editable floor plan.
//!
//! Python ends with `validate_facility_walkability(geometry, constraints)`, which only raises; the
//! browser builds the constraints from its own generator's plan, whose output Python's tests check
//! (the fixed-point test of the builder, #291), so the check is not repeated here.

use std::collections::{BTreeMap, BTreeSet};

use crate::scenario::population::{GeneratedPopulation, Relationship};
use crate::scenario::profiles::{map, text, texts};
use crate::value::V;

fn sorted_texts(values: impl IntoIterator<Item = String>) -> V {
    let set: BTreeSet<String> = values.into_iter().collect();
    texts(&set.into_iter().collect::<Vec<_>>())
}

/// `PopulationContext.model_dump(mode="json")` of the generated population.
pub fn population_context(population: &GeneratedPopulation) -> V {
    let sources = &population.source_ids;
    let mut households: Vec<(String, Vec<String>)> = Vec::new();
    for profile in &population.profile_set.profiles {
        let group = profile.group_id.clone().unwrap_or_default();
        match households.iter_mut().find(|(id, _)| *id == group) {
            Some((_, members)) => members.push(profile.id.clone()),
            None => households.push((group, vec![profile.id.clone()])),
        }
    }
    let mut parents_by_child: BTreeMap<&str, BTreeSet<String>> = BTreeMap::new();
    let mut partners: BTreeMap<&str, BTreeSet<String>> = BTreeMap::new();
    let mut relatives: BTreeMap<&str, BTreeSet<String>> = BTreeMap::new();
    for relation in &population.household_relationships {
        match relation.relationship {
            Relationship::ParentChild | Relationship::CaregiverChild => {
                parents_by_child.entry(&relation.dependent_id).or_default().insert(relation.caregiver_id.clone());
            }
            Relationship::Partner | Relationship::Relative => {
                let target = if relation.relationship == Relationship::Partner { &mut partners } else { &mut relatives };
                target.entry(&relation.caregiver_id).or_default().insert(relation.dependent_id.clone());
                target.entry(&relation.dependent_id).or_default().insert(relation.caregiver_id.clone());
            }
            Relationship::CaregiverSupport => {}
        }
    }
    let parent_ids: BTreeSet<&String> = parents_by_child.values().flatten().collect();
    let ids = |table: &BTreeMap<&str, BTreeSet<String>>, person: &str| {
        sorted_texts(table.get(person).into_iter().flatten().cloned())
    };
    let mut people = indexmap::IndexMap::new();
    for record in &population.person_requirements {
        let mut roles: BTreeSet<&str> = BTreeSet::new();
        if record.household_role != "adult" {
            roles.insert(record.household_role);
        }
        if parent_ids.contains(&record.person_id) {
            roles.insert("parent");
        }
        if !record.dependent_ids.is_empty() {
            roles.insert("caregiver");
        }
        if record.household_role == "dependent" {
            roles.insert("dependent");
        }
        // No medication support is generated, so the needs are the record's own.
        let care_support_needs = if record.care_support_needs.is_empty() && record.medical_support_required == Some(true) {
            vec!["medical_assessment"]
        } else {
            record.care_support_needs.clone()
        };
        people.insert(
            record.person_id.as_str().into(),
            map(vec![
                ("household_id", text(&record.household_id)),
                ("parent_ids", ids(&parents_by_child, &record.person_id)),
                ("caregiver_ids", texts(&record.caregiver_ids)),
                ("care_for_ids", texts(&record.dependent_ids)),
                ("partner_ids", ids(&partners, &record.person_id)),
                ("relative_ids", ids(&relatives, &record.person_id)),
                ("requires_review", V::Bool(!record.review_reasons.is_empty())),
                ("review_reasons", texts(&record.review_reasons)),
                ("roles", texts(&roles.into_iter().collect::<Vec<_>>())),
                ("medical_support", record.medical_support_required.map_or(V::Null, V::Bool)),
                ("guardian_status", text(record.guardian_status)),
                ("daily_rhythm_id", V::Null),
                ("medication_support", V::Null),
                ("care_support_needs", texts(&care_support_needs)),
                ("pet_count", V::Null),
                ("information_access", V::Null),
                ("usual_wake_minute", V::Null),
                ("usual_sleep_minute", V::Null),
                ("required_sleep_minutes", V::Null),
                ("source_ids", texts(sources)),
            ]),
        );
    }
    map(vec![
        ("schema_version", V::Int(1)),
        ("people", V::Map(people)),
        (
            "households",
            V::Map(
                households
                    .iter()
                    .map(|(id, members)| {
                        (id.as_str().into(), map(vec![("member_ids", texts(members)), ("source_ids", texts(sources))]))
                    })
                    .collect(),
            ),
        ),
        (
            "resident_distribution_source_ids",
            sorted_texts(population.age_bands.iter().flat_map(|band| band.source_ids.iter().cloned())),
        ),
        ("evacuation_selection_source_ids", V::List(Vec::new())),
    ])
}

fn field<'a>(value: &'a V, name: &str) -> Result<&'a V, String> {
    match value {
        V::Map(entries) => entries.get(name).ok_or_else(|| format!("the floor plan has no {name}")),
        _ => Err(format!("the floor plan's {name} is not in an object")),
    }
}

fn list<'a>(value: &'a V, name: &str) -> Result<&'a [V], String> {
    match field(value, name)? {
        V::List(items) => Ok(items),
        _ => Err(format!("the floor plan's {name} is not a list")),
    }
}

fn number(value: &V, name: &str) -> Result<f64, String> {
    match field(value, name)? {
        V::Float(number) => Ok(*number),
        V::Int(number) => Ok(*number as f64),
        _ => Err(format!("the floor plan's {name} is not a number")),
    }
}

fn copy(value: &V, name: &str) -> Result<V, String> {
    field(value, name).cloned()
}

/// `SpatialConstraints.model_dump(mode="json")` of an editable plan (`EditableFacilityGeometry`'s
/// dump) and the Scenario's `FacilityGeometry` dump (its `routes`); the empty constraints without
/// a plan.
pub fn spatial_constraints(editable: Option<&V>, geometry: &V) -> Result<V, String> {
    let empty = || V::List(Vec::new());
    let Some(editable) = editable else {
        return Ok(map(vec![
            ("schema_version", V::Int(1)),
            ("walls", empty()),
            ("openings", empty()),
            ("obstacles", empty()),
            ("equipment_footprints", empty()),
            ("planning_capacity_by_zone", V::Map(Default::default())),
            ("accessible_facility_ids", empty()),
        ]));
    };
    let routes = list(geometry, "routes")?;
    let mut route_ids: BTreeSet<&str> = BTreeSet::new();
    for route in routes {
        if let V::Str(id) = field(route, "edge_id")? {
            route_ids.insert(id);
        }
    }
    let route_openings = list(editable, "route_openings")?;
    let mut openings = Vec::new();
    for record in list(editable, "openings")? {
        let opening_id = field(record, "opening_id")?;
        let mut edges: Vec<V> = Vec::new();
        for usage in route_openings {
            let route_id = field(usage, "route_id")?;
            if field(usage, "opening_id")? == opening_id && matches!(route_id, V::Str(id) if route_ids.contains(&**id)) {
                edges.push(route_id.clone());
            }
        }
        let mut accessible = !edges.is_empty();
        for route in routes {
            if edges.contains(field(route, "edge_id")?) && !matches!(field(route, "accessible")?, V::Bool(true)) {
                accessible = false;
            }
        }
        let (start, end) = (field(record, "start")?, field(record, "end")?);
        let center = map(vec![
            ("x", V::Float((number(start, "x")? + number(end, "x")?) / 2.0)),
            ("y", V::Float((number(start, "y")? + number(end, "y")?) / 2.0)),
        ]);
        openings.push(map(vec![
            ("id", opening_id.clone()),
            ("wall_id", copy(record, "wall_id")?),
            ("center", center),
            ("width_m", copy(record, "width_m")?),
            ("edge_ids", V::List(edges)),
            ("accessible", V::Bool(accessible)),
            ("source_ids", copy(record, "source_ids")?),
        ]));
    }
    let walls = list(editable, "walls")?
        .iter()
        .map(|wall| {
            Ok(map(vec![
                ("id", copy(wall, "wall_id")?),
                ("start", copy(wall, "start")?),
                ("end", copy(wall, "end")?),
                ("source_ids", copy(wall, "source_ids")?),
            ]))
        })
        .collect::<Result<Vec<_>, String>>()?;
    let obstacles = list(editable, "obstacles")?
        .iter()
        .map(|item| {
            Ok(map(vec![
                ("id", copy(item, "obstacle_id")?),
                ("polygon", copy(item, "polygon")?),
                ("source_ids", copy(item, "source_ids")?),
            ]))
        })
        .collect::<Result<Vec<_>, String>>()?;
    let footprints = list(editable, "equipment_footprints")?
        .iter()
        .map(|item| {
            Ok(map(vec![
                ("facility_id", copy(item, "facility_id")?),
                ("polygon", copy(item, "polygon")?),
                ("area_m2", copy(item, "area_m2")?),
                ("source_ids", copy(item, "source_ids")?),
            ]))
        })
        .collect::<Result<Vec<_>, String>>()?;
    Ok(map(vec![
        ("schema_version", V::Int(1)),
        ("walls", V::List(walls)),
        ("openings", V::List(openings)),
        ("obstacles", V::List(obstacles)),
        ("equipment_footprints", V::List(footprints)),
        ("planning_capacity_by_zone", V::Map(Default::default())),
        ("accessible_facility_ids", empty()),
    ]))
}

/// `connect_generated_context(population, editable, geometry)`: the two model configs, in
/// Python's order.
pub fn connect_generated_context(
    population: &GeneratedPopulation,
    editable: Option<&V>,
    geometry: &V,
) -> Result<Vec<(String, V)>, String> {
    Ok(vec![
        ("population_context".to_string(), population_context(population)),
        ("spatial_constraints".to_string(), spatial_constraints(editable, geometry)?),
    ])
}
