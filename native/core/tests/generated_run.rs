//! The columns of `MetricSnapshot` and the entity rules on the worlds of a Run (Issue #225, lane
//! M7; moved from `tests/integration/test_entity_rules_kernel.py`).
//!
//! `rules::metrics::sample` is the kernel `metrics._snapshot_fields` calls through the PyO3 crate
//! (`snapshot_fields`), and the tick's metrics phase takes it the same way (`tick::metrics`). The
//! pytest file took the worlds every 20 minutes of a Run of the generated Scenario the Project
//! screen makes (`run_scenario`); here the core runs the same world's ticks
//! (`generated_texts.json`'s `generated`, `support/ticks.rs`) and samples it at the same minutes,
//! with the metrics phase on the world after the tick, as `_snapshot_fields(world, unsettled,
//! world.minute_index)` did. The columns of the Python rule (`kernel_only` since lane L10) were
//! recorded once from it as a digest of `json.dumps` of the columns of every world: `Dumps` writes
//! the same text, so that digest stays. The other cases change a world of the Run (the first, at
//! minute 20) as the pytest file did and read the columns again; the last reads which zones the
//! entity rules (`tick_validation::entity_rules`, the kernel of `_entity_rules_kernel`) leave to the
//! check of a zone's capacity.

mod support;

use std::rc::Rc;
use std::sync::Arc;

use indexmap::IndexMap;
use serde_json::Value;
use sheltersim_core::config::movement::RouteCapacities;
use sheltersim_core::config::SectionId;
use sheltersim_core::entities::RequestStatus;
use sheltersim_core::rules::kit;
use sheltersim_core::rules::metrics::MetricSample;
use sheltersim_core::rules::tick::metrics as metrics_phase;
use sheltersim_core::rules::tick_validation::{self, EntityInputs, Requests, LEDGER, OVERLAY, SERVICE, SPACES};
use sheltersim_core::value::V;
use sheltersim_core::world::{World, WorldCell};

use support::dumps::dumps;
use support::load::{self, Loaded};
use support::texts::sha;
use support::ticks::{self, tick_at};
use support::{cases, Call, TestHost};

/// `SNAPSHOTS_OF_THE_RUN` of the pytest file: the first 16 hex of the sha256 of `json.dumps` of the
/// columns of the worlds at minutes 20, 40, ... 360, recorded from the Python rule.
const SNAPSHOTS_OF_THE_RUN: &str = "b486e7a17d3d3943";

fn generated() -> Value {
    cases::entry("generated_texts", "generated")
}

/// The metrics phase on the world at its minute: the sample it gave the host.
fn sample(loaded: &Loaded, host: &mut TestHost) -> MetricSample {
    let minute = loaded.cell.with(|world| world.minute_index().unwrap()).unwrap();
    let before = host.calls.len();
    if let Err(failed) = metrics_phase::metrics(&mut tick_at(loaded, host, minute)) {
        panic!("minute {minute}: {:?} {:?}", failed.label, failed.error);
    }
    let taken: Vec<&MetricSample> = host.calls[before..]
        .iter()
        .filter_map(|call| match call {
            Call::Metric(sample, at) if *at == minute => Some(sample),
            _ => None,
        })
        .collect();
    assert_eq!(taken.len(), 1, "minute {minute}");
    taken[0].clone()
}

/// `json.loads(json.dumps(_snapshot_fields(...)))`: the columns, the minute first.
fn fields(sample: &MetricSample, minute: i64) -> V {
    let int = |value: usize| V::Int(value as i64);
    V::List(vec![
        V::Int(minute),
        int(sample.toilet_queue_length),
        int(sample.toilet_active_users),
        V::Int(sample.toilet_working_units),
        V::Float(sample.toilet_wait_count),
        V::Float(sample.toilet_wait_total_minutes),
        V::Float(sample.toilet_wait_mean_minutes),
        V::Float(sample.toilet_wait_max_minutes),
        V::Float(sample.stress_mean),
        int(sample.stress_high_count),
        int(sample.staff_assigned_task_count),
        int(sample.staff_busy_count),
        V::Float(sample.staff_load_ratio),
        int(sample.unresolved_support_request_count),
        V::Str(Arc::from(sample.source_ids_json.as_str())),
        int(sample.support_no_qualified_worker_count),
        V::Float(sample.support_no_qualified_worker_request_minutes),
        V::Float(sample.support_fairness_promoted_assignment_count),
        V::Float(sample.support_wait_staff_busy_request_minutes),
        V::Float(sample.support_wait_staff_off_shift_request_minutes),
    ])
}

