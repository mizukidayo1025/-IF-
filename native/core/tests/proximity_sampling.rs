//! The proximity sampling of `DistanceSampledProximityPolicy` and the step that schedules what it
//! finds (`rules::proximity`, the kernels of `sample_of` and `sample_contacts`), on the world of
//! `integration/test_read_rules.py`, without Python (Issue #225, lane M10b).
//!
//! `data/proximity.json` (`tests/support/core_fixtures_reads.py`) holds the generated Scenario of an
//! hour with the distance sampler at minute 0 (`start`), the positions that file drew with
//! `random.Random(7)`, and `stepped`: what the step reads of that world after 5 ticks (the
//! positions, the ID counters and the minute). A test moves the people (the row of `movement_v1`,
//! as the pytest file wrote the tree) and samples. Each answer is `json.dumps` of what the sampler
//! found (`sample_of`'s pairs) or of the step's new events and the ID counters, and the digest of a
//! test's answers is the one the pytest file took from the deleted Python rule (45a9bc9), written
//! again here with `Dumps`. The orders of Python's dicts that the canonical document sorts (the
//! positions, the ID counters) are in the entry (`orders`).

mod support;

use std::collections::HashSet;
use std::sync::Arc;

use indexmap::IndexMap;
use serde_json::Value;
use sheltersim_core::cell::Shard;
use sheltersim_core::checkpoint::sha256_hex;
use sheltersim_core::error::Error;
use sheltersim_core::events::writer::WriteError;
use sheltersim_core::events::EventRow;
use sheltersim_core::row::ValueRow;
use sheltersim_core::rules::kit::Held;
use sheltersim_core::rules::proximity::{self, Placed};
use sheltersim_core::run::held::{Class, Held as Attr, Obj};
use sheltersim_core::run::proximity::Proximity;
use sheltersim_core::state::movement::{MovementRow, Point, Position};
use sheltersim_core::value::V;
use sheltersim_core::world::{World, WorldCell, ID_COUNTERS};

use support::dumps::dumps;
use support::{cases, load, texts};

const FILE: &str = "proximity";
const MOVEMENT: &str = "movement_v1";
const PROXIMITY: &str = "distance-sampled-proximity-v1";

fn text(value: &str) -> V {
    V::Str(Arc::from(value))
}

fn map(entries: &[(&str, V)]) -> V {
    V::Map(entries.iter().map(|(key, value)| (Arc::from(*key), value.clone())).collect())
}

/// A world of the file, in a cell, with its entry and its own `orders` (a variant's are not its
/// base's).
fn world(name: &str) -> (Value, Shard<World>) {
    let data = load::data(FILE);
    let mut entry = cases::resolve(&data["worlds"], name);
    entry["orders"] = data["worlds"][name]["orders"].clone();
    let cell = texts::cell(&entry);
    (entry, cell)
}

/// The people's IDs in the order of Python's dict of positions.
fn position_order(entry: &Value) -> Vec<String> {
    entry["orders"]["positions"].as_array().unwrap().iter().map(|id| id.as_str().unwrap().to_owned()).collect()
}

/// `[record for record in positions.values() if record["point"] is not None]`, as IDs.
fn placed(entry: &Value, cell: &Shard<World>) -> Vec<String> {
    cell.with(|world| {
        let row = world.row::<MovementRow>(MOVEMENT).unwrap();
        position_order(entry).into_iter().filter(|id| row.positions.get(id.as_str()).unwrap().point.is_some()).collect()
    })
    .unwrap()
}

/// Change the positions of the row, as the pytest files wrote the tree.
fn positions(cell: &Shard<World>, change: impl FnOnce(&mut IndexMap<Arc<str>, Position, rustc_hash::FxBuildHasher>)) {
    cell.with(|world| change(&mut world.row_mut::<MovementRow>(MOVEMENT).unwrap().positions)).unwrap();
}

fn sampler(radius: V, limit: i64) -> V {
    map(&[("max_distance_m", radius), ("max_pairs_per_sample", V::Int(limit))])
}

fn point_value(point: &Placed) -> V {
    map(&[("x", V::Float(point.x)), ("y", V::Float(point.y))])
}

/// `sample_of`: the pairs `(distance, first, second, zone, first point, second point)`, and the
/// answer, `json.dumps` of them, added to `answers`.
fn sample(cell: &Shard<World>, config: &V, answers: &mut Vec<String>) -> Vec<V> {
    let (points, pairs) = cell.with(|world| proximity::sample(world, config, &Held::Held)).unwrap().unwrap();
    let pairs: Vec<V> = pairs
        .into_iter()
        .map(|(distance, first, second)| {
            let (first, second) = (&points[first], &points[second]);
            V::List(vec![
                V::Float(distance),
                V::Str(first.id.clone()),
                V::Str(second.id.clone()),
                V::Str(first.zone.clone()),
                point_value(first),
                point_value(second),
            ])
        })
        .collect();
    answers.push(dumps(&V::List(pairs.clone())));
    pairs
}

