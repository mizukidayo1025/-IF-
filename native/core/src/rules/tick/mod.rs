//! A tick of a Run in the core (Issue #152, lanes P9-2a and P9-2b; design-p9.md section 5): the
//! dispatch, complete, update, propose, arbitrate, sample and metrics phases of
//! `experiments/phases/*.py`, in `TICK_PHASE_ORDER`, over the Run's `Scratch`, with the observer of
//! the phases through the host (`Host::phase`) and the engine's clock checked after each phase.
//!
//! `run_tick` is the whole tick. The PyO3 crate's driver (`native/src/rules/tick.rs`) checks, before
//! anything has changed, that no hook changes the tick and that the dispatch can begin (`accept`,
//! policy E10), reads the tick's random source (H7), and then calls `run_tick` once with one native
//! host. Each step is the core's kernel, called directly; what a kernel reads besides the world comes
//! from the host (what the world holds, H1; the configs' sections, H2; the floor plan; the queue's
//! rows, H6) and from the Run's `Policies`, read when the controller starts. Only the update phase's
//! movement and replanning are still the PyO3 crate's: the caller gives them as `movement`.
//!
//! Each step's error carries the label of the kernel's Python entry (`module.name`), which the PyO3
//! crate raises as that entry's `KernelRefusal` where it is a refusal (`Failed`), as the steps did
//! when the driver called their entries. The dispatch phase's refusal is `KernelRefusal` of
//! `dispatch.run_phase` already (`Error::Refused`), as the phase's entry raises it. No borrow of the
//! world, and no lock of the Run, is held while the host runs (rule R6), and a step reads again what
//! a host call may have changed.

pub mod arbitrate;
pub mod metrics;
pub mod propose;
pub mod read;
pub mod sample;
pub mod update;

use std::sync::{Arc, Mutex};

use crate::error::{Error, Result};
use crate::events::writer::{WriteError, WriteResult};
use crate::host::{Again, Host, Phase};
use crate::rules::world_view::ViewData;
use crate::run::{Policies, Run};
use crate::world::WorldCell;

/// The label of an error no step names: the driver's own (`errors::kernel_refusal(py,
/// "tick.run_tick", error)` of the PyO3 crate), which is not counted as a kernel's refusal.
pub const RUN_TICK: &str = "tick.run_tick";

/// The Run of a tick, locked only while `f` runs: `f` reaches neither the host nor the world's
/// Python views.
pub trait RunCell {
    fn with<R>(&self, f: impl FnOnce(&mut Run) -> R) -> Result<R>;
}

impl RunCell for Mutex<Run> {
    fn with<R>(&self, f: impl FnOnce(&mut Run) -> R) -> Result<R> {
        let mut run = self.lock().map_err(|_| Error::unsupported("the run is poisoned"))?;
        Ok(f(&mut run))
    }
}

impl RunCell for std::cell::RefCell<Run> {
    fn with<R>(&self, f: impl FnOnce(&mut Run) -> R) -> Result<R> {
        let mut run = self.try_borrow_mut().map_err(|_| Error::Runtime("the run is borrowed already".to_owned()))?;
        Ok(f(&mut run))
    }
}

/// A step's error, with the label of its kernel: None for the driver's own.
#[derive(Debug)]
pub struct Failed<E> {
    pub label: Option<&'static str>,
    pub error: E,
}

/// What a step gives.
pub type Step<T, E> = std::result::Result<T, Failed<E>>;

/// A result, as a step's.
pub trait Labelled<T, E> {
    /// The error is the kernel's of this label.
    fn at(self, label: &'static str) -> Step<T, E>;
    /// The error is the driver's own.
    fn bare(self) -> Step<T, E>;
}

impl<T, E> Labelled<T, E> for std::result::Result<T, E> {
    fn at(self, label: &'static str) -> Step<T, E> {
        self.map_err(|error| Failed { label: Some(label), error })
    }

    fn bare(self) -> Step<T, E> {
        self.map_err(|error| Failed { label: None, error })
    }
}

/// The tick's random source as the kernels read it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Seed {
    /// `CounterRNG`: `str(seed)` of an exact int seed (None for another value), and whether it is
    /// below 0.
    Counter { text: Option<Arc<str>>, negative: bool },
    /// Another `KeyedRandom` (a test's): only the toilet draws from it, through the host.
    Other,
}

/// The view the update phase hands on to the propose phase (`_view_before_proposals`, decision D4
/// of design-p9.md): the update phase's snapshot, or what Python set there.
#[derive(Clone)]
pub enum Prior {
    View(Arc<ViewData>),
    /// A view Python made, with its `minute_index` where it is an int.
    Python { minute: Option<i64> },
}

