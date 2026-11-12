//! What the views of the update and the propose phases read of a world (Issue #225, lane M7; moved
//! from `tests/integration/test_world_view_row.py`).
//!
//! The world is the generated Scenario the Project screen makes at minute 0, with its Run
//! (`generated_texts.json`'s `generated`), changed as each pytest test changed the Python world; the
//! forms of the view's audit hash are its variants without the information namespace or the life
//! context (`views_*`, `tests/support/core_fixtures_kernels.py`). The views are the kernels the
//! phases' steps call (`support/views.rs`). A view kept or read again is compared with a view of
//! the same world built with nothing kept (`views::fresh`, as `support.world.view_of` made one),
//! field by field (`views::exact`); the values and hashes the pytest file wrote down from the
//! Python rule `WorldView.from_world` (gone since lane L10) are compared as they were.

mod support;

use std::rc::Rc;
use std::sync::Arc;

use indexmap::IndexMap;
use serde_json::Value;
use sheltersim_core::value::V;
use sheltersim_core::world::{World, WorldCell, MODEL_CONFIGS};

use support::load::{self, Loaded};
use support::views::{self, exact};
use support::{cases, TestHost};

fn text(value: &str) -> V {
    V::Str(Arc::from(value))
}

fn map(items: Vec<(&str, V)>) -> V {
    V::Map(items.into_iter().map(|(key, value)| (Arc::from(key), value)).collect::<IndexMap<_, _>>())
}

fn list(items: &[&str]) -> V {
    V::List(items.iter().map(|item| text(item)).collect())
}

/// A world of `generated_texts.json` with its Run and the host of a Run's world.
struct Case {
    loaded: Rc<Loaded>,
    host: TestHost,
}

fn case(name: &str) -> Case {
    let entry: Value = cases::entry("generated_texts", name);
    let loaded = Rc::new(load::loaded(&entry));
    let host = load::following_host(&entry, &loaded);
    Case { loaded, host }
}

impl Case {
    fn edit<T>(&self, edit: impl FnOnce(&mut World) -> T) -> T {
        self.loaded.cell.with(edit).unwrap()
    }

    fn after_movement(&mut self) -> sheltersim_core::rules::world_view::ViewData {
        views::after_movement(&self.loaded, &mut self.host)
    }

    /// The text of a view built with nothing kept, without its hash or with it.
    fn fresh(&mut self, hashed: bool) -> String {
        let (data, hash) = views::fresh(&self.loaded, &mut self.host, hashed);
        exact(&data, hash.as_deref())
    }

    /// The people's IDs in the order of `world.agents`.
    fn agent_ids(&self) -> Vec<Arc<str>> {
        self.edit(|world| world.rows().unwrap().agents.iter().map(|(id, _)| id.clone()).collect())
    }

    /// `world.model_configs[...]` written in place.
    fn configs(&self, edit: impl FnOnce(&mut V)) {
        self.edit(|world| edit(&mut world.row_mut::<sheltersim_core::row::ValueRow>(MODEL_CONFIGS).unwrap().0));
    }
}

fn agent<'a>(data: &'a sheltersim_core::rules::world_view::ViewData, id: &str) -> &'a sheltersim_core::rules::world_view::ViewAgent {
    &data.agents[data.index_of(id).unwrap_or_else(|| panic!("no {id}"))]
}

#[test]
fn a_household_overrides_the_profile_group() {
    let mut case = case("generated");
    let ids = case.agent_ids();
    let mut people = vec![(&*ids[0], map(vec![("household_id", text("H"))])), (&*ids[1], map(vec![("household_id", text("H"))]))];
    people.push((&*ids[2], map(vec![("household_id", V::Null)])));
    let people = map(people);
    case.edit(|world| load::put_namespace(world, "population_presence_v1", map(vec![("people", people)])));
    let made = case.after_movement();
    assert_eq!(agent(&made, &ids[0]).group_id.as_deref(), Some("H"));
    assert_eq!(agent(&made, &ids[2]).group_id, None);
    let mut household: Vec<&str> = vec![&ids[0], &ids[1], &ids[2]];
    household.sort();
    let members: Vec<(&Arc<str>, Vec<usize>)> = made.group_members();
    let (_, of_h) = members.iter().find(|(group, _)| &***group == "H").expect("the household H");
    let named: Vec<&str> = of_h.iter().map(|index| &*made.agents[*index].id).collect();
    assert_eq!(named, household[..2]);
}