/// The answers, joined by new lines, are what the Python rule gave: the first 16 hex of their sha256.
fn is_python_s(answers: &[String], digest: &str) {
    assert_eq!(&sha256_hex(answers.join("\n").as_bytes())[..16], digest);
}

#[test]
fn the_pairs_of_a_sample_are_the_python_rules_from_the_row() {
    let radii = [
        (V::Float(0.25), "e0a1057cd2ac16bd"),
        (V::Float(1.5), "37315c7ce0bf997c"),
        (V::Int(2), "e2a86b4db6b2805e"),
        (V::Float(10.0), "9a06c87007643049"),
    ];
    for (radius, digest) in radii {
        let (entry, cell) = world("start");
        let zones: Vec<Arc<str>> = cell
            .with(|world| {
                let mut zones: Vec<Arc<str>> = world.rows().unwrap().zones.iter().map(|(id, _)| id.clone()).collect();
                zones.sort_unstable();
                zones
            })
            .unwrap();
        let scale = match radius {
            V::Float(number) => number,
            V::Int(number) => number as f64,
            _ => unreachable!(),
        };
        let people = placed(&entry, &cell);
        let draws = entry["draws"].as_array().unwrap();
        assert_eq!(draws.len(), 60);
        let mut answers = Vec::new();
        let mut found = 0;
        for (index, drawn) in draws.iter().enumerate() {
            let limit = [1, 5, 10_000][index / 20];
            let drawn: Vec<&str> = drawn.as_str().unwrap().split(' ').collect();
            assert_eq!(drawn.len(), people.len());
            positions(&cell, |positions| {
                for (id, values) in people.iter().zip(&drawn) {
                    let parts: Vec<&str> = values.split(',').collect();
                    let record = positions.get_mut(id.as_str()).unwrap();
                    // `float(rng.randint(-8, 8)) * radius / 4`.
                    let x = parts[0].parse::<i64>().unwrap() as f64 * scale / 4.0;
                    record.point = Some(Point { x, y: parts[1].parse().unwrap() });
                    record.zone_id = zones[parts[2].parse::<usize>().unwrap()].clone();
                }
            });
            found += sample(&cell, &sampler(radius.clone(), limit), &mut answers).len();
        }
        assert!(found > 100);
        is_python_s(&answers, digest);
    }
}

#[test]
fn a_pair_at_nearly_the_radius_across_two_cells_is_found() {
    // The size of a cell decides who is looked at, so a smaller one would lose this pair.
    let (entry, cell) = world("start");
    let people = placed(&entry, &cell);
    let radius = 1.5;
    positions(&cell, |positions| {
        let zone = positions.get(people[0].as_str()).unwrap().zone_id.clone();
        for (number, id) in people.iter().enumerate() {
            let record = positions.get_mut(id.as_str()).unwrap();
            record.zone_id = zone.clone();
            record.point = Some(Point { x: 1000.0 * number as f64, y: 1000.0 * number as f64 });
        }
        positions.get_mut(people[0].as_str()).unwrap().point = Some(Point { x: 0.985 * radius, y: 0.0 });
        positions.get_mut(people[1].as_str()).unwrap().point = Some(Point { x: 1.984 * radius, y: 0.0 });
    });
    let mut answers = Vec::new();
    let pairs = sample(&cell, &sampler(V::Float(radius), 10), &mut answers);
    assert_eq!(pairs.len(), 1);
    is_python_s(&answers, "c94a22af8cb63695");
}

/// Everyone placed at (1, 1) in the zone of the first placed person.
fn together(entry: &Value, cell: &Shard<World>) -> Vec<String> {
    let people = placed(entry, cell);
    assert!(people.len() >= 3);
    positions(cell, |positions| {
        let zone = positions.get(people[0].as_str()).unwrap().zone_id.clone();
        for id in &people {
            let record = positions.get_mut(id.as_str()).unwrap();
            record.point = Some(Point { x: 1.0, y: 1.0 });
            record.zone_id = zone.clone();
        }
    });
    people
}