/// `controller.zone_neighbors`, as the social model and the information model read it: the PyO3
/// crate reads it when its identity changes (it is the controller's, not a policy the Run reads).
#[derive(Clone, Debug)]
pub struct ZoneNeighbors {
    pub social: crate::rules::social_propagation::Neighbors,
    pub information: std::result::Result<std::collections::HashMap<String, Vec<String>>, &'static str>,
}

/// One tick of a Run, as the phases see it.
pub struct Tick<'a, C: WorldCell, H: Host, R: RunCell> {
    pub cell: &'a C,
    pub host: &'a mut H,
    pub run: &'a R,
    /// The Run's policies, read when the controller started.
    pub policies: Arc<Policies>,
    pub minute: i64,
    pub seed: Seed,
    pub neighbors: Arc<ZoneNeighbors>,
}

impl<C: WorldCell, H: Host, R: RunCell> Tick<'_, C, H, R> {
    /// A core error as the host's.
    pub fn core<T>(&self, result: Result<T>) -> std::result::Result<T, H::Error> {
        result.map_err(H::Error::from)
    }

    /// A write's result as the host's.
    pub fn written<T>(&self, result: WriteResult<T>) -> std::result::Result<T, H::Error> {
        result.map_err(|error| self.host.write_error(error))
    }

    /// Run f on the world.
    pub fn world<T>(&self, f: impl FnOnce(&mut crate::world::World) -> Result<T>) -> std::result::Result<T, H::Error> {
        self.cell.with(f).and_then(|done| done).map_err(H::Error::from)
    }

    /// Run f on the Run.
    pub fn on_run<T>(&self, f: impl FnOnce(&mut Run) -> T) -> std::result::Result<T, H::Error> {
        self.run.with(f).map_err(H::Error::from)
    }

    /// The error of a policy whose read raised when the Run read it, at `path` of the controller
    /// (`policy_error` of the PyO3 crate's psychology glue, policy E14): its refusal, or what reading
    /// it again raises.
    pub fn policy_error(&mut self, path: &[&str], fault: &crate::config::Fault) -> H::Error {
        let fault = crate::rules::psychology::PolicyFault { path: Vec::new(), fault: fault.clone() };
        if let Some(text) = fault.refusal() {
            return Error::unsupported(text.to_owned()).into();
        }
        match self.host.read_again(Again::Policy(path)) {
            Err(error) => error,
            Ok(()) => Error::unsupported(fault.fault.text.to_string()).into(),
        }
    }

    /// The error of what a core reader of the policies could not make (`bad_error`).
    pub fn bad(&mut self, bad: crate::rules::psychology::Bad) -> H::Error {
        match bad {
            crate::rules::psychology::Bad::Refused(error) => error.into(),
            crate::rules::psychology::Bad::Policy(fault) => {
                let path = fault.path.clone();
                self.policy_error(&path, &fault.fault)
            }
        }
    }
}

/// One tick at `tick.minute`: the Run's scratch begins, then each phase in its order
/// (`TICK_PHASE_ORDER`): the observer before, the phase, the check that the clock is the engine's,
/// and the observer after. `movement` is the update phase's movement and replanning, which the caller
/// runs.
pub fn run_tick<C: WorldCell, H: Host, R: RunCell>(
    tick: &mut Tick<'_, C, H, R>,
    movement: &mut dyn FnMut(&mut H) -> Step<(), H::Error>,
) -> Step<(), H::Error>
where
    H::Error: From<WriteError>,
{
    tick.on_run(|run| run.scratch.begin(tick.minute)).bare()?;
    for phase in Phase::ORDER {
        tick.host.phase(phase, true).bare()?;
        match phase {
            // A refusal past the dispatch's checks is `KernelRefusal` of its entry, as
            // `_run_dispatch_phase` raises it, and not counted as a kernel's.
            Phase::DispatchDueEvents => crate::rules::dispatch::run(tick).map_err(|error| propose::refused(crate::rules::dispatch::RUN_PHASE, error)).bare()?,
            // `complete_scheduled_tasks` has no step in the controller.
            Phase::CompleteScheduledTasks => {}
            Phase::UpdateAgentState => update::update(tick, movement)?,
            Phase::ProposeActions => propose::propose(tick)?,
            Phase::ArbitrateActions => arbitrate::arbitrate(tick)?,
            Phase::SampleEndogenousEvents => sample::sample(tick)?,
            Phase::CollectMetrics => metrics::metrics(tick)?,
        }
        clock(tick).bare()?;
        tick.host.phase(phase, false).bare()?;
    }
    Ok(())
}

/// `TickExecutionError` where a phase moved the world's minute (`TickEngine.step`).
pub fn clock<C: WorldCell, H: Host, R: RunCell>(tick: &Tick<'_, C, H, R>) -> std::result::Result<(), H::Error> {
    let now = tick.cell.with(|world| world.minute_index()).map_err(H::Error::from)?;
    match now {
        Ok(minute) if minute == tick.minute => Ok(()),
        _ => Err(Error::Clock.into()),
    }
}
