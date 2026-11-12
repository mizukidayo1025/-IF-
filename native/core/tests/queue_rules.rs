//! The queue rules of a facility on the generated Scenario's world (Issue #225, lane M3): the tests of
//! `tests/integration/test_queue_rules.py` that check the core's rules, moved here.
//!
//! The world is the generated Scenario (the Project screen's, 20 people) after two ticks through
//! `TickEngine`, read from `tests/data/queue_rules.json` (`tests/support/core_fixtures_queues.py`
//! writes it with what the pytest read in Python: the facility of each kind, the people with nothing
//! under way, the middle of each zone, the strict FIFO Scenario's configs and selection, and the
//! points and speeds the walking tests computed). A test lines people up as the pytest's `_line_up`
//! does, edits the world as the pytest did, and calls the rule as the driver's steps call it
//! (`Tick::prepare_queue` and `Tick::progress`, `rules/tick/arbitrate.rs`): the configs' queue policy
//! and movement model as the core parses them, the selection and the floor plan from the export.
//! Values next to a coefficient or a limit are the pytest's, so a change of 1% shows.
//!
//! The pytest chained what `progress_physical_queue_positions` returned and left into a digest of
//! Python's `repr` (`ANSWERS`). Those digests are recorded again here from the core (decision 1 of
//! #225): `Answers` chains each call's result and the whole world after it (`support/answers.rs`).

mod support;

use std::cell::RefCell;
use std::sync::{Arc, OnceLock};

use serde_json::{json, Value};
use sheltersim_core::cell::Shard;
use sheltersim_core::entities::{Activity, TaskRow};
use sheltersim_core::error::Error;
use sheltersim_core::events::EventRow;
use sheltersim_core::json::Encoder;
use sheltersim_core::row::{Field, StrEnum, ValueRow};
use sheltersim_core::rules::tick::Tick;
use sheltersim_core::run::Run;
use sheltersim_core::state::movement::{MovementRow, Point};
use sheltersim_core::state::resource_service::{ServiceRow, Stall};
use sheltersim_core::value::V;
use sheltersim_core::world::{Fit, World, WorldCell, MODEL_CONFIGS};

use support::dumps::dumps;
use support::load::{self, Loaded};
use support::answers::{world_bytes, Answers};
use support::TestHost;

const FILE: &str = "queue_rules";
const WORLD: &str = "generated_minute_2";
const MOVEMENT: &str = "movement_v1";
const SERVICE: &str = "resource_service_v1";
const ABANDONED: &str = "facility_queue_abandon_count";
const BLOCKED: &str = "physical_queue_route_blocked_count";
const POSITIONS: bool = false;
const HEAD: bool = true;

fn text(value: &str) -> Arc<str> {
    Arc::from(value)
}

fn entry() -> Value {
    static ENTRY: OnceLock<Value> = OnceLock::new();
    ENTRY.get_or_init(|| load::entry(FILE, WORLD)).clone()
}

/// A JSON value as the core's `V`.
fn v(value: Value) -> V {
    load::v(&value)
}

/// A `V` as JSON, through Python's `json.dumps` text.
fn json_of(value: &V) -> Value {
    serde_json::from_str(&dumps(value)).unwrap()
}

fn field_json<T: Field>(value: &T) -> String {
    let mut encoder = Encoder::with_capacity(256);
    value.write_json(&mut encoder).unwrap();
    String::from_utf8(encoder.out).unwrap()
}

/// The world of the fixture, its Run, and a host with its sections and floor plan.
struct Case {
    entry: Value,
    loaded: Loaded,
    host: TestHost,
}

impl Case {
    fn new() -> Self {
        Self::of(entry())
    }

    fn of(entry: Value) -> Self {
        let loaded = load::loaded(&entry);
        let host = load::host(&entry);
        Self { entry, loaded, host }
    }

    fn with<T>(&self, f: impl FnOnce(&mut World) -> T) -> T {
        self.loaded.cell.with(f).unwrap()
    }

    fn bytes(&self) -> Vec<u8> {
        self.with(|world| world_bytes(world))
    }

    /// `_facility`: the first facility of the kind with a point in the floor plan.
    fn facility(&self, kind: &str) -> String {
        self.entry["facilities"][kind].as_str().unwrap().to_owned()
    }

    /// `_people`: agents with nothing under way.
    fn people(&self, count: usize) -> Vec<String> {
        self.entry["people"].as_array().unwrap().iter().take(count).map(|id| id.as_str().unwrap().to_owned()).collect()
    }

    fn center(&self, zone: &str) -> (f64, f64) {
        let point = &self.entry["zone_centers"][zone];
        (point[0].as_f64().unwrap(), point[1].as_f64().unwrap())
    }

    /// `_line_up`: these people in this facility's queue, in the zone's middle, as `request` leaves
    /// them.
    fn line_up(&self, facility: &str, people: &[String], resource: bool, starts: Option<&[i64]>) {
        let zone = self.with(|world| world.rows().unwrap().facilities.get(facility).unwrap().zone_id.clone());
        let (x, y) = self.center(&zone);
        self.with(|world| {
            {
                let mut rows = world.rows_mut().unwrap();
                for (place, id) in people.iter().enumerate() {
                    let agent = rows.agents.get_mut(id).unwrap();
                    agent.zone_id = zone.clone();
                    agent.activity = Activity::Queued;
                    agent.target_id = Some(text(facility));
                    let row = rows.facilities.get_mut(facility).unwrap();
                    let queue = if resource { &mut row.resource_queue_agent_ids } else { &mut row.queue_agent_ids };
                    queue.push(text(id));
                    let task = format!("queue-task-{id}");
                    let start = starts.map_or(0, |starts| starts[place]);
                    rows.active_tasks.insert(
                        text(&task),
                        TaskRow {
                            id: text(&task),
                            kind: text(if resource { "resource_service_queue" } else { "toilet_queue" }),
                            agent_id: Some(text(id)),
                            resource_id: Some(text(facility)),
                            start_minute: start,
                            due_minute: start,
                            staff_id: None,
                        },
                    );
                }
            }
            let mut movement = world.row_mut::<MovementRow>(MOVEMENT).unwrap();
            for id in people {
                let position = movement.positions.get_mut(id.as_str()).unwrap();
                position.zone_id = zone.clone();
                position.point = Some(Point { x, y });
                position.edge_id = None;
                position.progress = 0.0;
                position.position_kind = text("physical");
            }
        });
    }

