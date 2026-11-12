//! One tick's phases in order (`experiments/phases/*.py`), calling kernels directly and Python parts
//! through one entry per part (docs/rust-port/lanes/m6-driver.md).
//!
//! `TickEngine.step` asks `controller.run_tick` and, on false, runs the phases through `phase_handler`
//! as without the kernels. This refuses, with Unsupported and before anything has changed, a tick that
//! a hook changes: a benchmark's shadow, a patch of any step or rule it would skip. `install` gives
//! what those are as defined, and they are compared at each tick, so a patch shows there. A trace and
//! a Provider run here: the kernels write the trace's documents, and the Provider's hooks are Python
//! that the kernels call where the phases call them.
//!
//! After that, each step is its kernel. No Python rule stands behind one (lane L10): a kernel's
//! Unsupported is `KernelRefusal`, which names the kernel and gives its reason (`errors::kernel_refusal`),
//! as the step's Python entry (`segments.py`, `kernel_only`) raises it. No borrow is held across a
//! call into Python.

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;

use pyo3::exceptions::PyRuntimeError;
use pyo3::intern;
use pyo3::prelude::*;
use pyo3::sync::GILOnceCell;
use pyo3::types::{PyBool, PyDict, PyInt, PyString, PyTuple, PyType};

use sheltersim_core::rules::tick::{self as core, Seed, ZoneNeighbors};

use crate::core::census;
use crate::core::errors;
use crate::core::row::{attribute, exact_int, unsupported};
use crate::core::run::{run_of, RunCore};
use crate::core::world_core::{raw_dict_get, PyWorldCell};
use crate::host::{HostError, PyHost};
use crate::rules::{dispatch, movement, replan};
use crate::Unsupported;

const MOVEMENT: &str = "movement_v1";

/// What Python gives once.
struct Table {
    controller: Py<PyType>,
    toilet: Py<PyType>,
    resource: Py<PyType>,
    staff: Py<PyType>,
    /// The names of the controller's steps, and each as defined: on the class or on its bases.
    steps: Vec<(Py<PyString>, Py<PyAny>)>,
    /// `(holder, name, as defined)` of what a step calls: a module's function, a manager's method.
    rules: Vec<(Py<PyAny>, Py<PyString>, Py<PyAny>)>,
    /// `TICK_PHASE_ORDER`.
    phases: Vec<Py<PyAny>>,
    clock_error: Py<PyType>,
}

static TABLE: GILOnceCell<Table> = GILOnceCell::new();

static TICKS: AtomicU64 = AtomicU64::new(0);
static KERNELS_REFUSED: AtomicU64 = AtomicU64::new(0);
static REFUSALS: Mutex<BTreeMap<&'static str, u64>> = Mutex::new(BTreeMap::new());
static KERNEL_REASONS: Mutex<BTreeMap<String, u64>> = Mutex::new(BTreeMap::new());

#[track_caller]
fn refuse<T>(reason: &'static str) -> PyResult<T> {
    if let Ok(mut refusals) = REFUSALS.lock() {
        *refusals.entry(reason).or_default() += 1;
    }
    unsupported(reason)
}

/// A kernel's result. Its refusal is counted and becomes `KernelRefusal` naming it (`module.name`).
fn only<T>(py: Python<'_>, label: &str, result: PyResult<T>) -> PyResult<T> {
    result.map_err(|error| {
        if error.is_instance_of::<Unsupported>(py) {
            census::caught(py, label, &error);
            KERNELS_REFUSED.fetch_add(1, Ordering::Relaxed);
            if let Ok(mut reasons) = KERNEL_REASONS.lock() {
                *reasons.entry(error.value(py).to_string()).or_default() += 1;
            }
        }
        errors::kernel_refusal(py, label, error)
    })
}

struct Tick<'a, 'py> {
    table: &'a Table,
    controller: &'a Bound<'py, PyAny>,
    world: &'a Bound<'py, PyAny>,
    minute: &'a Bound<'py, PyAny>,
    minute_value: i64,
    rng: &'a Bound<'py, PyAny>,
}