#[test]
fn what_the_sampler_skips_it_skips_from_the_row() {
    let changes: [fn(&mut Position); 5] = [
        |record| record.point.as_mut().unwrap().x = f64::NAN,
        |record| record.point.as_mut().unwrap().y = f64::INFINITY,
        |record| record.point = None,
        |record| record.position_kind = Arc::from("edge"),
        |record| record.zone_id = Arc::from("nowhere"),
    ];
    for change in changes {
        let (entry, cell) = world("start");
        let people = together(&entry, &cell);
        let config = sampler(V::Float(1.5), 10_000);
        let mut answers = Vec::new();
        let before = sample(&cell, &config, &mut answers);
        positions(&cell, |positions| change(positions.get_mut(people[0].as_str()).unwrap()));
        let after = sample(&cell, &config, &mut answers);
        assert!(!before.is_empty());
        assert!(after.len() <= before.len());
        is_python_s(&answers, "1e8c8a88054f414b");
    }
}

#[test]
fn a_person_who_is_gone_is_skipped() {
    let (entry, cell) = world("start");
    let people = placed(&entry, &cell);
    positions(&cell, |positions| {
        let first = positions.get(people[0].as_str()).unwrap().clone();
        positions.insert(Arc::from("nobody"), first);
    });
    let mut answers = Vec::new();
    let pairs = sample(&cell, &sampler(V::Float(10.0), 10_000), &mut answers);
    assert!(pairs.iter().all(|pair| {
        let V::List(items) = pair else { unreachable!() };
        items[1..3].iter().all(|id| *id != text("nobody"))
    }));
    is_python_s(&answers, "c5660ccef72b95d6");
}

// ---- The step -----------------------------------------------------------------------------------

/// The config of the pytest file's stand-in for the controller (`_stand_in`).
fn contact_config() -> V {
    map(&[
        ("schema_version", V::Int(1)),
        ("enabled", V::Bool(true)),
        ("policy_version", text(PROXIMITY)),
        ("sample_interval_minutes", V::Int(15)),
        ("max_distance_m", V::Float(1.5)),
        ("max_pairs_per_sample", V::Int(500)),
    ])
}

/// What the Run reads of a controller whose config is this, and whose policy is
/// `DistanceSampledProximityPolicy(config)`.
fn stand_in(config: V) -> Proximity {
    let mut policy = Obj::new(Class::named("sheltersim.policies.proximity", "DistanceSampledProximityPolicy"));
    policy.attrs.insert(Arc::from("config"), Ok(Attr::Value(config.clone())));
    Proximity { config: Ok(config), known: Ok(true), policy: Ok(Some(Arc::new(policy))) }
}

/// The step's entry (`sample_contacts`): nothing off its interval, else the core's step.
fn step(cell: &Shard<World>, proximity: &Proximity, minute: i64) -> Result<(), WriteError> {
    match proximity::when(proximity, Some(minute))? {
        Some(minute) => cell.with(|world| proximity::sample_contacts(world, proximity, minute, &Held::Held)).unwrap(),
        None => Ok(()),
    }
}

fn pending_rows(cell: &Shard<World>) -> Vec<EventRow> {
    cell.with(|world| {
        let queue = world.queue().unwrap();
        queue.ordered().iter().map(|(_, serial)| EventRow::clone(queue.slot(*serial).row.as_ref().expect("a row"))).collect()
    })
    .unwrap()
}

/// `ScheduledEvent.to_dict()`, in its order of keys.
fn to_dict(row: &EventRow) -> V {
    let optional = |value: &Option<Arc<str>>| value.as_ref().map_or(V::Null, |id| V::Str(id.clone()));
    map(&[
        ("event_id", V::Str(row.event_id.clone())),
        ("kind", text(row.kind.as_str())),
        ("due_minute", V::Int(row.due_minute)),
        ("priority", V::Int(row.priority)),
        ("sequence", V::Int(row.sequence)),
        ("event_type", V::Str(row.event_type.clone())),
        ("payload", row.payload.clone()),
        ("source_id", optional(&row.source_id)),
        ("causation_id", optional(&row.causation_id)),
        ("provenance", row.provenance.clone()),
    ])
}

/// `world.id_counters` in the order of Python's dict.
fn counters(entry: &Value, cell: &Shard<World>) -> V {
    let V::Map(held) = cell.with(|world| world.row::<ValueRow>(ID_COUNTERS).unwrap().0.clone()).unwrap() else { panic!("not a map") };
    let order = entry["orders"]["id_counters"].as_array().unwrap();
    assert_eq!(order.len(), held.len());
    V::Map(order.iter().map(|name| (Arc::from(name.as_str().unwrap()), held.get(name.as_str().unwrap()).unwrap().clone())).collect())
}