    /// `_away_from_the_middle`: someone in their zone but not at its middle.
    fn away_from_the_middle(&self, id: &str, distance: f64) {
        let zone = self.with(|world| world.rows().unwrap().agents.get(id).unwrap().zone_id.clone());
        let (x, y) = self.center(&zone);
        self.put_point(id, Some((x + distance, y)));
    }

    fn put_point(&self, id: &str, point: Option<(f64, f64)>) {
        self.with(|world| {
            let mut movement = world.row_mut::<MovementRow>(MOVEMENT).unwrap();
            movement.positions.get_mut(id).unwrap().point = point.map(|(x, y)| Point { x, y });
        });
    }

    fn set_need(&self, id: &str, name: &str, value: f64) {
        self.with(|world| {
            let mut rows = world.rows_mut().unwrap();
            let needs = &mut rows.agents.get_mut(id).unwrap().needs;
            match name {
                "toilet_urgency" => needs.toilet_urgency = value,
                "hydration_need" => needs.hydration_need = value,
                "hunger" => needs.hunger = value,
                other => panic!("no need {other}"),
            }
        });
    }

    /// `world.model_configs[key]` set to `value`, or gone with None.
    fn set_config(&self, key: &str, value: Option<V>) {
        self.with(|world| {
            let mut configs = world.row_mut::<ValueRow>(MODEL_CONFIGS).unwrap();
            let V::Map(map) = &mut configs.0 else { panic!("the configs are a map") };
            match value {
                Some(value) => {
                    map.insert(text(key), value);
                }
                None => {
                    map.shift_remove(key);
                }
            }
        });
    }

    /// `world.model_configs[key]` changed in place.
    fn edit_config(&self, key: &str, edit: impl FnOnce(&mut V)) {
        self.with(|world| {
            let mut configs = world.row_mut::<ValueRow>(MODEL_CONFIGS).unwrap();
            let V::Map(map) = &mut configs.0 else { panic!("the configs are a map") };
            edit(map.get_mut(key).unwrap());
        });
    }

    fn config(&self, key: &str) -> V {
        self.with(|world| {
            let configs = world.row::<ValueRow>(MODEL_CONFIGS).unwrap();
            let V::Map(map) = &configs.0 else { panic!("the configs are a map") };
            map[key].clone()
        })
    }

    fn set_minute(&self, minute: i64) {
        self.with(|world| world.scalars.get_mut().minute_index = Fit::Ok(minute));
    }

    fn set_duration(&self, minutes: i64) {
        self.with(|world| world.scalars.get_mut().duration_minutes = Fit::Ok(minutes));
    }

    fn minute(&self) -> i64 {
        self.with(|world| world.minute_index().unwrap())
    }

    fn duration(&self) -> i64 {
        self.with(|world| world.duration_minutes().unwrap())
    }

    fn tick(&mut self, minute: i64) -> Tick<'_, Shard<World>, TestHost, RefCell<Run>> {
        Tick {
            cell: &self.loaded.cell,
            host: &mut self.host,
            run: &self.loaded.run,
            policies: self.loaded.policies.clone(),
            minute,
            seed: self.loaded.seed.clone(),
            neighbors: self.loaded.neighbors.clone(),
        }
    }

    /// `prepare_facility_queue`.
    fn prepare(&mut self, facility: &str, resource: bool, minute: i64) -> Result<(), Error> {
        self.tick(minute).prepare_queue(facility, resource).map_err(|failed| failed.error)
    }

    /// `progress_physical_queue_positions` or `progress_physical_queue_head_to_service`.
    fn progress(&mut self, facility: &str, resource: bool, minute: i64, head: bool) -> Result<bool, Error> {
        self.tick(minute).progress(facility, resource, head, None).map_err(|failed| failed.error)
    }

    /// `_same_after`: the walk to the slots, its answer chained.
    fn positions(&mut self, answers: &mut Answers, facility: &str, resource: bool, minute: i64) -> bool {
        let result = self.progress(facility, resource, minute, POSITIONS);
        let shown = format!("{result:?}");
        self.with(|world| answers.add(&shown, world));
        result.unwrap()
    }

    /// `_to_service`: the head's walk to service, which writes only when it schedules one.
    fn to_service(&mut self, facility: &str, resource: bool, minute: i64, changes: bool) -> bool {
        let before = self.bytes();
        let result = self.progress(facility, resource, minute, HEAD).unwrap();
        assert_eq!(self.bytes() != before, changes);
        result
    }

    fn queue(&self, facility: &str, resource: bool) -> Vec<String> {
        self.with(|world| {
            let rows = world.rows().unwrap();
            let row = rows.facilities.get(facility).unwrap();
            let queue = if resource { &row.resource_queue_agent_ids } else { &row.queue_agent_ids };
            queue.iter().map(|id| id.to_string()).collect()
        })
    }

    fn metric(&self, name: &str) -> Option<f64> {
        self.with(|world| world.metric_state().unwrap().get(name).copied())
    }

    fn activity(&self, id: &str) -> Activity {
        self.with(|world| world.rows().unwrap().agents.get(id).unwrap().activity)
    }

    /// The log's records, as JSON.
    fn records(&self) -> Vec<Value> {
        self.with(|world| {
            let log = world.log().unwrap();
            (0..log.len()).map(|index| serde_json::from_slice(log.written(index).unwrap()).unwrap()).collect()
        })
    }

    /// `_people_who_left`.
    fn left(&self) -> Vec<String> {
        self.records()
            .iter()
            .filter(|record| record["event"]["event_type"] == "facility_queue_abandon")
            .map(|record| record["event"]["payload"]["agent_id"].as_str().unwrap().to_owned())
            .collect()
    }

    /// The queue's events in their order.
    fn pending(&self) -> Vec<EventRow> {
        self.with(|world| {
            let queue = world.queue().unwrap();
            queue.ordered().iter().map(|(_, serial)| queue.slot(*serial).row.as_deref().unwrap().clone()).collect()
        })
    }

    fn journey(&self, id: &str) -> Option<sheltersim_core::state::movement::Journey> {
        self.with(|world| world.row::<MovementRow>(MOVEMENT).unwrap().journeys.get(id).cloned())
    }

    fn walking(&self, id: &str) -> bool {
        self.journey(id).is_some()
    }

    /// `_nobody_left`.
    fn nobody_left(&self, facility: &str, people: &[String], resource: bool) {
        assert_eq!(self.queue(facility, resource), people);
        assert!(self.left().is_empty());
        assert_eq!(self.metric(ABANDONED), None);
        for id in people {
            assert_eq!(self.activity(id), Activity::Queued);
        }
    }

    /// `_waited_long`: a config with limits of 50 minutes, and every need at `need`.
    fn waited_long(&self, need: f64, extra: Value) {
        let mut config = json!({"schema_version": 1, "abandon_after_minutes_by_service": {"toilet": 50, "water": 50, "meal": 50}});
        for (key, value) in extra.as_object().unwrap() {
            config[key] = value.clone();
        }
        self.set_config("facility_queues", Some(v(config)));
        self.with(|world| {
            let mut rows = world.rows_mut().unwrap();
            let ids: Vec<Arc<str>> = rows.agents.iter().map(|(id, _)| id.clone()).collect();
            for id in ids {
                let needs = &mut rows.agents.get_mut(&id).unwrap().needs;
                needs.toilet_urgency = need;
                needs.hydration_need = need;
                needs.hunger = need;
            }
        });
    }

    /// `_with_stall`: a stall record of the namespace's shape.
    fn stall(&self, facility: &str, agent: &str, reason: &str) {
        self.with(|world| {
            let mut service = world.row_mut::<ServiceRow>(SERVICE).unwrap();
            service.stall_state.insert(text(facility), Stall { agent_id: text(agent), reason: text(reason) });
        });
    }

    fn stalled(&self, facility: &str) -> bool {
        self.with(|world| world.row::<ServiceRow>(SERVICE).unwrap().stall_state.contains_key(facility))
    }
}