#[test]
fn the_proposal_view_reads_again_what_the_update_phase_writes() {
    let mut case = case("generated");
    let prior = case.after_movement();
    views::hand_on(&case.loaded, prior.clone());
    let id = case.agent_ids().remove(0);
    case.edit(|world| {
        let mut rows = world.rows_mut().unwrap();
        let person = rows.agents.get_mut(&id).unwrap();
        person.stress = if person.stress != 0.5 { 0.5 } else { 0.25 };
        person.trust = if person.trust != 0.75 { 0.75 } else { 0.25 };
        person.needs.hunger = if person.needs.hunger != 0.5 { 0.5 } else { 0.25 };
    });
    case.edit(|world| views::rewrite(world, "body_state_v1", |state| views::set(state, &["agents", &id], "uncertainty", V::Float(0.625))));
    let (made, hash) = views::for_proposals(&case.loaded, &mut case.host);
    // The step took the update phase's view out of the Run.
    assert!(views::handed_on(&case.loaded).is_none());
    assert_eq!(exact(&made, Some(&hash)), case.fresh(true));
    assert_ne!(agent(&made, &id).stress, agent(&prior, &id).stress);
}

#[test]
fn a_changed_context_is_seen_at_the_next_view() {
    let mut case = case("generated");
    let roles = |data: &sheltersim_core::rules::world_view::ViewData| -> Vec<String> { agent(data, "A001").person.roles.iter().map(|role| role.to_string()).collect() };
    assert_eq!(roles(&case.after_movement()), ["alone"]);
    let set_roles = |case: &Case, role: &str| case.configs(|configs| views::set(configs, &["population_context", "people", "A001"], "roles", list(&[role])));
    set_roles(&case, "parent");
    assert_eq!(roles(&case.after_movement()), ["parent"]);
    set_roles(&case, "child");
    let third = case.after_movement();
    assert_eq!(roles(&third), ["child"]);
    assert_eq!(exact(&third, None), case.fresh(false));
}

#[test]
fn a_changed_zone_modifier_or_a_new_cache_is_seen_at_the_next_view() {
    let mut case = case("generated");
    let zone: Arc<str> = case.edit(|world| world.rows().unwrap().agents.iter().next().unwrap().1.zone_id.clone());
    case.after_movement();
    let modifier = map(vec![
        ("zone_id", V::Str(zone.clone())),
        ("start_minute", V::Int(0)),
        ("op", text("set_lighting")),
        ("parameters", map(vec![("level", V::Float(0.25))])),
        ("start_event_id", text("e")),
        ("source_id", text("s")),
        ("assumption_ids", V::List(Vec::new())),
    ]);
    case.edit(|world| views::rewrite(world, "zone_environment_v1", |state| views::set(state, &["modifiers"], "m", modifier)));
    let made = case.after_movement();
    assert_eq!(exact(&made, None), case.fresh(false));
    let person = made.agents.iter().find(|person| made.zone_of(person).id == zone).unwrap();
    let in_zone = made.zone_of(person);
    assert_eq!(in_zone.lighting_level, 0.25);
    assert_eq!(in_zone.condition_modifier_event_ids, vec![Arc::<str>::from("e")]);
    // model_configs changed in place: its stamp is new, and the zones are read again.
    case.configs(|configs| views::set(configs, &["zone_environment"], "noise_rest_efficiency_loss", V::Float(0.25)));
    let made = case.after_movement();
    assert_eq!(exact(&made, None), case.fresh(false));
}