impl<'py> Tick<'_, 'py> {
    fn py(&self) -> Python<'py> {
        self.world.py()
    }

    fn dispatch(&self) -> PyResult<()> {
        let py = self.py();
        only(py, "dispatch.run_phase", dispatch::run_for_driver(self.controller, self.world, self.minute))
    }

    /// `_update_movement`: the update phase's first step, whose inputs lane P9-1a moves; the core
    /// calls it through `movement` of its phases.
    fn update_movement(&self) -> PyResult<()> {
        let py = self.py();
        let (controller, world, minute) = (self.controller, self.world, self.minute);
        let table = attribute(world, intern!(py, "model_state"))?;
        if let Some(state) = raw_dict_get(&table, intern!(py, MOVEMENT).as_any())?.filter(|state| !state.is_none()) {
            let moved = only(py, "movement.advance_movement", movement::advance_movement(world, minute).map(Bound::into_any))?;
            if moved.is_truthy()? {
                only(py, "replan.replan_journeys", replan::replan_journeys(controller, &state, &moved, minute))?;
            }
        }
        Ok(())
    }

    /// The phases after the dispatch: one call of the core's (`sheltersim_core::rules::tick`).
    fn core_phases(&self, observer: &Bound<'py, PyAny>) -> PyResult<()> {
        let py = self.py();
        let run = run_of(self.controller)?;
        let policies = run.get().policies()?;
        let mut host = PyHost::of_controller(self.controller)?.with_observer(observer).with_rng(self.rng);
        let cell = PyWorldCell(self.world);
        let neighbors = run.get().seen()?.neighbors.clone().ok_or_else(|| PyRuntimeError::new_err("the tick read no neighbors"))?;
        let mut tick = core::Tick { cell: &cell, host: &mut host, run: run.get(), policies, minute: self.minute_value, seed: seed_of(self.rng)?, neighbors };
        let mut movement = |host: &mut PyHost<'py>| -> core::Step<(), HostError> {
            let moved = self.update_movement();
            host.ran_python();
            moved.map_err(|error| core::Failed { label: None, error: HostError(error) })
        };
        core::phases(&mut tick, &mut movement).map_err(|failed| match failed.label {
            Some(label) => match only(py, label, Err::<(), _>(failed.error.0)) {
                Err(error) => error,
                Ok(()) => PyRuntimeError::new_err("a step's error went"),
            },
            None => failed.error.0,
        })
    }

    fn run(&self, observer: &Bound<'py, PyAny>) -> PyResult<()> {
        let py = self.py();
        let watched = !observer.is_none();
        // The dispatch phase, the first of `TICK_PHASE_ORDER`, until lane P9-1c moves it.
        let phase = self.table.phases[0].bind(py);
        if watched {
            observer.call1((phase, true))?;
        }
        self.dispatch()?;
        if !self.world.getattr(intern!(py, "minute_index"))?.eq(self.minute)? {
            return Err(PyErr::from_type(self.table.clock_error.bind(py).clone(), sheltersim_core::error::CLOCK));
        }
        if watched {
            observer.call1((phase, false))?;
        }
        self.core_phases(observer)
    }
}

/// `str(rng.seed)` of the tick's `CounterRNG`, as the core's kernels read it.
fn seed_of(rng: &Bound<'_, PyAny>) -> PyResult<Seed> {
    let py = rng.py();
    if !rng.get_type().is(crate::rules::social_propagation::counter_rng(py)?) {
        return Ok(Seed::Other);
    }
    let seed = rng.getattr(intern!(py, "seed"))?;
    Ok(match seed.downcast_exact::<PyInt>() {
        Ok(number) => Seed::Counter { text: Some(Arc::from(number.str()?.to_str()?)), negative: number.lt(0)? },
        Err(_) => Seed::Counter { text: None, negative: false },
    })
}