fn names(queue: &[String], first: &str) -> Vec<&'static str> {
    queue.iter().map(|id| if id == first { "first" } else { "second" }).collect()
}

fn xy(points: &[Point]) -> Vec<(f64, f64)> {
    points.iter().map(|point| (point.x, point.y)).collect()
}

fn point_of(value: &Value) -> (f64, f64) {
    (value[0].as_f64().unwrap(), value[1].as_f64().unwrap())
}

// ---- prepare_facility_queue ----------------------------------------------------------------------

/// `_near_tie`: two people whose priorities part by `gap`; the first has waited, the second needs
/// more.
fn near_tie(waited: i64, gap: f64) -> Vec<&'static str> {
    let mut case = Case::new();
    case.set_config("facility_queues", Some(v(json!({"persistence_urgency_threshold": 0.0}))));
    let toilet = case.facility("toilet");
    let people = case.people(2);
    case.line_up(&toilet, &people, false, Some(&[0, waited]));
    let base = 0.25;
    let aging = 0.1 * f64::min(1.0, waited as f64 / 1440.0);
    case.set_need(&people[0], "toilet_urgency", base);
    case.set_need(&people[1], "toilet_urgency", base + aging + gap);
    case.prepare(&toilet, false, waited).unwrap();
    names(&case.queue(&toilet, false), &people[0])
}

/// A 1% change of the weight 0.1 or the day 1440 turns one of these two around.
#[test]
fn aging_decides_a_near_tie_at_its_exact_weight() {
    for waited in [100, 720, 1439, 1440, 3000] {
        assert_eq!(near_tie(waited, 1e-6), ["second", "first"], "{waited}");
        assert_eq!(near_tie(waited, -1e-6), ["first", "second"], "{waited}");
    }
}

#[test]
fn a_tie_goes_to_the_earlier_start_then_the_earlier_place() {
    let mut case = Case::new();
    case.edit_config("facility_queues", |config| {
        let V::Map(map) = config else { panic!("a map") };
        map.insert(text("abandon_after_minutes_by_service"), v(json!({"toilet": 10080})));
    });
    let toilet = case.facility("toilet");
    let people = case.people(3);
    // Needs at the top of the range, so waits make no difference past a day.
    case.line_up(&toilet, &people, false, Some(&[5, 5, 0]));
    for id in &people {
        case.set_need(id, "toilet_urgency", 1.0);
    }
    case.prepare(&toilet, false, 10000).unwrap();
    assert_eq!(case.queue(&toilet, false), [people[2].clone(), people[0].clone(), people[1].clone()]);
}

#[test]
fn each_service_reads_its_own_need() {
    for (kind, field) in [("water_station", "hydration_need"), ("meal_distribution", "hunger")] {
        let mut case = Case::new();
        let facility = case.facility(kind);
        let people = case.people(2);
        case.line_up(&facility, &people, true, None);
        // Everything else says the other way round.
        for name in ["toilet_urgency", "hydration_need", "hunger"] {
            case.set_need(&people[0], name, 0.7);
            case.set_need(&people[1], name, 0.2);
        }
        case.set_need(&people[0], field, 0.2);
        case.set_need(&people[1], field, 0.7);
        case.prepare(&facility, true, 5).unwrap();
        assert_eq!(case.queue(&facility, true), [people[1].clone(), people[0].clone()], "{kind}");
    }
}

#[test]
fn a_need_past_the_range_counts_as_its_end() {
    let mut case = Case::new();
    let toilet = case.facility("toilet");
    let people = case.people(2);
    case.line_up(&toilet, &people, false, Some(&[0, 3]));
    case.set_need(&people[0], "toilet_urgency", 1.0);
    case.set_need(&people[1], "toilet_urgency", 1.5);
    case.prepare(&toilet, false, 3).unwrap();
    assert_eq!(case.queue(&toilet, false), people);
}

#[test]
fn someone_with_no_queue_task_leaves_the_list() {
    let mut case = Case::new();
    let toilet = case.facility("toilet");
    let people = case.people(2);
    case.line_up(&toilet, &people, false, None);
    case.with(|world| world.rows_mut().unwrap().active_tasks.remove_row(&format!("queue-task-{}", people[0])));
    case.prepare(&toilet, false, 3).unwrap();
    assert_eq!(case.queue(&toilet, false), [people[1].clone()]);
}