#[test]
fn stressors_outages_and_sanitation_are_read_as_python_reads_them() {
    let mut case = case("generated");
    let (facility, facility_zone, zone, facility_ids) = case.edit(|world| {
        let rows = world.rows().unwrap();
        let (facility, row) = rows.facilities.iter().next().unwrap();
        let zone = rows.agents.iter().next().unwrap().1.zone_id.clone();
        let ids: Vec<Arc<str>> = rows.facilities.iter().map(|(id, _)| id.clone()).collect();
        (facility.clone(), row.zone_id.clone(), zone, ids)
    });
    let stressor = |active: bool, zone: &str, stress: f64, start: &str, source: &str| {
        map(vec![
            ("active", V::Bool(active)),
            ("zone_id", text(zone)),
            ("stress_per_minute", V::Float(stress)),
            ("start_event_id", text(start)),
            ("source_id", text(source)),
        ])
    };
    let outage = |active: bool, start: &str| {
        map(vec![("active", V::Bool(active)), ("affected_facility_ids", V::List(vec![V::Str(facility.clone())])), ("start_event_id", text(start))])
    };
    let infrastructure = map(vec![
        (
            "environmental_stressors",
            map(vec![
                ("b", stressor(true, &zone, 0.1, "z", "second")),
                ("a", stressor(true, &zone, 0.2, "y", "first")),
                ("c", stressor(false, &zone, 9.0, "x", "off")),
                ("d", stressor(true, "no such zone", 9.0, "w", "elsewhere")),
            ]),
        ),
        ("outages", map(vec![("o", outage(true, "o2")), ("p", outage(false, "o1"))])),
    ]);
    let water_outage = |start: &str, kind: &str, target: &str| {
        map(vec![("outage_event_id", text(start)), ("active", V::Bool(true)), ("target_kind", text(kind)), ("target_id", text(target))])
    };
    let operational = map(vec![(
        "water_outages",
        map(vec![
            ("by_zone", water_outage("o3", "zone", &facility_zone)),
            ("by_id", water_outage("o0", "facility", &facility)),
            ("other", water_outage("o9", "facility", "another")),
        ]),
    )]);
    case.edit(|world| {
        load::put_namespace(world, "infrastructure_v1", infrastructure);
        load::put_namespace(world, "operational_events_v1", operational);
        world.rows_mut().unwrap().facilities.get_mut(&facility).unwrap().kind = Arc::from("Water_Station");
        views::rewrite(world, "staff_operations_v1", |state| views::set(state, &["zones", &zone], "sanitation", V::Float(0.25)));
    });
    let made = case.after_movement();
    // What the Python rule gave.
    for id in &facility_ids {
        let found = made.facilities.iter().find(|item| item.id == *id).unwrap();
        let wanted: Vec<&str> = if *id == facility { vec!["o0", "o2", "o3"] } else { Vec::new() };
        assert_eq!(found.facility_event_context_ids.iter().map(|item| &**item).collect::<Vec<_>>(), wanted, "{id}");
    }
    for person in &made.agents {
        let in_zone = made.zone_of(person);
        assert_eq!(in_zone.id, zone);
        assert_eq!(in_zone.environmental_stress_per_minute, f64::from_bits(0x3fd3333333333334));
        assert_eq!(in_zone.environmental_stressor_event_ids.iter().map(|item| &**item).collect::<Vec<_>>(), ["y", "z"]);
        assert_eq!(in_zone.environmental_assumption_source_id.as_deref(), Some("first"));
        assert_eq!(in_zone.sanitation_level, 0.25);
    }
}

/// `_HASHES` of the pytest file: the Python rule's audit hashes, by (information, life context).
/// Its forms of `schema_version` 6 are the same worlds (the generated world is v6 with its
/// `resource_service` config), with the same hashes.
const HASHES: [(&str, &str); 4] = [
    ("views_no_information_no_life", "5dd4adee1a9ebcf1f2f194a859e16be0e40c4e854ae5ba07f8cabeddf27e9302"),
    ("views_no_information_life", "0ec91f0ca399ce626a6e54bf11fb93068fcb9fc3c65e15aded1e7ea0dcb26883"),
    ("views_information_no_life", "17d67b8e26a07746c88b42d7f4783e74d195c2c0b09a6643783fb7bce14561e5"),
    ("generated", "e62b2a7a8153dc40eb2f4dbf478018e7aa9d9a84d175eb412cf64cb25d3f4fc4"),
];

#[test]
fn the_view_hash_covers_each_form_of_the_document() {
    for (name, hash) in HASHES {
        let mut case = case(name);
        let (_, made) = views::for_proposals(&case.loaded, &mut case.host);
        assert_eq!(made, hash, "{name}");
    }
}