/// Why Python runs this tick, before anything has changed.
fn accept(table: &Table, controller: &Bound<'_, PyAny>, world: &Bound<'_, PyAny>) -> PyResult<()> {
    let py = world.py();
    let class = table.controller.bind(py);
    if !controller.get_type().is(class) {
        return refuse("another class of controller");
    }
    if !attribute(controller, intern!(py, "world"))?.is(world) {
        return refuse("the controller is bound to another world");
    }
    let own = controller.getattr(intern!(py, "__dict__"))?;
    for (name, defined) in &table.steps {
        let name = name.bind(py);
        if !class.getattr(name)?.is(defined.bind(py)) {
            return refuse("a step of the controller is patched");
        }
        if raw_dict_get(&own, name.as_any())?.is_some() {
            return refuse("a step of this controller is patched");
        }
    }
    for (holder, name, defined) in &table.rules {
        if !holder.bind(py).getattr(name.bind(py))?.is(defined.bind(py)) {
            return refuse("a rule a step calls is patched");
        }
    }
    // The core arbitrates the managers of the classes installed (policy E10): any other runs the
    // tick through `phase_handler`.
    if !controller.getattr(intern!(py, "toilet_service"))?.get_type().is(table.toilet.bind(py)) {
        return refuse("another class of toilet service");
    }
    let resources = controller.getattr(intern!(py, "resource_service"))?;
    if !resources.is_none() && !resources.get_type().is(table.resource.bind(py)) {
        return refuse("another class of resource service");
    }
    if !controller.getattr(intern!(py, "staff_service"))?.get_type().is(table.staff.bind(py)) {
        return refuse("another class of staff service");
    }
    let Ok(run) = run_of(controller) else {
        return refuse("the controller has no run core");
    };
    // The open actions the core's steps read are the Run's rows: a store that holds a plain dict
    // (a restored Run's records) is read into them first.
    let active = controller.getattr(intern!(py, "active_actions"))?;
    let Ok(active) = active.downcast::<crate::rules::action_lifecycle::ActiveActions>() else {
        return refuse("the open actions are not a store of the Run");
    };
    if active.try_borrow_mut()?.with_rows(py, |_| Ok(())).is_err() {
        return refuse("the open actions do not fit their rows");
    }
    marks(controller, run.get())
}

/// The names of the controller's objects the core's tick reads through the Run (`MARKED`): what
/// they were when the Run last read them is the controller's `_tick_marks`.
const MARKED: [&str; 5] = ["toilet_service", "resource_service", "staff_service", "proximity_contact_config", "zone_neighbors"];

/// A test may put another manager, config or table of neighbors on the controller, or a `request`
/// of its own on a manager: the Run reads its policies (and the neighbors) again then. The first
/// tick of a controller reads them.
fn marks(controller: &Bound<'_, PyAny>, run: &RunCore) -> PyResult<()> {
    let py = controller.py();
    let mut now = Vec::with_capacity(MARKED.len() + 2);
    for name in MARKED {
        now.push(controller.getattr(name)?);
    }
    let mut owns = Vec::with_capacity(2);
    for manager in &now[..2] {
        owns.push(match manager.getattr(intern!(py, "__dict__")) {
            Ok(fields) => raw_dict_get(&fields, intern!(py, "request").as_any())?.is_some(),
            Err(_) => false,
        });
    }
    for own in owns {
        now.push(PyBool::new(py, own).to_owned().into_any());
    }
    let fields = controller.getattr(intern!(py, "__dict__"))?;
    let kept = raw_dict_get(&fields, intern!(py, "_tick_marks").as_any())?;
    let kept = kept.as_ref().and_then(|kept| kept.downcast::<PyTuple>().ok());
    let same = |index: usize| kept.is_some_and(|kept| kept.len() == now.len() && kept.get_item(index).is_ok_and(|item| item.is(&now[index])));
    let policies_same = (0..4).all(same) && same(5) && same(6);
    let neighbors_same = same(4) && run.seen()?.neighbors.is_some();
    if policies_same && neighbors_same {
        return Ok(());
    }
    if !policies_same {
        run.reread_policies(controller)?;
    }
    if !neighbors_same {
        let found = &now[4];
        let social = crate::rules::social_propagation::neighbors_of(found)?;
        let information = found.extract::<HashMap<String, Vec<String>>>().map_err(|_| "the zone neighbors are not str tuples");
        run.seen()?.neighbors = Some(Arc::new(ZoneNeighbors { social, information }));
    }
    fields.set_item(intern!(py, "_tick_marks"), PyTuple::new(py, now)?)
}