/// The world after the walk is recorded from the core in #225 lane M3; on the same kernel the pytest
/// `test_queue_rules.py::test_the_list_is_left_alone_while_someone_walks_in_it` with digest
/// `5a930920ab0e` passed.
#[test]
fn the_list_is_left_alone_while_someone_walks_in_it() {
    let mut case = Case::new();
    let mut answers = Answers::default();
    let toilet = case.facility("toilet");
    let people = case.people(2);
    case.line_up(&toilet, &people, false, None);
    case.set_need(&people[0], "toilet_urgency", 0.1);
    case.set_need(&people[1], "toilet_urgency", 0.9);
    case.set_minute(3);
    assert!(case.positions(&mut answers, &toilet, false, 3));
    assert!(case.walking(&people[0]));
    let before = case.bytes();
    case.prepare(&toilet, false, 3).unwrap();
    assert_eq!(case.queue(&toilet, false), people);
    assert!(case.bytes() == before);
    assert_eq!(answers.hex(), "06709da20ea35abb8ae2497cfb775026252e55ea0a5c22af283a06c286af62b3");
}

#[test]
fn each_default_limit_and_the_threshold_are_where_someone_gives_up() {
    for (kind, limit, field) in [("toilet", 120, "toilet_urgency"), ("water_station", 240, "hydration_need"), ("meal_distribution", 360, "hunger")] {
        let resource = kind != "toilet";
        let lined_up = |need: f64| {
            let case = Case::new();
            case.set_duration(2000);
            case.set_config("facility_queues", Some(v(json!({"schema_version": 1}))));
            let facility = case.facility(kind);
            let person = case.people(1);
            case.line_up(&facility, &person, resource, None);
            case.set_need(&person[0], field, need);
            (case, facility, person)
        };
        let below = f64::from_bits(0.8f64.to_bits() - 1);
        let (mut case, facility, person) = lined_up(below);
        case.prepare(&facility, resource, limit - 1).unwrap();
        case.nobody_left(&facility, &person, resource);
        let (mut case, facility, person) = lined_up(below);
        case.prepare(&facility, resource, limit).unwrap();
        assert_eq!(case.metric(ABANDONED), Some(1.0));
        assert_eq!(case.with(|world| world.rows().unwrap().agents.get(&person[0]).unwrap().next_decision_at), limit + 15);
        assert_eq!(case.left(), person);
        assert!(case.with(|world| world.rows().unwrap().active_tasks.get(&format!("queue-task-{}", person[0])).is_none()));
        let (mut case, facility, person) = lined_up(0.8);
        case.prepare(&facility, resource, limit).unwrap();
        case.nobody_left(&facility, &person, resource);
    }
}

#[test]
fn a_config_s_limit_and_threshold_are_read() {
    let lined_up = |need: f64, threshold: Value| {
        let case = Case::new();
        case.set_config(
            "facility_queues",
            Some(v(json!({
                "schema_version": 1,
                "abandon_after_minutes_by_service": {"toilet": 50},
                "retry_delay_minutes": 3,
                "persistence_urgency_threshold": threshold,
            }))),
        );
        let toilet = case.facility("toilet");
        let person = case.people(1);
        case.line_up(&toilet, &person, false, None);
        case.set_need(&person[0], "toilet_urgency", need);
        (case, toilet, person)
    };
    let (mut case, toilet, person) = lined_up(0.4, json!(0.5));
    case.prepare(&toilet, false, 49).unwrap();
    case.nobody_left(&toilet, &person, false);
    let (mut case, toilet, person) = lined_up(0.4, json!(0.5));
    case.prepare(&toilet, false, 50).unwrap();
    assert_eq!(case.metric(ABANDONED), Some(1.0));
    assert_eq!(case.left(), person);
    assert_eq!(case.with(|world| world.rows().unwrap().agents.get(&person[0]).unwrap().next_decision_at), 53);
    let (mut case, toilet, person) = lined_up(0.5, json!(0.5));
    case.prepare(&toilet, false, 50).unwrap();
    case.nobody_left(&toilet, &person, false);
    let (mut case, toilet, person) = lined_up(0.4, json!(1));
    case.prepare(&toilet, false, 50).unwrap();
    assert_eq!(case.left(), person);
    let (mut case, toilet, person) = lined_up(0.4, json!(0));
    case.prepare(&toilet, false, 50).unwrap();
    case.nobody_left(&toilet, &person, false);
}

/// `config.get("schema_version", 1) != 1` let these through, and so does the kernel.
#[test]
fn a_schema_version_python_counts_as_1_is_read() {
    for schema in [json!(1.0), json!(true)] {
        let mut case = Case::new();
        case.waited_long(0.1, json!({"schema_version": schema}));
        let toilet = case.facility("toilet");
        let person = case.people(1);
        case.line_up(&toilet, &person, false, None);
        case.prepare(&toilet, false, 60).unwrap();
        assert_eq!(case.left(), person, "{schema}");
    }
}

