//! The views of a Run, each with every field (Issue #225, lane M7; moved from
//! `tests/integration/test_world_view_row.py`).
//!
//! The worlds are the generated Scenario the Project screen makes, 120 minutes long, and 60 minutes
//! of it with a rumor from A001 and its correction where every contact passes a claim on
//! (`generated_texts.json`'s `views_run` and `views_claims`, `tests/support/core_fixtures_kernels.py`).
//! The core runs their ticks as `TickEngine.step` does (`support/ticks.rs`), and at each tick
//! takes the update phase's view, which the Run keeps for the propose phase, after that phase, and
//! a new view of the world as the propose phase starts (`support/views.rs`), as the pytest file's
//! observer took them. The digest of their texts (`views::exact`, the sha256 of the lines joined)
//! was recorded from the core in #225 lane M7: the pytest file's digests were of Python's objects
//! (`_exact`, of the views the PyO3 crate made), which the core does not make; on the same kernel
//! they passed (`f1bc3977da99` and `77460590f664`). When they were recorded, each Run's last world
//! here was the one `TickEngine` reaches on the same world (its canonical bytes but `rng_state`).

mod support;

use std::rc::Rc;

use sheltersim_core::host::{Host, Phase};
use sheltersim_core::rules::after_tick::Memo;
use sheltersim_core::rules::dispatch;
use sheltersim_core::rules::tick::{self, propose, Labelled, Step};
use sheltersim_core::world::{Fit, WorldCell};

use support::load::{self, Loaded};
use support::texts::sha;
use support::ticks::{check_after_tick, tick_at, NoPython};
use support::views::{self, exact};
use support::{cases, TestHost};

/// One tick at the world's minute as `tick::run_phases` runs it, with the views taken around the
/// update and the propose phases.
fn tick_watched(loaded: &Loaded, host: &mut TestHost, texts: &mut Vec<String>) -> Step<(), sheltersim_core::error::Error> {
    let minute = loaded.cell.with(|world| world.minute_index().unwrap()).unwrap();
    let mut tick = tick_at(loaded, host, minute);
    tick.on_run(|run| run.scratch.begin(minute)).bare()?;
    for phase in Phase::ORDER {
        tick.host.phase(phase, true).bare()?;
        if phase == Phase::ProposeActions {
            // A new view of the world as the propose phase starts (`view_of(world)`).
            let (data, hash) = views::fresh(loaded, tick.host, true);
            texts.push(exact(&data, hash.as_deref()));
        }
        match phase {
            Phase::DispatchDueEvents => dispatch::run(&mut tick).map_err(|error| propose::refused(dispatch::RUN_PHASE, error)).bare()?,
            Phase::CompleteScheduledTasks => {}
            Phase::UpdateAgentState => tick::update::update(&mut tick)?,
            Phase::ProposeActions => propose::propose(&mut tick)?,
            Phase::ArbitrateActions => tick::arbitrate::arbitrate(&mut tick)?,
            Phase::SampleEndogenousEvents => tick::sample::sample(&mut tick)?,
            Phase::CollectMetrics => tick::metrics::metrics(&mut tick)?,
        }
        tick::clock(&tick).bare()?;
        tick.host.phase(phase, false).bare()?;
        if phase == Phase::UpdateAgentState {
            // The update phase's view, which the Run keeps for the propose phase.
            let kept = views::handed_on(loaded).expect("the update phase's view");
            texts.push(exact(&kept, None));
        }
    }
    Ok(())
}

/// The digest of the views of the world's Run, and how many there were.
fn views_of_a_run(name: &str) -> (String, usize) {
    let entry = cases::entry("generated_texts", name);
    let loaded = Rc::new(load::loaded(&entry));
    let mut host = load::following_host(&entry, &loaded);
    let duration = loaded.cell.with(|world| world.duration_minutes().unwrap()).unwrap();
    let (mut memo, mut checks) = (Memo::default(), NoPython::default());
    let mut texts = Vec::new();
    for minute in 0..duration {
        if let Err(failed) = tick_watched(&loaded, &mut host, &mut texts) {
            panic!("minute {minute}: {:?} {:?}", failed.label, failed.error);
        }
        loaded.cell.with(|world| world.scalars.get_mut().minute_index = Fit::Ok(minute + 1)).unwrap();
        check_after_tick(&loaded, &mut host, &mut memo, &mut checks).unwrap_or_else(|error| panic!("minute {minute}: {error:?}"));
    }
    (sha(&texts.join("\n")), texts.len())
}

#[test]
fn each_view_of_a_run_has_every_field_recorded() {
    let (digest, count) = views_of_a_run("views_run");
    assert_eq!(count, 240);
    assert_eq!(digest, "03a86257334a78fa470f9fb041040c4efc6f32504b35487c51ee4c07d9a0c8ce");
}

#[test]
fn views_with_false_claims_have_every_field_recorded() {
    let (digest, count) = views_of_a_run("views_claims");
    assert_eq!(count, 120);
    assert_eq!(digest, "edd3ebb618e86c3f92832b71ecf034d9db3385245e045dd008bb2660f67c5a15");
}