/// The generated world's ticks in the core, 20 at a time, with `at` called on the world after each
/// 20 (the pytest file's `documents`, every 20 minutes), until it says to stop or the Run ends.
fn documents(mut at: impl FnMut(&Loaded, &mut TestHost) -> bool) {
    let entry = generated();
    let loaded = Rc::new(load::loaded(&entry));
    let mut host = load::following_host(&entry, &loaded);
    let duration = loaded.cell.with(|world| world.duration_minutes().unwrap()).unwrap();
    for _ in 0..duration / 20 {
        ticks::run(&loaded, &mut host, 20, &mut |_, _| {});
        if !at(&loaded, &mut host) {
            return;
        }
    }
}

#[test]
fn the_snapshot_is_the_python_rules_on_the_worlds_of_a_run() {
    let mut columns = Vec::new();
    documents(|loaded, host| {
        let minute = loaded.cell.with(|world| world.minute_index().unwrap()).unwrap();
        columns.push(fields(&sample(loaded, host), minute));
        true
    });
    assert_eq!(columns.len(), 18);
    assert_eq!(&sha(&dumps(&V::List(columns)))[..16], SNAPSHOTS_OF_THE_RUN);
}


/// The first world of the Run's (minute 20), loaded with its host.
fn first_world() -> (Rc<Loaded>, TestHost) {
    let entry = generated();
    let loaded = Rc::new(load::loaded(&entry));
    let mut host = load::following_host(&entry, &loaded);
    ticks::run(&loaded, &mut host, 20, &mut |_, _| {});
    (loaded, host)
}

fn edit<T>(loaded: &Loaded, f: impl FnOnce(&mut World) -> T) -> T {
    loaded.cell.with(f).unwrap()
}

/// The snapshot's IDs (`json.loads(fields[14])`).
fn source_ids(sample: &MetricSample) -> Value {
    serde_json::from_str(&sample.source_ids_json).unwrap()
}

#[test]
fn the_stress_threshold_is_where_the_python_rule_has_it() {
    let (loaded, mut host) = first_world();
    let ids: Vec<Arc<str>> = edit(&loaded, |world| {
        let mut rows = world.rows_mut().unwrap();
        let ids: Vec<Arc<str>> = rows.agents.iter().map(|(id, _)| id.clone()).take(3).collect();
        for (id, stress) in ids.iter().zip([0.7, 0.6999999999999999, 0.707]) {
            rows.agents.get_mut(id).unwrap().stress = stress;
        }
        ids
    });
    // The Python rule: `agent.stress >= 0.7`.
    let mut high: Vec<String> =
        edit(&loaded, |world| world.rows().unwrap().agents.iter().filter(|(_, agent)| agent.stress >= 0.7).map(|(id, _)| id.to_string()).collect());
    high.sort();
    assert!(high.contains(&ids[0].to_string()) && !high.contains(&ids[1].to_string()) && high.contains(&ids[2].to_string()));
    let taken = sample(&loaded, &mut host);
    assert_eq!(taken.stress_high_count, high.len());
    assert_eq!(source_ids(&taken)["high_stress_agents"], serde_json::json!(high));
}

#[test]
fn a_person_with_two_open_requests_is_the_last_in_the_snapshot() {
    // The Python rule's dict by agent: the later of two requests of one person, in the order of the
    // unsettled requests, is the one kept; the list of who waits names the person twice.
    let mut found = false;
    documents(|loaded, host| {
        let active: Vec<(Arc<str>, Arc<str>)> = edit(loaded, |world| {
            let rows = world.rows().unwrap();
            rows.support_requests.iter().filter(|(_, request)| request.status == RequestStatus::Pending).map(|(id, request)| (id.clone(), request.agent_id.clone())).collect()
        });
        if active.len() < 2 {
            return true;
        }
        let person = active[0].1.clone();
        edit(loaded, |world| world.rows_mut().unwrap().support_requests.get_mut(&active[1].0).unwrap().agent_id = person.clone());
        let ids = source_ids(&sample(loaded, host));
        assert_eq!(ids["support_wait_by_agent"][&*person]["request_id"], *active[1].0);
        let waiting = ids["unresolved_support_agents"].as_array().unwrap();
        assert_eq!(waiting.iter().filter(|agent| **agent == *person).count(), 2);
        found = true;
        false
    });
    assert!(found, "no document had two pending requests");
}