#[test]
fn someone_who_gives_up_walks_back() {
    let mut case = Case::new();
    case.waited_long(0.1, json!({"retry_delay_minutes": 7}));
    let toilet = case.facility("toilet");
    let people = case.people(3);
    let (first, second, third) = (&people[0], &people[1], &people[2]);
    case.line_up(&toilet, &people, false, None);
    case.away_from_the_middle(second, 0.3);
    case.away_from_the_middle(third, 0.6);
    let sequence = case.with(|world| world.id_counter("event").unwrap().unwrap_or(0));
    case.prepare(&toilet, false, 60).unwrap();
    assert!(case.queue(&toilet, false).is_empty());
    assert_eq!(case.left(), people);
    let (agent_first, agent_second) = case.with(|world| {
        let rows = world.rows().unwrap();
        (rows.agents.get(first.as_str()).unwrap().clone(), rows.agents.get(second.as_str()).unwrap().clone())
    });
    assert_eq!((agent_first.activity, agent_first.target_id), (Activity::Idle, None));
    assert_eq!(agent_second.activity, Activity::Moving);
    assert_eq!(agent_second.target_id.as_deref(), Some(&*agent_second.zone_id));
    assert_eq!(agent_second.next_decision_at, 67);
    assert_eq!(&*case.journey(second).unwrap().purpose, "queue_abandon");
    assert_eq!(case.metric(ABANDONED), Some(3.0));
    assert!(case.with(|world| world.rows().unwrap().active_tasks.iter().all(|(id, _)| !id.starts_with("queue-task-"))));
    // The first takes one sequence; each who walks takes one more for the walk.
    let left: Vec<Value> = case.records().into_iter().filter(|record| record["event"]["event_type"] == "facility_queue_abandon").collect();
    let ids: Vec<&str> = left.iter().map(|record| record["event"]["event_id"].as_str().unwrap()).collect();
    assert_eq!(
        ids,
        [format!("facility-queue-abandon-{sequence}"), format!("facility-queue-abandon-{}", sequence + 1), format!("facility-queue-abandon-{}", sequence + 3)]
    );
    let record = &left[0];
    assert_eq!(record["detail"], "Agent left a prolonged low-urgency queue and will reconsider after a bounded delay");
    let event = &record["event"];
    assert_eq!((&record["realized_minute"], &event["due_minute"], &event["priority"]), (&json!(60), &json!(60), &json!(0)));
    assert_eq!(
        event["payload"],
        json!({
            "facility_id": toilet,
            "agent_id": first,
            "task_id": format!("queue-task-{first}"),
            "service": "toilet",
            "start_minute": 0,
            "wait_minutes": 60,
            "retry_at_minute": 67,
            "reason": "need_weighted_wait_threshold",
        })
    );
    assert_eq!(event["source_id"], "ASM-NEED-WEIGHTED-PHYSICAL-QUEUE-V1");
    assert_eq!(event["causation_id"], json!(format!("queue-task-{first}")));
    assert_eq!(
        event["provenance"],
        json!({
            "source_type": "assumption",
            "source_id": "ASM-NEED-WEIGHTED-PHYSICAL-QUEUE-V1",
            "policy_version": "need-weighted-physical-queue-v1",
            "transition": "abandon_queue_then_retry",
        })
    );
    let walks: Vec<EventRow> = case.pending().into_iter().filter(|event| event.event_id.starts_with("queue-abandon-arrival-")).collect();
    let mut walkers: Vec<String> = walks.iter().map(|event| json_of(&event.payload)["agent_id"].as_str().unwrap().to_owned()).collect();
    walkers.sort();
    let mut expected = vec![second.clone(), third.clone()];
    expected.sort();
    assert_eq!(walkers, expected);
    let walk = walks.iter().find(|event| json_of(&event.payload)["agent_id"] == json!(second)).unwrap();
    let payload = json_of(&walk.payload);
    assert_eq!(&*walk.event_id, format!("queue-abandon-arrival-{}", sequence + 2));
    assert_eq!(&*walk.event_type, "agent_travel_complete");
    assert_eq!(walk.causation_id.as_deref(), Some(format!("facility-queue-abandon-{}", sequence + 1).as_str()));
    let travel_minutes = payload["travel_minutes"].as_i64().unwrap();
    assert!(travel_minutes > 0);
    assert_eq!(walk.due_minute, 60 + travel_minutes);
    assert_eq!(payload["purpose"], "queue_abandon");
    let task_id = format!("queue-abandon-travel-{}", sequence + 2);
    assert_eq!(payload["task_id"], json!(task_id));
    let zone = &*agent_second.zone_id;
    assert_eq!(payload["route_zone_ids"], json!([zone, zone]));
    assert_eq!(payload["route_edge_ids"], json!([]));
    assert_eq!(json_of(&walk.provenance)["movement"], "in_room_return_to_zone_anchor");
    let travel = case.with(|world| world.rows().unwrap().active_tasks.get(&task_id).unwrap().clone());
    assert_eq!((&*travel.kind, travel.agent_id.as_deref(), travel.resource_id), ("travel_return", Some(second.as_str()), None));
    assert_eq!((travel.start_minute, travel.due_minute), (60, walk.due_minute));
}

#[test]
fn only_those_past_their_limit_give_up_and_the_rest_keep_their_order() {
    let mut case = Case::new();
    case.waited_long(0.1, json!({}));
    let toilet = case.facility("toilet");
    let people = case.people(3);
    case.line_up(&toilet, &people, false, Some(&[5, 30, 5]));
    case.set_need(&people[0], "toilet_urgency", 0.9);
    case.prepare(&toilet, false, 60).unwrap();
    assert_eq!(case.queue(&toilet, false), [people[0].clone(), people[1].clone()]);
    assert_eq!(case.left(), [people[2].clone()]);
}

#[test]
fn no_way_out_leaves_someone_in_line() {
    let mut case = Case::new();
    case.waited_long(0.1, json!({}));
    let toilet = case.facility("toilet");
    let person = case.people(1);
    case.line_up(&toilet, &person, false, None);
    // Far outside the zone's polygon, so no path starts here.
    case.away_from_the_middle(&person[0], 10_000.0);
    case.prepare(&toilet, false, 60).unwrap();
    assert_eq!(case.queue(&toilet, false), person);
    assert!(case.left().is_empty());
}

#[test]
fn a_walk_past_the_end_of_the_run_leaves_someone_in_line() {
    let mut case = Case::new();
    case.waited_long(0.1, json!({}));
    let toilet = case.facility("toilet");
    let person = case.people(1);
    case.line_up(&toilet, &person, false, None);
    case.away_from_the_middle(&person[0], 0.3);
    case.set_duration(61);
    case.prepare(&toilet, false, 60).unwrap();
    case.nobody_left(&toilet, &person, false);
}

#[test]
fn giving_up_with_no_position_or_no_movement_state() {
    let mut case = Case::new();
    case.waited_long(0.1, json!({}));
    let toilet = case.facility("toilet");
    let person = case.people(1);
    case.line_up(&toilet, &person, false, None);
    case.put_point(&person[0], None);
    case.prepare(&toilet, false, 60).unwrap();
    assert_eq!(case.left(), person);
    assert_eq!(case.activity(&person[0]), Activity::Idle);
}

#[test]
fn giving_up_drops_a_stall_that_is_no_longer_the_head_s() {
    let mut case = Case::new();
    case.waited_long(0.1, json!({}));
    case.set_duration(2000);
    let water = case.facility("water_station");
    let people = case.people(2);
    case.line_up(&water, &people, true, Some(&[10, 40]));
    case.set_need(&people[1], "hydration_need", 0.9);
    case.stall(&water, &people[0], "staff");
    case.prepare(&water, true, 60).unwrap();
    assert_eq!(case.left(), [people[0].clone()]);
    assert!(case.with(|world| world.row::<ServiceRow>(SERVICE).unwrap().stall_state.is_empty()));
}

