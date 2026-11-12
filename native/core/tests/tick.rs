//! A tick of a Run in the core (lanes P9-2a and P9-2b, `sheltersim_core::rules::tick::run_tick`): the
//! Run's scratch begins before the dispatch, the observer sees each phase through the host in
//! `TICK_PHASE_ORDER`, the movement the caller gives runs first in the update phase, the steps'
//! errors carry their labels, the dispatch's refusal is its entry's `KernelRefusal`, and a phase that
//! moves the clock is the engine's error, with its text, before the observer hears the phase end.

mod support;

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Arc;

use indexmap::IndexMap;
use sheltersim_core::cell::Shard;
use sheltersim_core::error::{Error, CLOCK};
use sheltersim_core::events::log::EventLog;
use sheltersim_core::events::queue::Queue;
use sheltersim_core::events::{EventKind, EventRow, EventStatus};
use sheltersim_core::host::{Foreign, Phase};
use sheltersim_core::rules::dispatch::RUN_PHASE;
use sheltersim_core::rules::tick::{self, update, Failed, Prior, Seed, Step, Tick, ZoneNeighbors, RUN_TICK};
use sheltersim_core::run::dispatch::{Dispatch, Handler};
use sheltersim_core::run::{Policies, Run, RunConfig};
use sheltersim_core::value::V;
use sheltersim_core::world::{Fit, World, WorldCell};

use support::{Call, TestHost};

const MINUTE: i64 = 5;

fn text(value: &str) -> Arc<str> {
    Arc::from(value)
}

fn event(id: &str, sequence: i64) -> EventRow {
    let provenance = IndexMap::from([(text("source_type"), V::Str(text("assumption")))]);
    EventRow::new(text(id), EventKind::Endogenous, MINUTE, 0, sequence, text("probe"), IndexMap::new(), Some(text("probe-1")), None, provenance).unwrap()
}

/// A world at `minute` whose queue holds `events` and whose log is empty.
fn world(minute: i64, events: Vec<EventRow>) -> Rc<Shard<World>> {
    let mut world = World::default();
    let scalars = world.scalars.get_mut();
    scalars.minute_index = Fit::Ok(minute);
    scalars.duration_minutes = Fit::Ok(60);
    scalars.schema_version = Fit::Ok(6);
    let mut queue = Queue::new();
    for row in events {
        assert_eq!(queue.schedule_row(row), 0);
    }
    world.hold_queue(Arc::new(queue));
    world.hold_log(Arc::new(EventLog::new()));
    Rc::new(Shard::new(world))
}