/// The columns of the first world with the facilities' kinds set, in their order.
fn columns_with_kinds(kinds: &[&str]) -> MetricSample {
    let (loaded, mut host) = first_world();
    edit(&loaded, |world| {
        let mut rows = world.rows_mut().unwrap();
        let ids: Vec<Arc<str>> = rows.facilities.iter().map(|(id, _)| id.clone()).collect();
        for (id, kind) in ids.iter().zip(kinds) {
            rows.facilities.get_mut(id).unwrap().kind = Arc::from(*kind);
        }
    });
    sample(&loaded, &mut host)
}

#[test]
fn a_kind_that_is_not_ascii_is_folded_as_python_folds_it() {
    for (kind, like) in [
        // `"toilet" in kind.casefold()`: ß folds to ss and the rest to lower case.
        ("Straße-TOILET", "toilet"),
        // İ folds to i and a combining dot, so "toilet" is not in it.
        ("TOİLET", "storage"),
        // ﬆ folds to "st", which no lower casing gives (the core's full fold, P8-0a).
        ("ﬆoilet", "toilet"),
    ] {
        assert_eq!(columns_with_kinds(&[kind]), columns_with_kinds(&[like]), "{kind}");
    }
}

#[test]
fn kinds_that_are_not_ascii_are_folded_once_each() {
    // Every kind that isn't ASCII is folded as Python folds it (the core's table since P8-0a).
    let kinds = ["Ⓣoilet", "ﬆoilet", "TOİLET"];
    let likes = ["storage", "toilet", "storage"];
    assert_eq!(columns_with_kinds(&kinds), columns_with_kinds(&likes));
}

/// What the entity rules leave to the check of a zone's capacity: each zone and its occupancy.
fn left(loaded: &Loaded, host: &mut TestHost) -> Vec<(Arc<str>, i64)> {
    let cell = &loaded.cell;
    let holding = kit::holding(cell, host).unwrap();
    let plan = kit::kept_of(kit::section(cell, host, &holding, SectionId::TickPlan, |configs| &configs.tick_plan).unwrap());
    let plan = kit::kept(&plan).unwrap().clone();
    let route_capacities =
        kit::kept_section_value(cell, host, &holding, SectionId::RouteCapacities, |configs| &configs.route_capacities, RouteCapacities::parse).unwrap();
    let held = |name: &str| holding.held(name, "the world holds another tree than the namespace's");
    let (ledger, service, spaces, overlay) = (held(LEDGER), held(SERVICE), held(SPACES), held(OVERLAY));
    let inputs = EntityInputs {
        requests: Requests::All,
        resource_config: plan.resource_config.as_ref(),
        route_capacities: &route_capacities,
        ledger: &ledger,
        service: &service,
        spaces: &spaces,
        overlay: &overlay,
    };
    cell.with(|world| tick_validation::entity_rules(world, &inputs, &mut |_| {})).unwrap().unwrap()
}

#[test]
fn a_zone_with_temporary_space_is_left_to_the_python_rule() {
    let (loaded, mut host) = first_world();
    left(&loaded, &mut host);
    let zone = edit(&loaded, |world| world.rows().unwrap().zones.iter().next().unwrap().0.clone());
    edit(&loaded, |world| {
        let mut state = match world.slots.try_value(SPACES).unwrap().as_deref() {
            Some(V::Map(state)) => state.clone(),
            other => panic!("the spaces' state: {other:?}"),
        };
        let mut spaces = match state.get("spaces") {
            Some(V::Map(spaces)) => spaces.clone(),
            other => panic!("the spaces: {other:?}"),
        };
        spaces.insert(Arc::from("room"), V::Map(IndexMap::from([(Arc::from("zone_id"), V::Str(zone.clone()))])));
        state.insert(Arc::from("spaces"), V::Map(spaces));
        load::put_namespace(world, SPACES, V::Map(state));
    });
    let left = left(&loaded, &mut host);
    let (with_capacity, occupancy) = edit(&loaded, |world| {
        let rows = world.rows().unwrap();
        let with_capacity: Vec<Arc<str>> = rows.zones.iter().filter(|(_, zone)| zone.capacity.is_some()).map(|(id, _)| id.clone()).collect();
        let mut occupancy: IndexMap<Arc<str>, i64> = rows.zones.iter().map(|(id, _)| (id.clone(), 0)).collect();
        for (_, agent) in rows.agents.iter() {
            *occupancy.get_mut(&agent.zone_id).unwrap() += 1;
        }
        (with_capacity, occupancy)
    });
    let mut zones: Vec<&Arc<str>> = left.iter().map(|(zone, _)| zone).collect();
    zones.sort();
    let mut wanted: Vec<&Arc<str>> = with_capacity.iter().collect();
    wanted.sort();
    assert_eq!(zones, wanted);
    assert!(left.iter().all(|(zone, count)| occupancy[zone] == *count));
}