/// A 1% change of the default 15 shows here: the Run's configs all write theirs.
#[test]
fn giving_up_takes_its_retry_delay_from_the_config() {
    let mut case = Case::new();
    case.waited_long(0.1, json!({}));
    let toilet = case.facility("toilet");
    let person = case.people(1);
    case.line_up(&toilet, &person, false, None);
    case.edit_config("facility_queues", |config| {
        let V::Map(map) = config else { panic!("a map") };
        map.shift_remove("retry_delay_minutes");
    });
    case.prepare(&toilet, false, 60).unwrap();
    assert_eq!(case.with(|world| world.rows().unwrap().agents.get(&person[0]).unwrap().next_decision_at), 75);
}

/// The strict FIFO Scenario (`generated_scenario_with(queue_policy_id="strict-fifo-queue-v1")`)
/// selects its queue policy with its `policy_registry` and `facility_queues`: this world with those
/// configs and that Scenario's selection.
#[test]
fn the_reference_fifo_keeps_the_order_of_starts_and_nobody_gives_up() {
    let mut entry = entry();
    let strict = entry["strict_queue"].clone();
    for (key, value) in strict["configs"].as_object().unwrap() {
        entry["world"]["model_configs"][key] = value.clone();
    }
    let selected = strict["selection"]["value"].as_array().unwrap();
    assert!(selected.contains(&json!(["queue", "strict-fifo-queue-v1"])));
    entry["sections"]["selection"] = strict["selection"].clone();
    let mut case = Case::of(entry);
    let toilet = case.facility("toilet");
    let people = case.people(2);
    case.line_up(&toilet, &people, false, Some(&[4, 1]));
    case.set_need(&people[0], "toilet_urgency", 0.99);
    case.prepare(&toilet, false, 9000).unwrap();
    assert_eq!(case.queue(&toilet, false), [people[1].clone(), people[0].clone()]);
    assert!(case.left().is_empty());
}

const LIMITS_TEXT: &str = "queue abandonment limits must be within 1..10080 minutes";
const SERVICE_TEXT: &str = "queue abandonment limits contain an unsupported service";
const RETRY_TEXT: &str = "queue retry delay must be between 1 and 1440 minutes";
const THRESHOLD_TEXT: &str = "queue persistence threshold must be normalized to 0..1";
const OBJECT_TEXT: &str = "facility_queues config must be an object";

/// The rule raises the policy's ValueError, its text, before changing anything
/// (docs/rust-port/lanes/l8-texts-rest.md). A config the model selection turns down first is the
/// pytest's (Python raises that error).
#[test]
fn a_config_python_turns_down_raises_the_python_rule_s_error() {
    let nan = {
        let mut map = indexmap::IndexMap::new();
        map.insert(text("persistence_urgency_threshold"), V::Float(f64::NAN));
        V::Map(map)
    };
    let cases: Vec<(V, &str)> = vec![
        (v(json!({"schema_version": 2})), "facility_queues requires schema_version 1"),
        (v(json!({"schema_version": 1, "other": 1})), OBJECT_TEXT),
        (v(json!({"abandon_after_minutes_by_service": {"toilet": 0}})), LIMITS_TEXT),
        (v(json!({"abandon_after_minutes_by_service": {"toilet": 10081}})), LIMITS_TEXT),
        (v(json!({"abandon_after_minutes_by_service": {"toilet": 12.0}})), LIMITS_TEXT),
        (v(json!({"abandon_after_minutes_by_service": {"shower": 5}})), SERVICE_TEXT),
        (v(json!({"retry_delay_minutes": 0})), RETRY_TEXT),
        (v(json!({"retry_delay_minutes": 1441})), RETRY_TEXT),
        (v(json!({"persistence_urgency_threshold": 1.5})), THRESHOLD_TEXT),
        (v(json!({"persistence_urgency_threshold": true})), THRESHOLD_TEXT),
        (nan, THRESHOLD_TEXT),
        (v(json!({"persistence_urgency_threshold": "high"})), THRESHOLD_TEXT),
        (v(json!({"persistence_urgency_threshold": 2})), THRESHOLD_TEXT),
        (v(json!({"abandon_after_minutes_by_service": []})), SERVICE_TEXT),
        (v(json!({"retry_delay_minutes": 3.0})), RETRY_TEXT),
        (v(json!({"schema_version": 1, "abandon_after_minutes_by_service": {"meal": 0}, "retry_delay_minutes": 0})), LIMITS_TEXT),
    ];
    for (config, said) in cases {
        let mut case = Case::new();
        let toilet = case.facility("toilet");
        case.line_up(&toilet, &case.people(1), false, None);
        let before = case.bytes();
        let signed = case.config("facility_queues");
        case.set_config("facility_queues", Some(config.clone()));
        assert_eq!(case.prepare(&toilet, false, 3), Err(Error::Value(said.to_owned())), "{config:?}");
        case.set_config("facility_queues", Some(signed));
        assert!(case.bytes() == before, "{config:?}");
    }
}

#[test]
fn a_stall_stays_with_the_head_and_goes_when_the_head_changes() {
    let mut case = Case::new();
    let facility = case.facility("water_station");
    let people = case.people(2);
    case.line_up(&facility, &people, true, None);
    case.set_need(&people[0], "hydration_need", 0.9);
    case.set_need(&people[1], "hydration_need", 0.1);
    case.stall(&facility, &people[0], "stock");
    let history = case.with(|world| field_json(&world.row::<ServiceRow>(SERVICE).unwrap().queue_stall_history));
    case.prepare(&facility, true, 5).unwrap();
    assert!(case.stalled(&facility));
    case.set_need(&people[0], "hydration_need", 0.1);
    case.set_need(&people[1], "hydration_need", 0.9);
    case.prepare(&facility, true, 5).unwrap();
    assert_eq!(case.queue(&facility, true), [people[1].clone(), people[0].clone()]);
    assert!(!case.stalled(&facility));
    assert_eq!(case.with(|world| field_json(&world.row::<ServiceRow>(SERVICE).unwrap().queue_stall_history)), history);
}

#[test]
fn a_stall_goes_when_its_person_is_no_longer_in_the_queue() {
    let mut case = Case::new();
    let facility = case.facility("meal_distribution");
    let people = case.people(2);
    case.line_up(&facility, &people[1..], true, None);
    case.stall(&facility, &people[0], "stock");
    case.prepare(&facility, true, 5).unwrap();
    assert!(!case.stalled(&facility));
}