/// One tick, or Unsupported with nothing changed. `observer(phase, before)` is `TickEngine.phase_observer`.
#[pyfunction]
fn run_tick<'py>(
    controller: &Bound<'py, PyAny>,
    world: &Bound<'py, PyAny>,
    minute: &Bound<'py, PyAny>,
    rng: &Bound<'py, PyAny>,
    observer: &Bound<'py, PyAny>,
) -> PyResult<()> {
    let py = world.py();
    let Some(table) = TABLE.get(py) else {
        return refuse("experiments/phases/driver.py has not installed the table");
    };
    accept(table, controller, world)?;
    let tick = Tick { table, controller, world, minute, minute_value: exact_int(minute)?, rng };
    TICKS.fetch_add(1, Ordering::Relaxed);
    // The tick has begun, so the Python path must not run over it: a refusal is KernelRefusal.
    tick.run(observer).map_err(|error| errors::kernel_refusal(py, "tick.run_tick", error))
}

/// What `experiments/phases/driver.py` gives once.
#[pyfunction]
#[allow(clippy::too_many_arguments)]
fn install<'py>(
    py: Python<'py>,
    controller: Bound<'py, PyType>,
    managers: (Bound<'py, PyType>, Bound<'py, PyType>, Bound<'py, PyType>),
    steps: Vec<Bound<'py, PyString>>,
    rules: Vec<(Bound<'py, PyAny>, Bound<'py, PyString>)>,
    phases: Vec<Bound<'py, PyAny>>,
    _metrics_recorder: Bound<'py, PyType>,
    clock_error: Bound<'py, PyType>,
) -> PyResult<()> {
    let mut defined_steps = Vec::with_capacity(steps.len());
    for name in steps {
        let defined = controller.getattr(&name)?.unbind();
        defined_steps.push((name.unbind(), defined));
    }
    let mut defined_rules = Vec::with_capacity(rules.len());
    for (holder, name) in rules {
        let defined = holder.getattr(&name)?.unbind();
        defined_rules.push((holder.unbind(), name.unbind(), defined));
    }
    let table = Table {
        controller: controller.unbind(),
        toilet: managers.0.unbind(),
        resource: managers.1.unbind(),
        staff: managers.2.unbind(),
        steps: defined_steps,
        rules: defined_rules,
        phases: phases.into_iter().map(Bound::unbind).collect(),
        clock_error: clock_error.unbind(),
    };
    let _ = TABLE.set(py, table);
    Ok(())
}

/// What has run in this process: `(counts, refused ticks and refused kernels by reason)`.
#[pyfunction]
fn stats<'py>(py: Python<'py>) -> PyResult<(Bound<'py, PyDict>, Bound<'py, PyDict>)> {
    let counts = PyDict::new(py);
    counts.set_item("ticks", TICKS.load(Ordering::Relaxed))?;
    counts.set_item("kernels_refused", KERNELS_REFUSED.load(Ordering::Relaxed))?;
    let reasons = PyDict::new(py);
    if let Ok(refusals) = REFUSALS.lock() {
        for (reason, times) in refusals.iter() {
            reasons.set_item(*reason, *times)?;
        }
    }
    if let Ok(refusals) = KERNEL_REASONS.lock() {
        for (reason, times) in refusals.iter() {
            reasons.set_item(format!("kernel: {reason}"), *times)?;
        }
    }
    Ok((counts, reasons))
}

pub fn register(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_function(wrap_pyfunction!(run_tick, m)?)?;
    m.add_function(wrap_pyfunction!(install, m)?)?;
    m.add_function(wrap_pyfunction!(stats, m)?)?;
    Ok(())
}
