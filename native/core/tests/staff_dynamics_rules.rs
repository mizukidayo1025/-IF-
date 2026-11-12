//! The staff dynamics' update and availability on the small world of
//! `tests/integration/test_staff_dynamics_row.py` (Issue #225, lane M11; moved from that file).
//!
//! The world is the pytest file's `_world()`: three staff members (`S01`..`S03`) and the namespace
//! `create_staff_dynamics_state` made for an empty config, exported by
//! `tests/support/core_fixtures_staff_work.py` (`staff_dynamics.json`). The rules are the kernels
//! the PyO3 entries call (`rules::staff_dynamics::update`, `is_available_now`), with the fatigue
//! formula the core reads from the configs (`StaffFatigue::parse`), as the entries read it.
//!
//! The tests of the pytest file that draw hundreds of random worlds and questions with
//! `random.Random` stay there (decision 5 of #225), with the entries' tests and the comparisons
//! with the Python checks of a config and of a saved namespace.

mod support;

use std::sync::Arc;

use serde_json::Value;
use sheltersim_core::config::staff::StaffFatigue;
use sheltersim_core::rules::staff_dynamics;
use sheltersim_core::state::staff_dynamics::{LastActivity, StaffDynamicsRow, StaffRecord, NAME};
use sheltersim_core::world::World;

use support::load;

const FILE: &str = "staff_dynamics";

fn base() -> (World, Value) {
    let entry = load::entry(FILE, "base");
    (load::world(&entry["world"], &entry["slots"]), entry)
}

fn record(world: &World, id: &str) -> StaffRecord {
    *world.row::<StaffDynamicsRow>(NAME).unwrap().staff.get(id).unwrap()
}

fn fields(record: StaffRecord) -> (i64, i64, i64, f64, i64, LastActivity) {
    (record.consecutive_work_minutes, record.total_work_minutes, record.total_rest_minutes, record.fatigue, record.rest_until_minute, record.last_activity)
}

/// `test_a_working_minute_is_the_formula_s_fatigue`: the linear formula's fatigue is
/// `min(1.0, consecutive / maximum)`, 240 by default; a member without a task is off duty and
/// keeps a new record.
#[test]
fn a_working_minute_is_the_formula_s_fatigue() {
    let (mut world, entry) = base();
    world.rows_mut().unwrap().staff.get_mut("S01").unwrap().active_task_id = Some(Arc::from("t"));
    let formula = StaffFatigue::parse(&load::v(&entry["world"]["model_configs"])).unwrap();
    staff_dynamics::update(&mut world, 0, &formula).unwrap();
    assert_eq!(fields(record(&world, "S01")), (1, 1, 0, 1.0 / 240.0, 0, LastActivity::Working));
    assert_eq!(fields(record(&world, "S02")), fields(StaffRecord::new()));
}

/// `test_the_availability_reader_takes_each_shape_of_the_operations_state`: `operation is None or
/// operation.get("phase") == "idle"` of `staff_operations_v1`, for each shape of the namespace
/// (a record idle, travelling or working, no record, and None under the key, which the entry reads
/// as no state).
#[test]
fn the_availability_reader_takes_each_shape_of_the_operations_state() {
    let data = load::data(FILE);
    let shapes = data["availability"].as_array().unwrap();
    assert_eq!(shapes.len(), 5);
    for shape in shapes {
        let (document, available) = (&shape[0], shape[1].as_bool().unwrap());
        let (mut world, _) = base();
        let absent = document.is_null();
        if !absent {
            load::put_namespace(&mut world, "staff_operations_v1", load::v(document));
        }
        assert_eq!(staff_dynamics::is_available_now(&world, "S01", 0, absent), Ok(available), "{document}");
    }
}