/// A Run whose dispatcher gives the `probe` events to the host (a test's handler).
fn run() -> (Rc<RefCell<Run>>, Arc<Policies>) {
    let run = Run::new(RunConfig { route_capacities: Err("no capacities") });
    let mut policies = Policies::clone(&run.policies);
    let handlers: IndexMap<Arc<str>, Handler> = IndexMap::from([(text("probe"), Handler::Foreign)]);
    policies.dispatch = Dispatch { known: Ok(true), handlers: Ok(Arc::new(handlers)) };
    (Rc::new(RefCell::new(run)), Arc::new(policies))
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

/// The host's calls but the reads of what the world holds and of the configs' sections.
fn calls(host: &TestHost) -> Vec<Call> {
    host.calls.iter().filter(|call| !matches!(call, Call::Holding | Call::FillSection(_))).cloned().collect()
}

/// One tick at `MINUTE` with this host and movement.
fn ticked(
    cell: &Shard<World>,
    run: &RefCell<Run>,
    policies: Arc<Policies>,
    host: &mut TestHost,
    movement: &mut dyn FnMut(&mut TestHost) -> Step<(), Error>,
) -> Step<(), Error> {
    let seed = Seed::Counter { text: Some(Arc::from("7")), negative: false };
    let mut tick = Tick { cell, host, run, policies, minute: MINUTE, seed, neighbors: neighbors() };
    tick::run_tick(&mut tick, movement)
}

#[test]
fn the_tick_begins_the_scratch_dispatches_and_starts_the_update_phase_with_the_movement() {
    let cell = world(MINUTE, vec![event("first", 1)]);
    let (run, policies) = run();
    run.borrow_mut().scratch.put(Prior::Python { minute: Some(MINUTE) });
    let mut host = TestHost::default();
    // The handler sees the scratch of this tick: it began before the dispatch.
    let seen = Rc::new(RefCell::new(Vec::new()));
    {
        let (run, seen) = (run.clone(), seen.clone());
        host.foreign = Some(Box::new(move |row: &EventRow| {
            let run = run.borrow();
            seen.borrow_mut().push((row.event_id.clone(), run.scratch.minute(), run.scratch.get::<Prior>().is_none()));
            Ok(Foreign { status: EventStatus::Applied, detail: None, records: Vec::new() })
        }));
    }
    let mut moved = Vec::new();
    let failed = {
        let mut movement = |host: &mut TestHost| {
            moved.push(calls(host).len());
            Ok(())
        };
        ticked(&cell, &run, policies, &mut host, &mut movement).expect_err("a world without people refuses")
    };
    assert_eq!(*seen.borrow(), vec![(text("first"), Some(MINUTE), true)]);
    // The observer saw the dispatch, with the handler inside it, and the second phase whole, then
    // the update phase begin; the movement ran first in it, and the next step failed with its
    // kernel's label (a Run without a controller has no psychology model).
    let expected = vec![
        Call::Phase(Phase::DispatchDueEvents, true),
        Call::Foreign(text("probe")),
        Call::Phase(Phase::DispatchDueEvents, false),
        Call::Phase(Phase::CompleteScheduledTasks, true),
        Call::Phase(Phase::CompleteScheduledTasks, false),
        Call::Phase(Phase::UpdateAgentState, true),
    ];
    let made = calls(&host);
    assert_eq!(&made[..expected.len().min(made.len())], &expected[..], "{made:?}");
    assert_eq!(moved, vec![expected.len()]);
    assert!(matches!(failed.label, Some(update::VIEW) | Some(update::PSYCHOLOGY)), "{failed:?}");
    assert_eq!(run.borrow().scratch.minute(), Some(MINUTE));
    assert_eq!(&Phase::ORDER[..], &[
        Phase::DispatchDueEvents,
        Phase::CompleteScheduledTasks,
        Phase::UpdateAgentState,
        Phase::ProposeActions,
        Phase::ArbitrateActions,
        Phase::SampleEndogenousEvents,
        Phase::CollectMetrics,
    ]);
}

#[test]
fn a_dispatch_that_moves_the_clock_is_the_engine_s_error_before_the_observer_hears_its_end() {
    let cell = world(MINUTE, vec![event("first", 1)]);
    let (run, policies) = run();
    let mut host = TestHost::default();
    {
        let held = cell.clone();
        host.foreign = Some(Box::new(move |_: &EventRow| {
            held.with(|world| world.scalars.get_mut().minute_index = Fit::Ok(MINUTE + 1)).unwrap();
            Ok(Foreign { status: EventStatus::Applied, detail: None, records: Vec::new() })
        }));
    }
    let mut movement = |_: &mut TestHost| -> Step<(), Error> { panic!("the update phase never begins") };
    let failed = ticked(&cell, &run, policies, &mut host, &mut movement).expect_err("the clock moved");
    assert_eq!(failed.label, None);
    assert_eq!(failed.error, Error::Clock);
    assert_eq!(failed.error.to_string(), "tick phase attempted to change the engine-owned clock");
    assert_eq!(CLOCK, "tick phase attempted to change the engine-owned clock");
    assert_eq!(phases_seen(&host), vec![(Phase::DispatchDueEvents, true)]);
}

#[test]
fn a_refusal_of_the_dispatch_is_the_kernel_refusal_of_its_entry() {
    // A world whose queue the core does not hold: the phase refuses before its first event.
    let mut bare = World::default();
    bare.scalars.get_mut().minute_index = Fit::Ok(MINUTE);
    let cell = Shard::new(bare);
    let (run, policies) = run();
    let mut host = TestHost::default();
    let mut movement = |_: &mut TestHost| -> Step<(), Error> { panic!("the update phase never begins") };
    let failed = ticked(&cell, &run, policies, &mut host, &mut movement).expect_err("no queue");
    assert_eq!(failed.label, None);
    match &failed.error {
        Error::Refused { label, .. } => assert_eq!(label, RUN_PHASE),
        other => panic!("{other:?}"),
    }
    assert_eq!(phases_seen(&host), vec![(Phase::DispatchDueEvents, true)]);
}

#[test]
fn an_error_of_the_movement_goes_on_as_the_caller_gave_it() {
    let cell = world(MINUTE, Vec::new());
    let (run, policies) = run();
    let mut host = TestHost::default();
    let mut movement = |_: &mut TestHost| Err(Failed { label: Some("movement.advance_movement"), error: Error::Value("moved".to_owned()) });
    let failed = ticked(&cell, &run, policies, &mut host, &mut movement).expect_err("the movement failed");
    assert_eq!(failed.label, Some("movement.advance_movement"));
    assert_eq!(failed.error, Error::Value("moved".to_owned()));
}

#[test]
fn a_phase_that_moves_the_clock_is_the_engine_s_error() {
    let cell = world(MINUTE, Vec::new());
    let (run, policies) = run();
    let mut host = TestHost::default();
    let tick = Tick { cell: &*cell, host: &mut host, run: &*run, policies, minute: MINUTE, seed: Seed::Other, neighbors: neighbors() };
    assert_eq!(tick::clock(&tick), Ok(()));
    cell.with(|world| world.scalars.get_mut().minute_index = Fit::Ok(MINUTE + 1)).unwrap();
    assert_eq!(tick::clock(&tick), Err(Error::Clock));
    assert_eq!(Error::Clock.to_string(), CLOCK);
    assert_eq!(RUN_TICK, "tick.run_tick");
}