/// The pytest file's `_run_step`: the step's new events, by sequence, each `to_dict()`; the
/// answer, `json.dumps([events, world.id_counters])`, added to `answers`.
fn run_step(entry: &Value, cell: &Shard<World>, minute: i64, answers: &mut Vec<String>, config: Option<V>) -> Vec<V> {
    let seen: HashSet<Arc<str>> = pending_rows(cell).into_iter().map(|row| row.event_id).collect();
    step(cell, &stand_in(config.unwrap_or_else(contact_config)), minute).unwrap_or_else(|error| panic!("{error:?}"));
    let mut events: Vec<EventRow> = pending_rows(cell).into_iter().filter(|row| !seen.contains(&row.event_id)).collect();
    events.sort_by_key(|row| row.sequence);
    let events: Vec<V> = events.iter().map(to_dict).collect();
    answers.push(dumps(&V::List(vec![V::List(events.clone()), counters(entry, cell)])));
    events
}

fn field<'a>(event: &'a V, path: &[&str]) -> &'a V {
    path.iter().fold(event, |value, key| match value {
        V::Map(map) => map.get(*key).unwrap_or_else(|| panic!("no {key}")),
        other => panic!("not a map: {other:?}"),
    })
}

#[test]
fn the_step_schedules_the_events_of_the_python_rule() {
    let (entry, cell) = world("stepped");
    let order = position_order(&entry);
    positions(&cell, |positions| {
        let zone = order.iter().find_map(|id| positions.get(id.as_str()).unwrap().point.as_ref().map(|_| positions.get(id.as_str()).unwrap().zone_id.clone())).unwrap();
        for (number, id) in order.iter().enumerate() {
            let record = positions.get_mut(id.as_str()).unwrap();
            if record.point.is_some() {
                record.zone_id = zone.clone();
                record.point = Some(Point { x: 0.125 * number as f64, y: 0.0 });
            }
        }
    });
    let mut answers = Vec::new();
    let events = run_step(&entry, &cell, 30, &mut answers, None);
    is_python_s(&answers, "079b3c75a9d8ef83");
    assert!(events.len() > 3);
    let first = &events[0];
    assert_eq!(field(first, &["event_type"]), &text("proximity_contact_update"));
    assert_eq!((field(first, &["priority"]), field(first, &["kind"])), (&V::Int(40), &text("endogenous")));
    assert_eq!(field(first, &["source_id"]), &text("ASM-PROXIMITY-CONTACT-V1"));
    assert_eq!(field(first, &["payload", "contact_minutes"]), &V::Int(0));
    assert_eq!(field(first, &["provenance", "observed_minute"]), &V::Int(31));
}

#[test]
fn a_minute_off_the_interval_or_at_the_end_schedules_nothing() {
    let (entry, cell) = world("start");
    let duration = cell.with(|world| world.duration_minutes().unwrap()).unwrap();
    let mut answers = Vec::new();
    assert!(run_step(&entry, &cell, 7, &mut answers, None).is_empty());
    assert!(run_step(&entry, &cell, duration - 1, &mut answers, None).is_empty());
    let every = |interval: i64| {
        map(&[
            ("schema_version", V::Int(1)),
            ("enabled", V::Bool(true)),
            ("policy_version", text(PROXIMITY)),
            ("sample_interval_minutes", V::Int(interval)),
            ("max_distance_m", V::Float(10.0)),
            ("max_pairs_per_sample", V::Int(2)),
        ])
    };
    assert!(run_step(&entry, &cell, duration - 1, &mut answers, Some(every(1))).is_empty());
    assert_eq!(run_step(&entry, &cell, 7, &mut answers, Some(every(7))).len(), 2);
    is_python_s(&answers, "1422d1ce681fb0d7");
}

#[test]
fn a_coordinate_too_large_for_a_cell_is_refused() {
    // No floor plan reaches such a coordinate (the Python rule gave an OverflowError): the sampler
    // and the step refuse, and the step schedules nothing.
    let (entry, cell) = world("start");
    let people = placed(&entry, &cell);
    positions(&cell, |positions| positions.get_mut(people[0].as_str()).unwrap().point.as_mut().unwrap().x = 1e300);
    let refusal = Error::Unsupported("a coordinate's cell does not fit an i64".into());
    let sampled = cell.with(|world| proximity::sample(world, &sampler(V::Float(1.5), 10), &Held::Held).map(|_| ())).unwrap();
    assert_eq!(sampled, Err(refusal.clone()));
    let pending = pending_rows(&cell);
    let stepped = step(&cell, &stand_in(contact_config()), 0);
    assert_eq!(format!("{stepped:?}"), format!("{:?}", Err::<(), _>(WriteError::from(refusal))));
    assert_eq!(pending_rows(&cell), pending);
}
