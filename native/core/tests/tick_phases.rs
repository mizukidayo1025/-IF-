//! The core's phases 2 to 6 of a tick (lane P9-2a, `sheltersim_core::rules::tick`): the observer of
//! each phase through the host, in `TICK_PHASE_ORDER`, the movement the caller gives at the start of
//! the update phase, the labels of the steps' errors, the clock's check and the Run's scratch.

mod support;

use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::Arc;

use sheltersim_core::cell::Shard;
use sheltersim_core::error::Error;
use sheltersim_core::host::Phase;
use sheltersim_core::rules::tick::{self, update, Failed, Prior, Seed, Tick, ZoneNeighbors, RUN_TICK};
use sheltersim_core::run::{Run, RunConfig};
use sheltersim_core::world::{Fit, World, WorldCell};

use support::{Call, TestHost};

const MINUTE: i64 = 5;

fn world(minute: i64) -> Shard<World> {
    let mut world = World::default();
    let scalars = world.scalars.get_mut();
    scalars.minute_index = Fit::Ok(minute);
    scalars.duration_minutes = Fit::Ok(60);
    scalars.schema_version = Fit::Ok(6);
    Shard::new(world)
}

fn run() -> RefCell<Run> {
    RefCell::new(Run::new(RunConfig { route_capacities: Err("no capacities") }))
}

fn neighbors() -> Arc<ZoneNeighbors> {
    Arc::new(ZoneNeighbors { social: Ok(HashMap::new()), information: Ok(HashMap::new()) })
}

fn phases_seen(host: &TestHost) -> Vec<(Phase, bool)> {
    host.calls
        .iter()
        .filter_map(|call| match call {
            Call::Phase(phase, before) => Some((*phase, *before)),
            _ => None,
        })
        .collect()
}

#[test]
fn the_phases_start_after_the_dispatch_and_the_update_phase_starts_with_the_movement() {
    let cell = world(MINUTE);
    let run = run();
    run.borrow_mut().scratch.put(Prior::Python { minute: Some(MINUTE) });
    let mut host = TestHost::default();
    let mut moved = Vec::new();
    let failed = {
        let mut tick = Tick {
            cell: &cell,
            host: &mut host,
            run: &run,
            policies: run.borrow().policies.clone(),
            minute: MINUTE,
            seed: Seed::Counter { text: Some(Arc::from("7")), negative: false },
            neighbors: neighbors(),
        };
        let mut movement = |host: &mut TestHost| {
            moved.push(phases_seen(host).len());
            Ok(())
        };
        tick::phases(&mut tick, &mut movement).expect_err("a world without people or policies refuses")
    };
    // The observer saw the second phase whole, then the update phase begin; the movement ran first
    // in it, and the next step failed with its kernel's label (a Run without a controller has no
    // psychology model).
    assert_eq!(
        phases_seen(&host),
        vec![(Phase::CompleteScheduledTasks, true), (Phase::CompleteScheduledTasks, false), (Phase::UpdateAgentState, true)]
    );
    assert_eq!(moved, vec![3]);
    assert!(matches!(failed.label, Some(update::VIEW) | Some(update::PSYCHOLOGY)), "{failed:?}");
    // The tick began: the scratch of the last one is gone, and it is of this minute.
    assert!(run.borrow().scratch.get::<Prior>().is_none());
    assert_eq!(run.borrow().scratch.minute(), Some(MINUTE));
    assert_eq!(&Phase::ORDER[1..], &[
        Phase::CompleteScheduledTasks,
        Phase::UpdateAgentState,
        Phase::ProposeActions,
        Phase::ArbitrateActions,
        Phase::SampleEndogenousEvents,
        Phase::CollectMetrics,
    ]);
}

#[test]
fn an_error_of_the_movement_goes_on_as_the_caller_gave_it() {
    let cell = world(MINUTE);
    let run = run();
    let mut host = TestHost::default();
    let mut tick = Tick { cell: &cell, host: &mut host, run: &run, policies: run.borrow().policies.clone(), minute: MINUTE, seed: Seed::Other, neighbors: neighbors() };
    let mut movement = |_: &mut TestHost| Err(Failed { label: Some("movement.advance_movement"), error: Error::Value("moved".to_owned()) });
    let failed = tick::phases(&mut tick, &mut movement).expect_err("the movement failed");
    assert_eq!(failed.label, Some("movement.advance_movement"));
    assert_eq!(failed.error, Error::Value("moved".to_owned()));
}

#[test]
fn a_phase_that_moves_the_clock_is_the_engine_s_error() {
    let cell = world(MINUTE);
    let run = run();
    let mut host = TestHost::default();
    let tick = Tick { cell: &cell, host: &mut host, run: &run, policies: run.borrow().policies.clone(), minute: MINUTE, seed: Seed::Other, neighbors: neighbors() };
    assert_eq!(tick::clock(&tick), Ok(()));
    cell.with(|world| world.scalars.get_mut().minute_index = Fit::Ok(MINUTE + 1)).unwrap();
    assert_eq!(tick::clock(&tick), Err(Error::Clock));
    assert_eq!(Error::Clock.to_string(), sheltersim_core::error::CLOCK);
    assert_eq!(RUN_TICK, "tick.run_tick");
}