/// A strange kind raises the rule's error with Python's `repr` of it, which `str.casefold` decides:
/// "İ" folds to "i̇", so "İtoilet" is a toilet, and "TOİLET" folds to "toi̇let", which is not. A
/// missing facility changes nothing. (Someone listed twice is the pytest's: Python's check of the
/// state turns it down.)
#[test]
fn a_strange_kind_and_a_missing_facility() {
    let mut case = Case::new();
    let toilet = case.facility("toilet");
    let first = case.people(1);
    case.line_up(&toilet, &first, false, None);
    let set_kind = |case: &Case, kind: &str| case.with(|world| world.rows_mut().unwrap().facilities.get_mut(toilet.as_str()).unwrap().kind = text(kind));
    set_kind(&case, "İtoilet");
    case.prepare(&toilet, false, 3).unwrap();
    assert_eq!(case.queue(&toilet, false), first);
    for (kind, shown) in [("TOİLET", "'TOİLET'"), ("shelf", "'shelf'")] {
        set_kind(&case, kind);
        let before = case.bytes();
        assert_eq!(case.prepare(&toilet, false, 3), Err(Error::Value(format!("unsupported queue service facility kind {shown}"))));
        assert!(case.bytes() == before);
    }
    set_kind(&case, "toilet");
    let before = case.bytes();
    case.prepare("nowhere", false, 3).unwrap();
    assert!(case.bytes() == before);
}

// ---- progress_physical_queue_positions and _head_to_service --------------------------------------

/// Recorded from the core in #225 lane M3; on the same kernel the pytest
/// `test_queue_rules.py::test_the_first_who_is_not_in_their_place_walks_there` with digest
/// `25917c5cddb3` passed.
#[test]
fn the_first_who_is_not_in_their_place_walks_there() {
    let mut case = Case::new();
    let mut answers = Answers::default();
    let toilet = case.facility("toilet");
    let people = case.people(3);
    case.line_up(&toilet, &people, false, None);
    let minute = case.minute();
    // The head walks to the slot, which holds service back.
    assert!(case.positions(&mut answers, &toilet, false, minute));
    let journey = case.journey(&people[0]).unwrap();
    assert_eq!(&*journey.purpose, "toilet_queue_position");
    let event = case.pending().into_iter().find(|event| event.event_id == journey.event_id).unwrap();
    assert_eq!(json_of(&event.provenance)["fifo_position"], json!(0));
    assert_eq!(json_of(&event.payload)["task_id"], json!(format!("queue-task-{}", people[0])));
    // While that walk lasts nobody else starts one, and service waits for it.
    assert!(case.positions(&mut answers, &toilet, false, minute));
    assert!(case.to_service(&toilet, false, minute, false));
    assert_eq!(answers.hex(), "903ee38a3df2c06167588491c9d0c888819e11c32fc9339b7bf779b2023b1cc1");
}

/// Recorded from the core in #225 lane M3; on the same kernel the pytest
/// `test_queue_rules.py::test_someone_who_is_moved_from_another_activity_is_queued_by_the_walk`
/// with digest `845e8b40162e` passed.
#[test]
fn someone_who_is_moved_from_another_activity_is_queued_by_the_walk() {
    let mut case = Case::new();
    let mut answers = Answers::default();
    let facility = case.facility("water_station");
    let person = case.people(1);
    case.line_up(&facility, &person, true, None);
    case.with(|world| {
        let mut rows = world.rows_mut().unwrap();
        let agent = rows.agents.get_mut(&person[0]).unwrap();
        agent.activity = Activity::Idle;
        agent.target_id = None;
    });
    let minute = case.minute();
    assert!(case.positions(&mut answers, &facility, true, minute));
    let agent = case.with(|world| world.rows().unwrap().agents.get(&person[0]).unwrap().clone());
    assert_eq!((agent.activity, agent.target_id.as_deref()), (Activity::Queued, Some(facility.as_str())));
    assert_eq!(&*case.journey(&person[0]).unwrap().purpose, "resource_queue_position");
    assert_eq!(answers.hex(), "58d2cbfeea6dcab006dc80fc8b02a6ea1762e6fb326e240314bb1d7983482d70");
}

/// The head is at the facility, the follower 0.05 m from the slot, either side of it (the points
/// the pytest computed in Python: `walks.follower`). Recorded from the core in #225 lane M3; on the
/// same kernel the pytest `test_queue_rules.py::test_a_follower_within_the_tolerance_stands_in_their_slot`
/// with digests `f972edadbc0c` (0.0499) and `3de0a726d52c` (0.0501) passed.
#[test]
fn a_follower_within_the_tolerance_stands_in_their_slot() {
    for (away, walks, recorded) in [(0.0499, false, "34d7129ce639f9bf8b6e578338bd2a34ef2d2af48fff27512e67af3480e3a784"), (0.0501, true, "47fd455fc98b18fc402226823664cfadb0ee7ab166b72a1ceb1c9d71ca2a928e")] {
        let mut case = Case::new();
        let mut answers = Answers::default();
        let toilet = case.facility("toilet");
        let people = case.people(2);
        case.line_up(&toilet, &people, false, None);
        let follower = &case.entry["walks"]["follower"];
        let head = point_of(&follower["head_point"]);
        let (x, y) = point_of(&follower["slot"]);
        case.put_point(&people[0], Some(head));
        case.put_point(&people[1], Some((x + away, y)));
        let minute = case.minute();
        assert!(!case.positions(&mut answers, &toilet, false, minute));
        assert_eq!(case.walking(&people[1]), walks, "{away}");
        assert_eq!(answers.hex(), recorded, "{away}");
    }
}

#[test]
fn a_head_with_no_route_holds_the_queue_and_a_follower_only_those_behind() {
    let mut case = Case::new();
    let toilet = case.facility("toilet");
    let people = case.people(2);
    case.line_up(&toilet, &people, false, None);
    // In another room there's no route in this one to any slot.
    let other_zone = case.with(|world| {
        let rows = world.rows().unwrap();
        let home = rows.facilities.get(toilet.as_str()).unwrap().zone_id.clone();
        let mut zones: Vec<Arc<str>> = rows.zones.iter().map(|(id, _)| id.clone()).collect();
        zones.sort();
        zones.into_iter().find(|zone| *zone != home).unwrap()
    });
    case.with(|world| world.rows_mut().unwrap().agents.get_mut(&people[0]).unwrap().zone_id = other_zone);
    let minute = case.minute();
    assert!(case.to_service(&toilet, false, minute, true));
    assert_eq!(case.metric(BLOCKED), Some(1.0));
    assert!(!case.walking(&people[0]));
}

