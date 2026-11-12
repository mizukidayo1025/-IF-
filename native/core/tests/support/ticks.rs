//! A Run's ticks on a loaded world, as `TickEngine.step` steps it (#214; shared since Issue #225,
//! lane M7): `run_tick` at the world's minute with the host, the minute moved on, then the
//! end-of-tick check (`after_tick::check`, with no Python validator to ask: `NoPython`). The crate's
//! `sheltersim_core::engine` (#244, PR 4-3) does it; these are its calls on a loaded world with the
//! `TestHost`, which panic where it stops.
//!
//! `tick_core.rs` checks that these ticks reach the native path's world; the tests of lane M7 run
//! the generated world's ticks with them and read the worlds of the Run (`world_view_run.rs`,
//! `metric_snapshot.rs`). `metric_line` writes a metric sample as the JSON list of the snapshot's
//! fields, as `json.dumps` of `_snapshot_fields` with `separators=(",", ":")` writes it.

use sheltersim_core::engine::{self, StepError, Stepping};
use sheltersim_core::error::Error;
use sheltersim_core::rules::after_tick::Memo;
use sheltersim_core::rules::metrics::MetricSample;
use sheltersim_core::rules::tick::Tick;
use sheltersim_core::world::World;

use super::load::Loaded;
use super::TestHost;

/// The end-of-tick check's questions to Python (the crate's, #244 PR 4-3).
pub use sheltersim_core::engine::NoPython;

/// `WorldState.validate_after_tick`: the core's check, with the plan and the sections it reads
/// (`engine::check_after_tick`).
pub fn check_after_tick(loaded: &Loaded, host: &mut TestHost, memo: &mut Memo, checks: &mut NoPython) -> std::result::Result<(), Error> {
    engine::check_after_tick(&loaded.cell, host, memo, checks)
}

/// A tick of the loaded Run at `minute`, with this host (`engine::tick_at`).
pub fn tick_at<'a>(loaded: &'a Loaded, host: &'a mut TestHost, minute: i64) -> Tick<'a, sheltersim_core::cell::Shard<World>, TestHost, std::cell::RefCell<sheltersim_core::run::Run>> {
    engine::tick_at(loaded, host, minute)
}

/// `TickEngine.step`, `ticks` times (`engine::step`): `run_tick` at the world's minute, the minute
/// moved on, the check; `after` sees the world after each tick (its minute the next one), with the
/// host. What the check asked of Python.
pub fn run(loaded: &Loaded, host: &mut TestHost, ticks: i64, after: &mut dyn FnMut(&Loaded, &mut TestHost)) -> Vec<String> {
    let mut stepping = Stepping::default();
    for _ in 0..ticks {
        match engine::step(loaded, host, &mut stepping) {
            Ok(_) => {}
            Err(StepError::Complete) => panic!("the clock is complete"),
            Err(StepError::Tick { minute, label, error }) => panic!("minute {minute}: {label:?} {error:?}"),
            Err(StepError::Check { minute, error }) => panic!("minute {minute}: the end-of-tick check: {error:?}"),
        }
        after(loaded, host);
    }
    stepping.checks.asked
}

/// A metric sample as the JSON list of `MetricSnapshot`'s fields in order (`engine::metric_line`).
pub fn metric_line(sample: &MetricSample, minute: i64) -> Vec<u8> {
    engine::metric_line(sample, minute).unwrap()
}