/// A speed that puts the walk just under 5 minutes: 1% more length would make it 6 (the speeds the
/// pytest computed in Python: `walks.speeds`). The walk to the slot is recorded from the core in
/// #225 lane M3; on the same kernel the pytest
/// `test_queue_rules.py::test_a_walk_s_minutes_turn_over_where_python_s_do[position]` with digest
/// `af7e67cc53d2` passed.
#[test]
fn a_walk_s_minutes_turn_over_where_python_s_do() {
    for phase in ["position", "service"] {
        let mut case = Case::new();
        let mut answers = Answers::default();
        let toilet = case.facility("toilet");
        let person = case.people(1);
        case.line_up(&toilet, &person, false, None);
        let mobility = case.with(|world| world.rows().unwrap().agents.get(&person[0]).unwrap().profile.mobility.as_text());
        let speed = case.entry["walks"]["speeds"][phase].as_f64().unwrap();
        case.edit_config("movement_model", |config| {
            let V::Map(model) = config else { panic!("a map") };
            let Some(V::Map(speeds)) = model.get_mut("speed_m_per_minute_by_mobility") else { panic!("a map") };
            speeds.insert(text(mobility), V::Float(speed));
        });
        let minute = case.minute();
        if phase == "position" {
            assert!(case.positions(&mut answers, &toilet, false, minute));
            assert_eq!(answers.hex(), "c1837d28b6b16da03076be8a641d1846ce8664c64e2cea92f20e669741f95ff5");
        } else {
            assert!(case.to_service(&toilet, false, minute, true));
        }
        let journey = case.journey(&person[0]).unwrap();
        assert_eq!(journey.due_minute - journey.started_minute, 5, "{phase}");
    }
}

/// Recorded from the core in #225 lane M3; on the same kernel the pytest
/// `test_queue_rules.py::test_a_walk_past_the_end_of_the_run_is_counted_and_not_started` with
/// digest `24f43e263bc5` passed.
#[test]
fn a_walk_past_the_end_of_the_run_is_counted_and_not_started() {
    let mut case = Case::new();
    let mut answers = Answers::default();
    let toilet = case.facility("toilet");
    let person = case.people(1);
    case.line_up(&toilet, &person, false, None);
    let minute = case.duration() - 1;
    assert!(case.positions(&mut answers, &toilet, false, minute));
    assert_eq!(case.metric(BLOCKED), Some(1.0));
    assert!(!case.walking(&person[0]));
    assert_eq!(answers.hex(), "3c120d9543aaaa3be7127dbcc6492f74254e8f7f2f797740887a930b08f10bd2");
}

/// The route is the one the pytest computed in Python (`walks.head_route`). Recorded from the core
/// in #225 lane M3; on the same kernel the pytest
/// `test_queue_rules.py::test_the_head_walks_from_its_slot_to_the_service_point` with digest
/// `f18ab419ea38` passed.
#[test]
fn the_head_walks_from_its_slot_to_the_service_point() {
    let mut case = Case::new();
    let mut answers = Answers::default();
    let toilet = case.facility("toilet");
    let person = case.people(1);
    case.line_up(&toilet, &person, false, None);
    let minute = case.minute();
    let route = case.entry["walks"]["head_route"].clone();
    let path: Vec<(f64, f64)> = route["points"].as_array().unwrap().iter().map(point_of).collect();
    assert!(case.to_service(&toilet, false, minute, true));
    let journey = case.journey(&person[0]).unwrap();
    assert_eq!(&*journey.purpose, "toilet_queue_service");
    assert_eq!(xy(&journey.route_points), path);
    assert_eq!(journey.due_minute, minute + route["travel_minutes"].as_i64().unwrap());
    let event = case.pending().into_iter().find(|event| event.event_id == journey.event_id).unwrap();
    assert_eq!(&*event.event_type, "agent_travel_complete");
    assert_eq!(event.due_minute, journey.due_minute);
    let payload = json_of(&event.payload);
    assert_eq!(payload["purpose"], "toilet_queue_service");
    assert_eq!(payload["task_id"], json!(format!("queue-task-{}", person[0])));
    let provenance = json_of(&event.provenance);
    assert_eq!((&provenance["queue_phase"], &provenance["fifo_position"]), (&json!("service"), &json!(0)));
    assert_eq!(case.activity(&person[0]), Activity::Queued);
    // At the point, the walk is over and service may start.
    let mut case = Case::new();
    case.line_up(&toilet, &person, false, None);
    case.put_point(&person[0], path.last().copied());
    assert!(!case.to_service(&toilet, false, minute, false));
    assert!(!case.positions(&mut answers, &toilet, false, minute));
    assert_eq!(answers.hex(), "e05cd4b925b0a18f6f78cb1a3cce4d9aa11014559d060f8395a5d717e9d7c350");
}

/// Recorded from the core in #225 lane M3; on the same kernel the pytest
/// `test_queue_rules.py::test_an_empty_queue_and_a_missing_facility` with digest `1081e598c3d4`
/// passed.
#[test]
fn an_empty_queue_and_a_missing_facility() {
    let mut case = Case::new();
    let mut answers = Answers::default();
    let toilet = case.facility("toilet");
    let minute = case.minute();
    assert!(!case.positions(&mut answers, &toilet, false, minute));
    assert!(case.positions(&mut answers, "nowhere", false, minute));
    assert!(!case.to_service(&toilet, false, minute, false));
    assert!(case.to_service("nowhere", false, minute, false));
    assert_eq!(answers.hex(), "f5d7ed9c21c00fcae4802bf43f8b249bf2cf2d1149cf5430004eed2bf0c7aee5");
}

/// The deleted rule's ValueError, which the kernel raises before it writes anything.
#[test]
fn without_a_task_the_rule_raises_its_own_error() {
    let mut case = Case::new();
    let toilet = case.facility("toilet");
    let person = case.people(1);
    case.line_up(&toilet, &person, false, None);
    case.with(|world| world.rows_mut().unwrap().active_tasks.remove_row(&format!("queue-task-{}", person[0])));
    let minute = case.minute();
    let before = case.bytes();
    assert_eq!(
        case.progress(&toilet, false, minute, POSITIONS),
        Err(Error::Value("physical queue reservation has no matching active queue task".to_owned()))
    );
    assert!(case.bytes() == before);
}
