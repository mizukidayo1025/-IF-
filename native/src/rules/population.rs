//! `domain/population.py`: the glue of the core's kernels (`sheltersim_core::rules::population`,
//! lane c5 of Issue #152). Each entry reads its arguments and what Python holds (whether
//! `model_state` holds each namespace, the sections of the configs, the floor plan, the approach
//! routes), runs the core on the world borrowed whole, and makes the Python values of what it
//! returns. The core writes the rows; an admitted person is a new row of `agents`, whose object
//! the entity store makes from it, and the attached trees follow their rows when the world is
//! given back. The contracts of the inputs go through the native host (`Host::contract`, policy
//! E15). What stays here: putting a new `population_dynamics_v1` into `model_state` (a new slot),
//! and letting go the Python objects of the events a departure settled (`QueueStore::forget`).
//!
//! The helpers past the glue are what `rules/population_lifecycle.rs` still reads through Python
//! (lane c7 moves them with that file).

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use indexmap::IndexMap;
use pyo3::exceptions::PyRuntimeError;
use pyo3::intern;
use pyo3::prelude::*;
use pyo3::types::{PyDict, PyFloat, PyInt, PyList, PySlice, PyString, PyTuple};

use sheltersim_core::config::agent_dynamics::{awareness, policy as config_policy};
use sheltersim_core::config::overlay::{overlay_schema as parse_overlay_schema, OverlayRooms};
use sheltersim_core::error::Error as CoreError;
use sheltersim_core::rules::kit::{self, Held};
use sheltersim_core::rules::population::{
    self as core, Alternates, ArrivalArgs, Built, DepartureArgs, Departures, Inputs, Language, Namespaces, Overrides, RoutesFault, PLAN_UNREAD,
};
use sheltersim_core::value::V as CoreV;

use crate::core::configs;
use crate::core::errors::{self, to_py_err, OrPy};
use crate::core::row::{attribute, exact_dict, exact_int, exact_str, unsupported};
use crate::core::value::{V, VPy};
use crate::core::world_core::{attached_state, namespace_held, state_value, with_state_value, with_world, PyWorldCell};
use crate::host::PyHost;
use crate::rules::movement::mirror_held;
use crate::rules::movement_geometry::{approach_routes, Geo, GeoPy, Pt};
use crate::state::entities::{entities_of, Activity, EntityStore};
use crate::state::events::{read_frozen, EventRow, EventRowPy, EventStatus, EventWriter, MAX_ROW_DEPTH};
use crate::state::movement::MovementRow;
use crate::{Encoder, Unsupported};
use sha2::{Digest, Sha256};

pub(crate) use sheltersim_core::rules::kit::{DYNAMICS, PRESENCE};
pub(crate) const MOVEMENT: &str = "movement_v1";
pub(crate) const INFORMATION: &str = "information_spread_v1";
pub(crate) const BODY: &str = "body_state_v1";
pub(crate) const AGENT_DYNAMICS: &str = "agent_dynamics_v1";
pub(crate) const SOCIAL_NETWORK: &str = "social_network_v1";
pub(crate) const SOCIAL_RUNTIME: &str = "social_propagation_v1";
pub(crate) const OVERLAY: &str = "geometry_overlay_v1";
const ROOM_INTAKE_SOURCE: &str = "ASM-TEMPORARY-ROOM-INTAKE-V1";
const MAX_PRESENCE_HISTORY: usize = 20_000;

// ---- The two stages ------------------------------------------------------------------------------

/// A plan's result: any error of what it read means the Python rule has something to say. An
/// outcome the plan raised with the rule's text (`core/errors.rs`) goes on as it is.
pub(crate) fn planning<T>(result: PyResult<T>) -> PyResult<T> {
    result.map_err(|error| {
        Python::with_gil(|py| if error.is_instance_of::<Unsupported>(py) || errors::made_here(py, &error) { error } else { Unsupported::new_err(format!("the plan read: {error}")) })
    })
}

/// A write stage's result: the Python rule must not run after a write, so a refusal here is a bug
/// to see. Every other error is what the same operation raises in the Python rule.
/// The core's kernels use `sheltersim_core::rules::kit::writing`, which writes the same text.
pub(crate) fn writing<T>(result: PyResult<T>) -> PyResult<T> {
    result.map_err(|error| Python::with_gil(|py| if error.is_instance_of::<Unsupported>(py) { PyRuntimeError::new_err(format!("{}: {error}", kit::WRITING_REFUSED)) } else { error }))
}

// ---- The glue of the core's kernels ----------------------------------------------------------------

/// A Python error read before the core runs, as the plan says it: a refusal stays itself, and
/// any other error is what `planning` makes of it (`the plan read: ...`). Built without counting:
/// the core raises it only where the rule reads what raised.
fn plan_read(py: Python<'_>, error: &PyErr) -> CoreError {
    if error.is_instance_of::<Unsupported>(py) {
        CoreError::Unsupported(error.value(py).to_string().into())
    } else {
        CoreError::Unsupported(format!("the plan read: {error}").into())
    }
}

fn held(world: &Bound<'_, PyAny>, name: &str, other: &'static str) -> PyResult<Held> {
    namespace_held(world, &PyString::intern(world.py(), name), other)
}

fn namespaces_of(world: &Bound<'_, PyAny>) -> PyResult<Namespaces> {
    Ok(Namespaces {
        dynamics: held(world, DYNAMICS, "the world holds another population dynamics state")?,
        presence: held(world, PRESENCE, "the world holds another presence state")?,
        information: held(world, INFORMATION, "the world holds another information state")?,
        body: held(world, BODY, "the world holds another body state")?,
        agent_dynamics: held(world, AGENT_DYNAMICS, "the world holds another agent dynamics state")?,
        movement: held(world, MOVEMENT, "the world holds another movement state")?,
        overlay: held(world, OVERLAY, "the world holds another overlay state")?,
        spaces: held(world, core::SPACES, "the world holds another temporary spaces state")?,
        social_network: held(world, SOCIAL_NETWORK, "the world holds another social network state")?,
        social_runtime: held(world, SOCIAL_RUNTIME, "the world holds another social propagation state")?,
        resource_service: held(world, "resource_service_v1", "the world holds another resource service state")?,
        effect_program: held(world, "effect_program_v1", "the world holds another effect program state")?,
    })
}

/// What the core's rules read of the Python world, and what reading the floor plan raised.
struct PopulationIn {
    inputs: Inputs,
    geo_error: Option<PyErr>,
}

impl PopulationIn {
    /// `routes`: whether the rule reads the approach routes.
    fn of(world: &Bound<'_, PyAny>, routes: bool) -> PyResult<Self> {
        let py = world.py();
        let namespaces = namespaces_of(world)?;
        let schema = configs::kept_section(world, |configs| &configs.overlay_schema, parse_overlay_schema)?;
        let policy = configs::kept_section(world, |configs| &configs.agent_dynamics_policy, config_policy)?;
        let awareness = configs::kept_section(world, |configs| &configs.facility_awareness, awareness)?;
        // The floor plan is read only where a rule can reach it: with movement or the overlay.
        let (geo, geo_error) = if matches!(namespaces.movement, Held::Absent) && matches!(namespaces.overlay, Held::Absent) {
            (None, None)
        } else {
            match Geo::of(world) {
                Ok(geo) => (Some(geo), None),
                Err(error) => (None, Some(error)),
            }
        };
        let routes = routes.then(|| {
            approach_routes(world).map_err(|error| RoutesFault {
                caught: !error.is_instance_of::<Unsupported>(py)
                    && (error.is_instance_of::<pyo3::exceptions::PyTypeError>(py) || error.is_instance_of::<pyo3::exceptions::PyValueError>(py)),
                refusal: plan_read(py, &error),
            })
        });
        Ok(Self { inputs: Inputs { namespaces, schema, policy, awareness, geo, routes }, geo_error })
    }

    /// The core's result, with what reading the floor plan raised where the core read the plan, as
    /// the plan says it.
    fn finish<T>(&self, py: Python<'_>, result: PyResult<T>) -> PyResult<T> {
        match (result, &self.geo_error) {
            (Err(error), Some(read)) if error.is_instance_of::<Unsupported>(py) && error.value(py).to_string() == PLAN_UNREAD => {
                Err(to_py_err(plan_read(py, read)))
            }
            (result, _) => result,
        }
    }

    fn run<T>(&self, world: &Bound<'_, PyAny>, kernel: impl FnOnce(&sheltersim_core::world::World, &Inputs) -> sheltersim_core::error::Result<T>) -> PyResult<T> {
        let result = with_world(world, |cell| kernel(cell, &self.inputs))?.or_py();
        self.finish(world.py(), result)
    }
}

/// Makes a row of each pending event the queue holds as a Python object, for a rule that reads
/// the pending events as rows: an object that is no row stays as it is, and the rule refuses.
fn fill_rows(world: &Bound<'_, PyAny>) -> PyResult<()> {
    crate::rules::travel::read_queue_rows(world)
}

/// A kernel that reads the pending events as rows: the rows are made before it runs, as the
/// rules of travel make them, so that the core refuses (`NO_ROW`) only for an object that is no
/// row. Running the kernel first and again after its refusal counted that refusal in the census.
fn with_rows<T>(world: &Bound<'_, PyAny>, kernel: impl FnOnce() -> PyResult<T>) -> PyResult<T> {
    fill_rows(world)?;
    kernel()
}

/// `model_state[DYNAMICS] = state`, the first write of a rule whose plan made the state.
fn install_dynamics(world: &Bound<'_, PyAny>, state: &CoreV, inputs: &mut Inputs) -> PyResult<()> {
    let py = world.py();
    attribute(world, intern!(py, "model_state"))?.set_item(DYNAMICS, state.to_py(py)?)?;
    inputs.namespaces.dynamics = held(world, DYNAMICS, "the world holds another population dynamics state")?;
    Ok(())
}

/// `model_configs.get("population")` as a value, for a plan that may make the state: None for none.
fn population_config(world: &Bound<'_, PyAny>, dynamics: &Held) -> PyResult<Option<CoreV>> {
    if !matches!(dynamics, Held::Absent) {
        return Ok(None);
    }
    let raw = get(&attribute(world, intern!(world.py(), "model_configs"))?, "population")?;
    if raw.is_none() {
        return Ok(None);
    }
    Ok(Some(<CoreV as VPy>::from_py(&raw)?))
}

/// A Python value read as an argument: its refusal is raised where the rule reads it.
fn deferred<T>(py: Python<'_>, result: PyResult<T>) -> sheltersim_core::error::Result<T> {
    result.map_err(|error| plan_read(py, &error))
}

/// A zone given to a rule: its text, or None for a value no zone has.
fn zone_arg(value: &Bound<'_, PyAny>) -> PyResult<Option<Arc<str>>> {
    if value.is_instance_of::<PyString>() {
        return text(value).map(Some);
    }
    // `value in world.zones` hashes it.
    value.hash()?;
    Ok(None)
}

/// `admission_zone_ids` of `queue_arrivals`, as the rule's first checks of it read it.
fn alternates_arg(given: Option<&Bound<'_, PyAny>>) -> PyResult<Alternates> {
    let Some(given) = given else {
        return Ok(Alternates::NotGiven);
    };
    if !(given.is_exact_instance_of::<PyTuple>() || given.is_exact_instance_of::<PyList>()) {
        if given.is_instance_of::<PyTuple>() || given.is_instance_of::<PyList>() {
            return unsupported("the alternate zones are a subclass of list or tuple");
        }
        return Ok(Alternates::Invalid);
    }
    let items: Vec<Bound<'_, PyAny>> = given.try_iter()?.collect::<PyResult<_>>()?;
    if items.iter().any(|item| !item.is_instance_of::<PyString>()) {
        return Ok(Alternates::Invalid);
    }
    Ok(Alternates::Zones(items.iter().map(text).collect::<PyResult<_>>()?))
}

const MOBILITIES: [&str; 3] = ["independent", "assisted", "wheelchair"];

/// `Mobility(value)`: Some(the value) for a member's value, None where the enum raises ValueError. A
/// value of another type than these, whose `==` or repr the enum would call, is outside the contract.
fn mobility_of(value: &Bound<'_, PyAny>) -> PyResult<Option<&'static str>> {
    if value.is_instance_of::<PyString>() {
        let member = errors::member_of(value, &MOBILITIES)?;
        return Ok(member.and_then(|text| MOBILITIES.iter().copied().find(|member| *member == text)));
    }
    // A list, tuple or dict is no member either: the enum's lookup falls back to `==`, then raises.
    if value.is_none()
        || value.is_exact_instance_of::<pyo3::types::PyBool>()
        || value.is_exact_instance_of::<PyInt>()
        || value.is_exact_instance_of::<PyFloat>()
        || value.is_exact_instance_of::<PyList>()
        || value.is_exact_instance_of::<PyTuple>()
        || value.is_exact_instance_of::<PyDict>()
    {
        return Ok(None);
    }
    unsupported("a mobility of another type")
}

/// A value given from a payload (frozen: tuples and mapping proxies) or a call, as a `V`.
fn arg_value(value: &Bound<'_, PyAny>) -> PyResult<CoreV> {
    if let Ok(plain) = <CoreV as VPy>::from_py(value) {
        return Ok(plain);
    }
    match read_frozen(value, MAX_ROW_DEPTH)? {
        Some(frozen) => Ok(frozen),
        None => unsupported("an argument is not a plain value"),
    }
}

fn language_arg(value: &Bound<'_, PyAny>) -> PyResult<Language> {
    match errors::str_of(value)?.filter(|text| is_identifier(text)) {
        Some(language) => Ok(Language::Valid(Arc::from(language))),
        None => Ok(Language::Ask(arg_value(value)?)),
    }
}

/// `person_context_overrides`: its items, or `Caught` where iterating it raised a `TypeError` or
/// `ValueError`.
fn overrides_arg(value: &Bound<'_, PyAny>) -> PyResult<Overrides> {
    let py = value.py();
    let caught = |error: &PyErr| {
        !error.is_instance_of::<Unsupported>(py)
            && (error.is_instance_of::<pyo3::exceptions::PyTypeError>(py) || error.is_instance_of::<pyo3::exceptions::PyValueError>(py))
    };
    let iterator = match value.try_iter() {
        Ok(iterator) => iterator,
        Err(error) if caught(&error) => return Ok(Overrides::Caught),
        Err(error) => return Err(error),
    };
    let mut items = Vec::new();
    for item in iterator {
        match item {
            Ok(item) => items.push(arg_value(&item)?),
            Err(error) if caught(&error) => return Ok(Overrides::Caught),
            Err(error) => return Err(error),
        }
    }
    Ok(Overrides::Items(items))
}

/// The optional arguments of `queue_arrivals` as a call gives them: None for one not given (the
/// rule's default), Some for one given, None included.
#[derive(Default)]
pub(crate) struct ArrivalOptions<'py> {
    pub(crate) household_size: Option<Bound<'py, PyAny>>,
    pub(crate) mobility: Option<Bound<'py, PyAny>>,
    pub(crate) language: Option<Bound<'py, PyAny>>,
    pub(crate) admission_zone_ids: Option<Bound<'py, PyAny>>,
    pub(crate) approach_route_id: Option<Bound<'py, PyAny>>,
    pub(crate) person_context_overrides: Option<Bound<'py, PyAny>>,
}

impl<'py> ArrivalOptions<'py> {
    fn of(options: Option<&Bound<'py, PyDict>>) -> PyResult<Self> {
        let mut parsed = Self::default();
        let Some(options) = options else {
            return Ok(parsed);
        };
        for (key, value) in options.iter() {
            let slot = match exact_str(&key)? {
                "household_size" => &mut parsed.household_size,
                "mobility" => &mut parsed.mobility,
                "language" => &mut parsed.language,
                "admission_zone_ids" => &mut parsed.admission_zone_ids,
                "approach_route_id" => &mut parsed.approach_route_id,
                "person_context_overrides" => &mut parsed.person_context_overrides,
                _ => return unsupported("an argument the rule does not take"),
            };
            *slot = Some(value);
        }
        Ok(parsed)
    }
}

#[pyfunction]
#[pyo3(signature = (world, *, event, zone_id, count, **options))]
pub(crate) fn queue_arrivals<'py>(
    world: &Bound<'py, PyAny>,
    event: &Bound<'py, PyAny>,
    zone_id: &Bound<'py, PyAny>,
    count: &Bound<'py, PyAny>,
    options: Option<&Bound<'py, PyDict>>,
) -> PyResult<i64> {
    let options = ArrivalOptions::of(options)?;
    arrivals(world, event, zone_id, count, &options)
}

/// `queue_arrivals`.
pub(crate) fn arrivals<'py>(world: &Bound<'py, PyAny>, event: &Bound<'py, PyAny>, zone_id: &Bound<'py, PyAny>, count: &Bound<'py, PyAny>, options: &ArrivalOptions<'py>) -> PyResult<i64> {
    let py = world.py();
    let (mut read, plan) = planning((|| {
        let row = event_row(event)?;
        let route_id = match options.approach_route_id.as_ref().filter(|route| !route.is_none()) {
            None => None,
            Some(route) if route.is_instance_of::<PyString>() => Some(Some(Arc::<str>::from(route.extract::<String>()?))),
            Some(_) => Some(None),
        };
        let read = PopulationIn::of(world, route_id.is_some())?;
        let rooms = configs::kept_python(world, |configs| &configs.overlay_rooms, overlay_rooms)?;
        let schema = if rooms.is_dict { deferred(py, rooms_schema(world, &rooms)) } else { Ok(false) };
        let args = ArrivalArgs {
            zone: deferred(py, zone_arg(zone_id)),
            alternates: deferred(py, alternates_arg(options.admission_zone_ids.as_ref())),
            route_id,
            count: deferred(py, errors::int_not_bool(count)),
            household_size: options.household_size.as_ref().map(|size| deferred(py, errors::int_not_bool(size))),
            mobility: options.mobility.as_ref().map(|mobility| deferred(py, mobility_of(mobility))),
            language: options.language.as_ref().map(|language| deferred(py, language_arg(language))),
            overrides: options.person_context_overrides.as_ref().map(|overrides| deferred(py, overrides_arg(overrides))),
            rooms: (rooms.is_dict, schema),
            population: population_config(world, &read.inputs.namespaces.dynamics)?,
        };
        let mut host = PyHost::of_world(world, PyDict::new(py));
        let plan = core::plan_arrivals(&PyWorldCell(world), &mut host, &read.inputs, &row, args).map_err(|error| error.0);
        let plan = read.finish(py, plan)?;
        Ok((read, plan))
    })())?;
    if let Some(state) = &plan.new_state {
        install_dynamics(world, state, &mut read.inputs)?;
    }
    if let Some(text) = &plan.fails {
        return errors::failed(py, text.clone());
    }
    writing(read.run(world, |cell, inputs| core::write_arrivals(cell, inputs, &plan)))
}

#[pyfunction]
pub(crate) fn admit_pending_arrivals<'py>(world: &Bound<'py, PyAny>, minute: &Bound<'py, PyAny>) -> PyResult<Bound<'py, PyTuple>> {
    let minute = exact_int(minute)?;
    admit_inner(world, minute)
}

/// `admit_pending_arrivals`: nothing is read past the namespace while no one waits.
fn admit_inner<'py>(world: &Bound<'py, PyAny>, minute: i64) -> PyResult<Bound<'py, PyTuple>> {
    let py = world.py();
    let dynamics = held(world, DYNAMICS, "the world holds another population dynamics state")?;
    if with_world(world, |cell| core::nothing_waiting(cell, &dynamics))?.or_py()? {
        return Ok(PyTuple::empty(py));
    }
    let read = PopulationIn::of(world, false)?;
    let admitted = read.run(world, |cell, inputs| core::admit_pending(cell, inputs, minute))?;
    PyTuple::new(py, admitted.iter().map(|id| &**id))
}

#[pyfunction]
pub(crate) fn apply_arrival_approach_completion(world: &Bound<'_, PyAny>, event: &Bound<'_, PyAny>) -> PyResult<()> {
    let row = event_row(event)?;
    let read = PopulationIn::of(world, true)?;
    read.run(world, |cell, inputs| core::apply_approach_completion(cell, inputs, &row))
}

#[pyfunction]
#[pyo3(signature = (world, event, *, id_prefix="departure-retry", subject="departure"))]
pub(crate) fn schedule_population_retry(world: &Bound<'_, PyAny>, event: &Bound<'_, PyAny>, id_prefix: &str, subject: &str) -> PyResult<()> {
    let row = event_row(event)?;
    let program = held(world, "effect_program_v1", "the world holds another effect program state")?;
    with_world(world, |cell| core::schedule_retry(cell, &program, &row, id_prefix, subject))?.or_py()
}

#[pyfunction]
#[pyo3(signature = (world, *, event, zone_id, count, **options))]
pub(crate) fn queue_departures<'py>(
    world: &Bound<'py, PyAny>,
    event: &Bound<'py, PyAny>,
    zone_id: &Bound<'py, PyAny>,
    count: &Bound<'py, PyAny>,
    options: Option<&Bound<'py, PyDict>>,
) -> PyResult<usize> {
    let mut include_initial = None;
    if let Some(options) = options {
        for (key, value) in options.iter() {
            if exact_str(&key)? != "include_initial" {
                return unsupported("an argument the rule does not take");
            }
            include_initial = Some(value);
        }
    }
    departures(world, event, zone_id, count, include_initial.as_ref())
}

/// `queue_departures`. `include_initial` is None when the call doesn't give it.
pub(crate) fn departures<'py>(
    world: &Bound<'py, PyAny>,
    event: &Bound<'py, PyAny>,
    zone_id: &Bound<'py, PyAny>,
    count: &Bound<'py, PyAny>,
    include_initial: Option<&Bound<'py, PyAny>>,
) -> PyResult<usize> {
    let py = world.py();
    let row = planning(event_row(event))?;
    let (mut read, planned) = planning((|| {
        let read = PopulationIn::of(world, false)?;
        let planned = with_rows(world, || {
            let args = DepartureArgs {
                zone: deferred(py, zone_arg(zone_id)),
                count: deferred(py, errors::int_not_bool(count)),
                include_initial: include_initial.map(|flag| flag.downcast_exact::<pyo3::types::PyBool>().ok().map(|flag| flag.is_true())),
                population: population_config(world, &read.inputs.namespaces.dynamics)?,
            };
            let mut host = PyHost::of_world(world, PyDict::new(py));
            let planned = core::plan_departures(&PyWorldCell(world), &mut host, &read.inputs, &row, args).map_err(|error| error.0);
            read.finish(py, planned)
        })?;
        Ok((read, planned))
    })())?;
    let selected = match planned {
        Departures::Fails { new_state, text } => {
            if let Some(state) = &new_state {
                install_dynamics(world, state, &mut read.inputs)?;
            }
            return errors::failed(py, text);
        }
        Departures::Busy { new_state } => {
            // Waits for their active tasks: the retry and the suppression of the rule. `_state` has run.
            let program = read.inputs.namespaces.effect_program.clone();
            let plan = with_world(world, |cell| core::plan_retry(cell, &program, &row, "departure-retry", "departure").map(|plan| plan.map(drop)))?.or_py()?;
            if let Some(state) = &new_state {
                install_dynamics(world, state, &mut read.inputs)?;
            }
            match plan {
                Ok(()) => schedule_population_retry(world, event, "departure-retry", "departure")?,
                Err(text) => return errors::suppressed(py, text),
            }
            return errors::suppressed(py, "departure waits for the selected residents' active tasks".to_owned());
        }
        Departures::Leave { new_state, selected } => {
            if let Some(state) = &new_state {
                writing(install_dynamics(world, state, &mut read.inputs))?;
            }
            selected
        }
    };
    let queue = EventWriter::of(world)?.queue().clone();
    let taken = writing(read.run(world, |cell, inputs| core::write_departures(cell, inputs, &row, &selected)))?;
    // The objects of the events the departures settled on the world's queue.
    writing(queue.get().forget(&taken))?;
    let minute = exact_int(&attribute(world, intern!(py, "minute_index"))?)?;
    // Written, so nothing can fall back from here: the plan checked the waiting.
    writing(admit_inner(world, minute))?;
    Ok(selected.len())
}

/// What `admit_pending_arrivals` asks of the waiting, whoever gets a place. A rule that writes and
/// then admits asks this first: which of them get a place depends on the writes, and it can't
/// refuse then.
pub(crate) fn check_waiting_for_admission(world: &Bound<'_, PyAny>) -> PyResult<()> {
    let dynamics = held(world, DYNAMICS, "the world holds another population dynamics state")?;
    if with_world(world, |cell| core::nothing_waiting(cell, &dynamics))?.or_py()? {
        return Ok(());
    }
    let read = PopulationIn::of(world, false)?;
    read.run(world, core::check_waiting_for_admission)
}

// ---- Read-only rules -----------------------------------------------------------------------------

#[pyfunction]
fn zone_has_room(world: &Bound<'_, PyAny>, zone_id: &Bound<'_, PyAny>) -> PyResult<bool> {
    let zone_id = exact_str(zone_id)?;
    let read = PopulationIn::of(world, false)?;
    read.run(world, |cell, inputs| core::zone_has_room(cell, inputs, zone_id))
}

/// `_choose_admission_target`, for the comparison.
#[pyfunction]
fn choose_admission_target<'py>(world: &Bound<'py, PyAny>, item: &Bound<'py, PyAny>) -> PyResult<Bound<'py, PyAny>> {
    let py = world.py();
    let item = <CoreV as VPy>::from_py(item)?;
    let read = PopulationIn::of(world, false)?;
    match read.run(world, |cell, inputs| core::choose_admission_target(cell, inputs, &item))? {
        None => Ok(py.None().into_bound(py)),
        Some((zone, room)) => Ok(PyTuple::new(py, [zone.into_pyobject(py)?.into_any(), room.as_deref().into_pyobject(py)?.into_any()])?.into_any()),
    }
}

/// `validate_population_dynamics_state`: its ValueErrors in its order of checks, and Unsupported, a
/// `KernelRefusal` in Python, for what the Python rule's contracts decided. It is part of the
/// whole world's validation (policy E6), which also checks worlds the core does not hold as they
/// are (a save's world before its core, a tainted store, a namespace the world holds past its
/// table): those are read from their Python values, for the same core rule.
#[pyfunction]
pub(crate) fn validate_population_dynamics_state(world: &Bound<'_, PyAny>) -> PyResult<()> {
    let dynamics = held(world, DYNAMICS, "the world holds another population dynamics state")?;
    let presence = held(world, PRESENCE, "the world holds another presence state")?;
    let other = |held: &Held| matches!(held, Held::Other(_));
    let core_holds = !other(&dynamics)
        && !other(&presence)
        && crate::core::world_core::core_of(world).is_ok()
        && with_world(world, |cell| {
            let readable = |held: &Held, name: &str| matches!(held, Held::Absent) || cell.row::<sheltersim_core::row::ValueRow>(name).is_ok();
            readable(&dynamics, DYNAMICS)
                && readable(&presence, PRESENCE)
                && cell.rows().is_ok()
                && cell.entities.try_borrow().is_ok_and(|rows| rows.zones_tainted().is_none())
        })?;
    if !core_holds {
        return validate_values(world);
    }
    let routes = with_world(world, |cell| core::validation_reads_routes(cell, &dynamics))?.or_py()?;
    let read = PopulationIn::of(world, routes)?;
    with_rows(world, || read.run(world, core::validate))
}

/// `validate_population_dynamics_state` of the Python values of a world.
fn validate_values(world: &Bound<'_, PyAny>) -> PyResult<()> {
    let py = world.py();
    let Some(state) = state_value(world, DYNAMICS)? else {
        return Ok(());
    };
    let presence = state_value(world, PRESENCE)?;
    let mut agents = Vec::new();
    for id in attribute(world, intern!(py, "agents"))?.try_iter()? {
        agents.push(text(&id?)?);
    }
    let zones = attribute(world, intern!(py, "zones"))?;
    let zone_known = |zone: &str| -> sheltersim_core::error::Result<bool> { deferred(py, zones.contains(zone)) };
    let minute_index = deferred(py, attribute(world, intern!(py, "minute_index")).and_then(|value| exact_int(&value)));
    let duration = deferred(py, attribute(world, intern!(py, "duration_minutes")).and_then(|value| exact_int(&value)));
    let reads_routes = matches!(&state, CoreV::Map(fields) if matches!(fields.get("pending_arrivals"), Some(CoreV::List(pending)) if pending.iter().any(|item| matches!(item, CoreV::Map(item) if item.contains_key("approach_route_id")))));
    let routes = reads_routes.then(|| {
        approach_routes(world).map_err(|error| RoutesFault { caught: false, refusal: plan_read(py, &error) })
    });
    let event = |event_id: &str| -> sheltersim_core::error::Result<Option<Arc<EventRow>>> {
        deferred(py, (|| {
            let store = attribute(&attribute(world, intern!(py, "event_queue"))?, intern!(py, "_store"))?;
            let queue = store.downcast::<crate::state::events::QueueStore>().map_err(|_| Unsupported::new_err("the queue is not in Rust"))?;
            queue.get().row(py, event_id)
        })())
    };
    core::validate_facts(&core::Facts {
        state: Some(&state),
        presence: presence.as_ref(),
        agents: &agents,
        zone_known: &zone_known,
        minute_index,
        duration,
        routes: routes.as_ref(),
        event: &event,
    })
    .or_py()
}

/// What the constructors make of a saved profile (`core::build_agent`), for the tests: ("row",
/// the agent's fields as `vars()` of the objects would hold them), ("value_error", text) or
/// ("refused", reason).
#[pyfunction]
fn agent_of_profile<'py>(profile: &Bound<'py, PyAny>, zone_id: &str, minute: i64, schema_version: i64) -> PyResult<(&'static str, Bound<'py, PyAny>)> {
    let py = profile.py();
    let profile = <CoreV as VPy>::from_py(profile)?;
    match core::build_agent(&profile, &Arc::from(zone_id), minute, schema_version) {
        Ok(Built::Row(agent)) => {
            let needs = |needs: &sheltersim_core::entities::NeedRow| -> PyResult<Bound<'py, PyDict>> {
                let out = PyDict::new(py);
                out.set_item("toilet_urgency", needs.toilet_urgency)?;
                out.set_item("hunger", needs.hunger)?;
                out.set_item("fatigue", needs.fatigue)?;
                out.set_item("hydration_need", needs.hydration_need)?;
                Ok(out)
            };
            let profile = PyDict::new(py);
            profile.set_item("id", &*agent.profile.id)?;
            profile.set_item("group_id", agent.profile.group_id.as_deref())?;
            profile.set_item("initial_zone_id", &*agent.profile.initial_zone_id)?;
            profile.set_item("mobility", agent.profile.mobility.text())?;
            profile.set_item("support_need", agent.profile.support_need)?;
            profile.set_item("stress_sensitivity", agent.profile.stress_sensitivity)?;
            profile.set_item("rng_key", &*agent.profile.rng_key)?;
            profile.set_item("initial_needs", needs(&agent.profile.initial_needs)?)?;
            profile.set_item("provenance", agent.profile.provenance.to_py(py)?)?;
            let out = PyDict::new(py);
            out.set_item("profile", profile)?;
            out.set_item("zone_id", &*agent.zone_id)?;
            out.set_item("activity", agent.activity.text())?;
            out.set_item("target_id", agent.target_id.as_deref())?;
            out.set_item("needs", needs(&agent.needs)?)?;
            out.set_item("stress", agent.stress)?;
            out.set_item("trust", agent.trust)?;
            out.set_item("next_decision_at", agent.next_decision_at)?;
            out.set_item("support_request_id", agent.support_request_id.as_deref())?;
            out.set_item("support_cooldown_until", agent.support_cooldown_until)?;
            Ok(("row", out.into_any()))
        }
        Ok(Built::Raises(error)) => Ok(("value_error", to_py_err(error).value(py).str()?.into_any())),
        Err(error) => Ok(("refused", to_py_err(error).value(py).str()?.into_any())),
    }
}

pub fn register(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_function(wrap_pyfunction!(admit_pending_arrivals, m)?)?;
    m.add_function(wrap_pyfunction!(queue_arrivals, m)?)?;
    m.add_function(wrap_pyfunction!(apply_arrival_approach_completion, m)?)?;
    m.add_function(wrap_pyfunction!(schedule_population_retry, m)?)?;
    m.add_function(wrap_pyfunction!(queue_departures, m)?)?;
    m.add_function(wrap_pyfunction!(zone_has_room, m)?)?;
    m.add_function(wrap_pyfunction!(choose_admission_target, m)?)?;
    m.add_function(wrap_pyfunction!(validate_population_dynamics_state, m)?)?;
    m.add_function(wrap_pyfunction!(agent_of_profile, m)?)?;
    Ok(())
}

// ==== Helpers rules/population_lifecycle.rs still reads through Python (lane c7) ================

// ---- Reads through the object protocol -----------------------------------------------------------

/// `world.model_state.get(name)`, or None.
pub(crate) fn namespace<'py>(world: &Bound<'py, PyAny>, name: &str) -> PyResult<Option<Bound<'py, PyAny>>> {
    let table = attribute(world, intern!(world.py(), "model_state"))?;
    // These rules read and write through the tree: a detached namespace's is rebuilt.
    Ok(attached_state(&table, name)?.filter(|state| !state.is_none()))
}

/// Whether a rule that reads `model_state[name][part]` surely has nothing to do: the namespace is
/// absent or None, or a dict whose part is there and empty. Read as a `V`, a detached namespace
/// from its row, so that such a rule builds no tree; anything else goes on to read the tree.
pub(crate) fn nothing_in(world: &Bound<'_, PyAny>, name: &str, part: &str) -> PyResult<bool> {
    with_state_value(world, name, |state| {
        Ok(match state {
            None | Some(V::Null) => true,
            Some(V::Map(state)) => state.get(part).is_some_and(|value| !value.is_truthy()),
            Some(_) => false,
        })
    })
}

pub(crate) fn has_namespace(world: &Bound<'_, PyAny>, name: &str) -> PyResult<bool> {
    let table = attribute(world, intern!(world.py(), "model_state"))?;
    exact_dict(&table)?.contains(name)
}

/// `value.get(key)`.
pub(crate) fn get<'py>(value: &Bound<'py, PyAny>, key: &str) -> PyResult<Bound<'py, PyAny>> {
    value.call_method1(intern!(value.py(), "get"), (key,))
}

/// `value.get(key, {})`, for a read of a chain of mappings that may be absent.
pub(crate) fn get_map<'py>(value: &Bound<'py, PyAny>, key: &str) -> PyResult<Bound<'py, PyAny>> {
    value.call_method1(intern!(value.py(), "get"), (key, PyDict::new(value.py())))
}

pub(crate) fn is_dict(value: &Bound<'_, PyAny>) -> bool {
    value.is_instance_of::<PyDict>()
}

pub(crate) fn text(value: &Bound<'_, PyAny>) -> PyResult<Arc<str>> {
    Ok(Arc::from(exact_str(value)?))
}

pub(crate) fn optional_text(value: &Bound<'_, PyAny>) -> PyResult<Option<Arc<str>>> {
    if value.is_none() {
        Ok(None)
    } else {
        text(value).map(Some)
    }
}

/// The keys of a mapping, each an exact str, sorted as Python sorts them.
pub(crate) fn sorted_keys(mapping: &Bound<'_, PyAny>) -> PyResult<Vec<Arc<str>>> {
    let mut keys = Vec::new();
    for key in mapping.try_iter()? {
        keys.push(text(&key?)?);
    }
    keys.sort_unstable_by(|first, second| first.as_bytes().cmp(second.as_bytes()));
    Ok(keys)
}

pub(crate) fn new_list<'py>(py: Python<'py>, items: &[Arc<str>]) -> PyResult<Bound<'py, PyList>> {
    PyList::new(py, items.iter().map(|item| &**item))
}

/// The world's event, as a row, or Unsupported.
pub(crate) fn event_row(event: &Bound<'_, PyAny>) -> PyResult<EventRow> {
    EventRow::from_event(event)?.ok_or_else(|| Unsupported::new_err("the event has no row"))
}

pub(crate) use sheltersim_core::rules::kit::{payload, provenance};

// ---- Hashes and identifiers --------------------------------------------------------------------

fn hex(digest: &[u8]) -> String {
    let mut out = String::with_capacity(digest.len() * 2);
    for byte in digest {
        out.push(char::from_digit((byte >> 4) as u32, 16).unwrap_or('0'));
        out.push(char::from_digit((byte & 15) as u32, 16).unwrap_or('0'));
    }
    out
}

pub(crate) fn sha_hex(text: &str) -> String {
    hex(&Sha256::digest(text.as_bytes()))
}

/// `Identifier` of contracts/extensions.py.
pub(crate) fn is_identifier(text: &str) -> bool {
    let bytes = text.as_bytes();
    !bytes.is_empty()
        && bytes.len() <= 64
        && bytes[0].is_ascii_alphanumeric()
        && bytes[1..].iter().all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b':' | b'-'))
}

pub(crate) fn identifier(text: &str) -> PyResult<()> {
    if is_identifier(text) {
        Ok(())
    } else {
        unsupported("a value is not an Identifier")
    }
}

pub(crate) fn optional_identifier(text: Option<&str>) -> PyResult<()> {
    text.map_or(Ok(()), identifier)
}

// ---- The presence ledger: contracts/presence.py as far as a plain document goes ------------------

const RECORD_FIELDS: [&str; 9] = [
    "agent_id",
    "household_id",
    "status",
    "since_minute",
    "transition_zone_id",
    "return_zone_id",
    "expected_return_minute",
    "archive_path",
    "source_event_ids",
];
const STATUSES: [&str; 6] = ["admitted", "approaching", "awaiting_admission", "temporarily_absent", "departed", "transferred"];
const TRANSITION_FIELDS: [&str; 14] = [
    "schema_version",
    "transition_id",
    "event_id",
    "agent_id",
    "minute",
    "action",
    "from_zone_id",
    "to_zone_id",
    "from_household_id",
    "to_household_id",
    "causation_id",
    "source_ids",
    "state_snapshot_hash",
    "time_semantics",
];
const ACTIONS: [&str; 11] = [
    "arrived",
    "approaching",
    "approach_completed",
    "waiting",
    "admitted",
    "departed",
    "temporary_exit",
    "returned",
    "transferred",
    "reunited",
    "relocated",
];

fn required_identifier(map: &IndexMap<Arc<str>, V>, key: &str) -> PyResult<Arc<str>> {
    match map.get(key) {
        Some(V::Str(value)) if is_identifier(value) => Ok(value.clone()),
        _ => unsupported("a presence field is not an Identifier"),
    }
}

fn maybe_identifier(map: &IndexMap<Arc<str>, V>, key: &str) -> PyResult<Option<Arc<str>>> {
    match map.get(key) {
        None | Some(V::Null) => Ok(None),
        Some(V::Str(value)) if is_identifier(value) => Ok(Some(value.clone())),
        _ => unsupported("a presence field is not an Identifier or None"),
    }
}

fn identifiers(value: Option<&V>) -> PyResult<Vec<Arc<str>>> {
    match value {
        None => Ok(Vec::new()),
        Some(V::List(items)) => items
            .iter()
            .map(|item| match item {
                V::Str(text) if is_identifier(text) => Ok(text.clone()),
                _ => unsupported("a list holds what is not an Identifier"),
            })
            .collect(),
        _ => unsupported("a list of Identifiers is something else"),
    }
}

fn minute_field(value: Option<&V>) -> PyResult<i64> {
    match value {
        Some(V::Int(minute)) if *minute >= 0 => Ok(*minute),
        _ => unsupported("a minute is not a plain non-negative int"),
    }
}

/// `PresenceRecord`, of a document whose result is certain.
#[derive(Clone)]
pub(crate) struct Person {
    pub(crate) agent_id: Arc<str>,
    pub(crate) household_id: Option<Arc<str>>,
    pub(crate) status: Arc<str>,
    pub(crate) since_minute: i64,
    pub(crate) transition_zone_id: Option<Arc<str>>,
    pub(crate) return_zone_id: Option<Arc<str>>,
    pub(crate) expected_return_minute: Option<i64>,
    pub(crate) archive_path: Option<Vec<Arc<str>>>,
    pub(crate) source_event_ids: Vec<Arc<str>>,
}

impl Person {
    pub(crate) fn parse(value: &V) -> PyResult<Self> {
        let V::Map(map) = value else {
            return unsupported("a presence record is not a mapping");
        };
        if map.keys().any(|key| !RECORD_FIELDS.contains(&&**key)) {
            return unsupported("a presence record has another field");
        }
        let status = match map.get("status") {
            Some(V::Str(status)) if STATUSES.contains(&&**status) => status.clone(),
            _ => return unsupported("a presence status is another value"),
        };
        let expected_return_minute = match map.get("expected_return_minute") {
            None | Some(V::Null) => None,
            other => Some(minute_field(other)?),
        };
        let archive_path = match map.get("archive_path") {
            None | Some(V::Null) => None,
            other => {
                let path = identifiers(other)?;
                if path.is_empty() || path.len() > 5 {
                    return unsupported("an archive path has another length");
                }
                Some(path)
            }
        };
        let person = Self {
            agent_id: required_identifier(map, "agent_id")?,
            household_id: maybe_identifier(map, "household_id")?,
            status,
            since_minute: minute_field(map.get("since_minute"))?,
            transition_zone_id: maybe_identifier(map, "transition_zone_id")?,
            return_zone_id: maybe_identifier(map, "return_zone_id")?,
            expected_return_minute,
            archive_path,
            source_event_ids: identifiers(map.get("source_event_ids"))?,
        };
        person.check()?;
        Ok(person)
    }

    /// `coherent_return`.
    pub(crate) fn check(&self) -> PyResult<()> {
        if &*self.status == "temporarily_absent" {
            if self.expected_return_minute.is_some_and(|minute| minute < self.since_minute) {
                return unsupported("expected return precedes departure");
            }
            if self.archive_path.is_none() {
                return unsupported("temporary absence requires a retained state reference");
            }
        }
        if &*self.status == "admitted" && self.transition_zone_id.is_none() {
            return unsupported("admitted presence requires a zone");
        }
        Ok(())
    }

    /// `model_dump(mode="json")`.
    pub(crate) fn dump(&self) -> V {
        let mut map = IndexMap::with_capacity(RECORD_FIELDS.len());
        map.insert(Arc::from("agent_id"), V::Str(self.agent_id.clone()));
        map.insert(Arc::from("household_id"), optional(&self.household_id));
        map.insert(Arc::from("status"), V::Str(self.status.clone()));
        map.insert(Arc::from("since_minute"), V::Int(self.since_minute));
        map.insert(Arc::from("transition_zone_id"), optional(&self.transition_zone_id));
        map.insert(Arc::from("return_zone_id"), optional(&self.return_zone_id));
        map.insert(Arc::from("expected_return_minute"), self.expected_return_minute.map_or(V::Null, V::Int));
        map.insert(Arc::from("archive_path"), self.archive_path.as_ref().map_or(V::Null, |path| strings(path)));
        map.insert(Arc::from("source_event_ids"), strings(&self.source_event_ids));
        V::Map(map)
    }
}

pub(crate) fn optional(value: &Option<Arc<str>>) -> V {
    value.as_ref().map_or(V::Null, |text| V::Str(text.clone()))
}

pub(crate) fn strings(items: &[Arc<str>]) -> V {
    V::List(items.iter().map(|item| V::Str(item.clone())).collect())
}

/// `sorted(set(items))`.
pub(crate) fn sorted_set(mut items: Vec<Arc<str>>) -> Vec<Arc<str>> {
    items.sort_unstable_by(|first, second| first.as_bytes().cmp(second.as_bytes()));
    items.dedup();
    items
}

/// `PresenceTransition`, with values the plan has checked.
pub(crate) struct Transition<'a> {
    pub(crate) transition_id: String,
    pub(crate) event_id: &'a str,
    pub(crate) agent_id: &'a str,
    pub(crate) minute: i64,
    pub(crate) action: &'a str,
    pub(crate) from_zone_id: Option<Arc<str>>,
    pub(crate) to_zone_id: Option<Arc<str>>,
    pub(crate) from_household_id: Option<Arc<str>>,
    pub(crate) to_household_id: Option<Arc<str>>,
    pub(crate) causation_id: Option<Arc<str>>,
    pub(crate) source_ids: Vec<Arc<str>>,
    pub(crate) state_snapshot_hash: Option<String>,
}

impl Transition<'_> {
    fn dump(&self) -> V {
        let mut map = IndexMap::with_capacity(TRANSITION_FIELDS.len());
        map.insert(Arc::from("schema_version"), V::Int(2));
        map.insert(Arc::from("transition_id"), V::Str(Arc::from(self.transition_id.as_str())));
        map.insert(Arc::from("event_id"), V::Str(Arc::from(self.event_id)));
        map.insert(Arc::from("agent_id"), V::Str(Arc::from(self.agent_id)));
        map.insert(Arc::from("minute"), V::Int(self.minute));
        map.insert(Arc::from("action"), V::Str(Arc::from(self.action)));
        map.insert(Arc::from("from_zone_id"), optional(&self.from_zone_id));
        map.insert(Arc::from("to_zone_id"), optional(&self.to_zone_id));
        map.insert(Arc::from("from_household_id"), optional(&self.from_household_id));
        map.insert(Arc::from("to_household_id"), optional(&self.to_household_id));
        map.insert(Arc::from("causation_id"), optional(&self.causation_id));
        map.insert(Arc::from("source_ids"), strings(&self.source_ids));
        map.insert(Arc::from("state_snapshot_hash"), self.state_snapshot_hash.as_ref().map_or(V::Null, |hash| V::Str(Arc::from(hash.as_str()))));
        map.insert(Arc::from("time_semantics"), V::Str(Arc::from("recorded_event_minute")));
        V::Map(map)
    }
}

fn transition_is_plain(value: &V) -> PyResult<()> {
    let V::Map(map) = value else {
        return unsupported("a presence transition is not a mapping");
    };
    if map.keys().any(|key| !TRANSITION_FIELDS.contains(&&**key)) {
        return unsupported("a presence transition has another field");
    }
    if !matches!(map.get("schema_version"), None | Some(V::Int(2))) {
        return unsupported("a presence transition has another schema");
    }
    required_identifier(map, "transition_id")?;
    required_identifier(map, "event_id")?;
    required_identifier(map, "agent_id")?;
    minute_field(map.get("minute"))?;
    if !matches!(map.get("action"), Some(V::Str(action)) if ACTIONS.contains(&&**action)) {
        return unsupported("a presence transition has another action");
    }
    for key in ["from_zone_id", "to_zone_id", "from_household_id", "to_household_id", "causation_id"] {
        maybe_identifier(map, key)?;
    }
    identifiers(map.get("source_ids"))?;
    match map.get("state_snapshot_hash") {
        None | Some(V::Null) => {}
        Some(V::Str(hash)) if hash.len() == 64 && hash.bytes().all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f')) => {}
        _ => return unsupported("a presence transition has another hash"),
    }
    if !matches!(map.get("time_semantics"), None) && !matches!(map.get("time_semantics"), Some(V::Str(text)) if &**text == "recorded_event_minute") {
        return unsupported("a presence transition has other time semantics");
    }
    Ok(())
}

/// `PresenceState.model_validate(raw)`, for a document it certainly accepts.
pub(crate) fn presence_is_plain(raw: &Bound<'_, PyAny>) -> PyResult<()> {
    presence_value_is_plain(&V::from_py(raw)?)
}

/// `presence_is_plain` of the state as a `V`.
pub(crate) fn presence_value_is_plain(raw: &V) -> PyResult<()> {
    let V::Map(state) = raw else {
        return unsupported("the presence state is not a mapping");
    };
    if state.keys().any(|key| !["schema_version", "people", "history"].contains(&&**key)) {
        return unsupported("the presence state has another field");
    }
    if !matches!(state.get("schema_version"), None | Some(V::Int(1))) {
        return unsupported("the presence state has another schema");
    }
    let Some(V::Map(people)) = state.get("people") else {
        return unsupported("the presence state lacks its people");
    };
    for (key, record) in people {
        identifier(key)?;
        Person::parse(record)?;
    }
    match state.get("history") {
        None => {}
        Some(V::List(items)) if items.len() <= MAX_PRESENCE_HISTORY => {
            for item in items {
                transition_is_plain(item)?;
            }
        }
        _ => return unsupported("the presence history is something else"),
    }
    Ok(())
}

/// The namespace if a Python rule would use it, checked. `_presence_state`.
pub(crate) fn presence_state<'py>(world: &Bound<'py, PyAny>) -> PyResult<Option<Bound<'py, PyAny>>> {
    match namespace(world, PRESENCE)? {
        Some(raw) if is_dict(&raw) => {
            presence_is_plain(&raw)?;
            Ok(Some(raw))
        }
        _ => Ok(None),
    }
}

/// The record of a person in the ledger, if any.
pub(crate) fn person_in<'py>(raw: &Bound<'py, PyAny>, agent_id: &str) -> PyResult<Option<Person>> {
    let found = get(&raw.get_item("people")?, agent_id)?;
    if found.is_none() {
        Ok(None)
    } else {
        Person::parse(&V::from_py(&found)?).map(Some)
    }
}

/// `raw["people"][agent_id] = record` and the append of `_append_presence_history`.
pub(crate) fn write_presence(raw: &Bound<'_, PyAny>, person: &Person, transition: &Transition<'_>) -> PyResult<()> {
    let py = raw.py();
    raw.get_item("people")?.set_item(&*person.agent_id, person.dump().to_py(py)?)?;
    let history = raw.get_item("history")?;
    history.call_method1(intern!(py, "append"), (transition.dump().to_py(py)?,))?;
    let length = history.len()?;
    if length > MAX_PRESENCE_HISTORY {
        history.del_item(PySlice::new(py, 0, (length - MAX_PRESENCE_HISTORY) as isize, 1))?;
    }
    Ok(())
}

/// `json.dumps(record.model_dump(mode="json"), sort_keys=True, separators=(",", ":"))`'s hash: the
/// values of a record are Identifiers, ints and None, so the canonical bytes are those.
pub(crate) fn record_hash(person: &Person) -> PyResult<String> {
    let mut encoder = Encoder { out: Vec::new() };
    person.dump().write_json(&mut encoder).or_py()?;
    Ok(hex(&Sha256::digest(&encoder.out)))
}

// ---- Room in a zone: `capacity_metrics` as far as admission reads it -----------------------------

/// What `effective_capacity` is made of, for one zone.
#[derive(Clone, Copy)]
struct Capacity {
    nominal: i64,
    active_people: i64,
    expired_people: i64,
}

impl Capacity {
    /// `nominal + active_people + retained`, for this many people.
    fn effective(self, current: i64) -> i64 {
        let retained = self.expired_people.min((current - self.nominal - self.active_people).max(0));
        self.nominal + self.active_people + retained
    }
}

/// `capacity_metrics`' reads for this zone (`rules/overlay.rs`).
fn capacity_of(world: &Bound<'_, PyAny>, zone_id: &str) -> PyResult<Option<Capacity>> {
    let py = world.py();
    let zones = attribute(world, intern!(py, "zones"))?;
    let zone = zones.get_item(zone_id)?;
    let nominal = attribute(&zone, intern!(py, "capacity"))?;
    if nominal.is_none() {
        return Ok(None);
    }
    let nominal = exact_int(&nominal)?;
    let read = crate::rules::overlay::capacity_of(world, zone_id, nominal, Some(nominal))?;
    Ok(Some(Capacity { nominal, active_people: read.active_people, expired_people: read.expired_people }))
}

/// The people of a world and the rooms given in this plan, as admission counts them.
pub(crate) struct Places<'py> {
    world: Bound<'py, PyAny>,
    occupancy: HashMap<Arc<str>, i64>,
    incoming: HashMap<Arc<str>, i64>,
    capacity: HashMap<String, Option<Capacity>>,
    planned_rooms: Vec<(Arc<str>, Option<Pt>)>,
}

impl<'py> Places<'py> {
    pub(crate) fn of(world: &Bound<'py, PyAny>) -> PyResult<Self> {
        let (occupancy, incoming) = EntityStore::with_rows(&entities_of(world)?, |rows| {
            let mut occupancy: HashMap<Arc<str>, i64> = HashMap::new();
            let mut incoming: HashMap<Arc<str>, i64> = HashMap::new();
            for (_, agent) in rows.agents.iter() {
                *occupancy.entry(agent.zone_id.clone()).or_default() += 1;
                if agent.activity == Activity::Moving {
                    if let Some(target) = &agent.target_id {
                        // `incoming_travelers`: a facility's zone, or the target itself.
                        let zone = rows.facilities.get(target).map_or(target, |facility| &facility.zone_id);
                        *incoming.entry(zone.clone()).or_default() += 1;
                    }
                }
            }
            Ok((occupancy, incoming))
        })?;
        Ok(Self { world: world.clone(), occupancy, incoming, capacity: HashMap::new(), planned_rooms: Vec::new() })
    }

    fn capacity(&mut self, zone_id: &str) -> PyResult<Option<Capacity>> {
        if let Some(known) = self.capacity.get(zone_id) {
            return Ok(*known);
        }
        let found = capacity_of(&self.world, zone_id)?;
        self.capacity.insert(zone_id.to_owned(), found);
        Ok(found)
    }

    fn occupancy(&self, zone_id: &str) -> i64 {
        self.occupancy.get(zone_id).copied().unwrap_or(0)
    }

    /// population_lifecycle.py's `_zone_has_room`.
    pub(crate) fn has_room_for(&mut self, zone_id: &str, excluding: Option<&str>) -> PyResult<bool> {
        let mut total = self.occupancy(zone_id) + self.incoming.get(zone_id).copied().unwrap_or(0);
        if let Some(excluded) = excluding {
            let leaving = EntityStore::with_rows(&entities_of(&self.world)?, |rows| {
                let Some(agent) = rows.agents.get(excluded) else {
                    return Ok(0);
                };
                let mut count = i64::from(&*agent.zone_id == zone_id);
                if agent.activity == Activity::Moving {
                    if let Some(target) = &agent.target_id {
                        let zone = rows.facilities.get(target).map_or(target, |facility| &facility.zone_id);
                        count += i64::from(&**zone == zone_id);
                    }
                }
                Ok(count)
            })?;
            total -= leaving;
        }
        let Some(capacity) = self.capacity(zone_id)? else {
            return Ok(true);
        };
        Ok(total < capacity.effective(total))
    }

    /// A person is planned into this zone.
    pub(crate) fn add(&mut self, zone_id: &Arc<str>) {
        *self.occupancy.entry(zone_id.clone()).or_default() += 1;
    }

    /// `available_temporary_room`, and the point `admit_to_temporary_room` then gives.
    pub(crate) fn available_room(&self, zone_id: &str) -> PyResult<Option<(Arc<str>, Pt)>> {
        crate::rules::overlay::available_room_after(&self.world, zone_id, &self.planned_rooms)
    }

    /// The room this plan fills, for the admissions that follow it.
    pub(crate) fn take_room(&mut self, room_id: &Arc<str>, point: Pt) -> PyResult<()> {
        let held = has_namespace(&self.world, MOVEMENT)?;
        self.planned_rooms.push((room_id.clone(), held.then_some(point)));
        Ok(())
    }
}

// ---- `_admit_profile` ----------------------------------------------------------------------------

/// `admit_to_temporary_room`, whose checks the plan made, and a point of a position's record.
pub(crate) fn write_room_admission(
    world: &Bound<'_, PyAny>,
    agent_id: &str,
    room_id: &str,
    minute: i64,
    event_id: &str,
) -> PyResult<()> {
    let py = world.py();
    let state = namespace(world, OVERLAY)?.ok_or_else(|| PyRuntimeError::new_err("the overlay left during an admission"))?;
    let room = state.get_item("temporary_rooms")?.get_item(room_id)?;
    let mut sources = vec![Arc::from(ROOM_INTAKE_SOURCE)];
    for source in room.get_item("source_ids")?.try_iter()? {
        sources.push(text(&source?)?);
    }
    let record = PyDict::new(py);
    record.set_item("agent_id", agent_id)?;
    record.set_item("room_id", room_id)?;
    record.set_item("admitted_minute", minute)?;
    record.set_item("event_id", event_id)?;
    record.set_item("source_ids", new_list(py, &sorted_set(sources))?)?;
    state.get_item("room_admissions")?.set_item(agent_id, record)
}

pub(crate) fn point_document(py: Python<'_>, point: Pt) -> PyResult<Bound<'_, PyDict>> {
    let document = PyDict::new(py);
    document.set_item("x", point.x)?;
    document.set_item("y", point.y)?;
    Ok(document)
}

/// `position.update({...})` after an admission to a room.
pub(crate) fn place_in_room(position: &Bound<'_, PyAny>, zone_id: &str, point: Pt) -> PyResult<()> {
    let py = position.py();
    let update = PyDict::new(py);
    update.set_item("zone_id", zone_id)?;
    update.set_item("point", point_document(py, point)?)?;
    update.set_item("edge_id", py.None())?;
    update.set_item("progress", 1.0)?;
    update.set_item("position_kind", "physical")?;
    position.call_method1(intern!(py, "update"), (update,))?;
    Ok(())
}

fn models<'py>(py: Python<'py>) -> PyResult<Bound<'py, PyModule>> {
    py.import("sheltersim.domain.models")
}

/// A new `AgentState` from a saved profile and needs, as `_admit_profile` and `_restore_person` make.
pub(crate) fn profile_object<'py>(world: &Bound<'py, PyAny>, raw: &Bound<'py, PyAny>) -> PyResult<(Bound<'py, PyAny>, Bound<'py, PyDict>)> {
    let py = world.py();
    let models = models(py)?;
    let initial = PyDict::new(py);
    initial.call_method1(intern!(py, "update"), (raw.get_item("initial_needs")?,))?;
    if exact_int(&attribute(world, intern!(py, "schema_version"))?)? == 5 {
        initial.set_item("hydration_need", 0.0)?;
    }
    let provenance = PyDict::new(py);
    provenance.call_method1(intern!(py, "update"), (raw.get_item("provenance")?,))?;
    let options = PyDict::new(py);
    options.set_item("id", raw.get_item("id")?)?;
    options.set_item("group_id", raw.get_item("group_id")?)?;
    options.set_item("initial_zone_id", raw.get_item("initial_zone_id")?)?;
    options.set_item("mobility", models.getattr("Mobility")?.call1((raw.get_item("mobility")?,))?)?;
    options.set_item("support_need", raw.get_item("support_need")?)?;
    options.set_item("stress_sensitivity", raw.get_item("stress_sensitivity")?)?;
    options.set_item("rng_key", raw.get_item("rng_key")?)?;
    options.set_item("initial_needs", models.getattr("InitialNeedProfile")?.call((), Some(&initial))?)?;
    options.set_item("provenance", provenance)?;
    Ok((models.getattr("AgentProfile")?.call((), Some(&options))?, initial))
}

/// What admission and restoration ask of the Rust paths they use, in a plan: their writes must not
/// refuse. Their own checks say so here.
pub(crate) fn check_admission_paths(world: &Bound<'_, PyAny>, zones: &[&str]) -> PyResult<()> {
    if has_namespace(world, AGENT_DYNAMICS)? {
        crate::rules::agent_dynamics::check_add_agent(world)?;
    }
    if has_namespace(world, MOVEMENT)? {
        let mirror = mirror_held(world)?;
        crate::core::world_core::NamespaceMirror::with_row::<MovementRow, _>(&mirror, |_| Ok(()))?;
        crate::rules::movement::check_idle_points(world, zones)?;
    }
    Ok(())
}

/// `state["positions"][id] = {...}` of a person now standing in a zone, and `finish_movement`.
pub(crate) fn reset_position(world: &Bound<'_, PyAny>, movement: &Bound<'_, PyAny>, agent_id: &str, zone_id: &str) -> PyResult<()> {
    let py = world.py();
    let position = PyDict::new(py);
    position.set_item("zone_id", zone_id)?;
    position.set_item("point", py.None())?;
    position.set_item("edge_id", py.None())?;
    position.set_item("progress", 0.0)?;
    position.set_item("position_kind", "unavailable")?;
    movement.get_item("positions")?.set_item(agent_id, position)?;
    let key = PyString::new(py, agent_id);
    crate::rules::movement::finish_movement(world, key.as_any())
}

/// `V::Map` from pairs in this order.
macro_rules! v_map {
    ($($key:expr => $value:expr),* $(,)?) => {{
        let mut map: indexmap::IndexMap<Arc<str>, V> = indexmap::IndexMap::new();
        $(map.insert(Arc::from($key), $value);)*
        map
    }};
}
pub(crate) use v_map;

/// `geometry_overlay.get("schema_version") not in {3, 4}`, as the set membership.
fn room_schema(config: &Bound<'_, PyAny>) -> PyResult<bool> {
    let py = config.py();
    pyo3::types::PySet::new(py, [3, 4])?.contains(get(config, "schema_version")?)
}

/// `room_schema` of the overlay's config, as the section holds it: what it raised is raised again.
fn rooms_schema(world: &Bound<'_, PyAny>, rooms: &OverlayRooms) -> PyResult<bool> {
    match &rooms.room_schema {
        Ok(schema) => Ok(*schema),
        Err(fault) => Err(configs::raise(fault, || {
            room_schema(&get_map(&attribute(world, intern!(world.py(), "model_configs"))?, "geometry_overlay")?)
        })),
    }
}

/// The section `overlay_rooms` as the arrivals read it, for the tests: whether the config is a
/// dict, and its room schema for a dict.
pub(crate) fn overlay_rooms_of(world: &Bound<'_, PyAny>) -> PyResult<(bool, bool)> {
    let rooms = configs::kept_python(world, |configs| &configs.overlay_rooms, overlay_rooms)?;
    Ok((rooms.is_dict, rooms.is_dict && rooms_schema(world, &rooms)?))
}

/// `model_configs.get("geometry_overlay", {})` as the arrivals read it: whether it is a dict, and
/// for a dict `room_schema` of it or what that raised. The section `overlay_rooms`.
fn overlay_rooms(world: &Bound<'_, PyAny>) -> PyResult<OverlayRooms> {
    let py = world.py();
    let config = get_map(&attribute(world, intern!(py, "model_configs"))?, "geometry_overlay")?;
    let is_dict = is_dict(&config);
    let room_schema = if is_dict { room_schema(&config).map_err(|error| configs::fault_of(py, &error)) } else { Ok(false) };
    Ok(OverlayRooms { is_dict, room_schema })
}

/// `json.loads(key)` of a key of a pair of people, as `_pair_from_key` accepts it.
pub(crate) fn pair_of(key: &str) -> PyResult<[String; 2]> {
    let Ok(parts) = serde_json::from_str::<Vec<String>>(key) else {
        return unsupported("a pair key is not a list of strs");
    };
    match <[String; 2]>::try_from(parts) {
        Ok(pair) => Ok(pair),
        Err(_) => unsupported("a pair key is not two people"),
    }
}

pub(crate) fn check_pairs(mapping: &Bound<'_, PyAny>) -> PyResult<()> {
    for key in mapping.try_iter()? {
        pair_of(exact_str(&key?)?)?;
    }
    Ok(())
}


/// What `_remove_information_state` and `_archive_person` ask of the information state.
pub(crate) fn check_information(world: &Bound<'_, PyAny>) -> PyResult<()> {
    if let Some(info) = namespace(world, INFORMATION)? {
        for name in ["beliefs", "query_pressure", "messages"] {
            exact_dict(&info.get_item(name)?)?;
        }
        check_pairs(&info.get_item("contacts")?)?;
        info.get_item("support_requests")?.try_iter()?;
    }
    Ok(())
}

pub(crate) fn check_relationship_keys(world: &Bound<'_, PyAny>) -> PyResult<()> {
    for name in [SOCIAL_NETWORK, BODY] {
        if let Some(state) = namespace(world, name)? {
            check_pairs(&state.get_item("relationships")?)?;
        }
    }
    if let Some(runtime) = namespace(world, SOCIAL_RUNTIME)?.filter(is_dict) {
        let contacts = get_map(&runtime, "contacts")?;
        if is_dict(&contacts) {
            check_pairs(&contacts)?;
        }
    }
    Ok(())
}

/// Each pending event has a row: `_cancel_agent_events` records rows.
pub(crate) fn check_queue_rows(world: &Bound<'_, PyAny>) -> PyResult<()> {
    let py = world.py();
    let writer = EventWriter::of(world)?;
    let ids = writer.queue().get().pending_ids(py, None)?;
    for id in ids {
        if writer.queue().get().row(py, &id)?.is_none() {
            return unsupported("a pending event has no row");
        }
    }
    Ok(())
}

fn event_names_agent(row: &EventRow, agent_id: &str) -> bool {
    ["agent_id", "sender_agent_id", "recipient_agent_id", "participant_agent_id"]
        .iter()
        .any(|key| matches!(payload(row, key), Some(V::Str(value)) if &**value == agent_id))
}

/// `_cancel_agent_events`.
pub(crate) fn cancel_agent_events(world: &Bound<'_, PyAny>, agent_id: &str, withdrawn: &HashSet<Arc<str>>) -> PyResult<()> {
    let py = world.py();
    let minute = exact_int(&attribute(world, intern!(py, "minute_index"))?)?;
    let writer = EventWriter::of(world)?;
    let pending = |writer: &EventWriter<'_>| -> PyResult<Vec<Arc<EventRow>>> {
        let ids = writer.queue().get().pending_ids(py, None)?;
        let mut rows = Vec::with_capacity(ids.len());
        for id in ids {
            if let Some(row) = writer.queue().get().row(py, &id)? {
                rows.push(row);
            }
        }
        Ok(rows)
    };
    let detail = format!("Agent {agent_id} departed before the event was due");
    for row in pending(&writer)? {
        if event_names_agent(&row, agent_id) {
            writer.settle(&row, minute, EventStatus::Suppressed, Some(&detail))?;
        }
    }
    let detail = format!("Claim withdrawn after Agent {agent_id} departed");
    for row in pending(&writer)? {
        let from_agent = &*row.event_type == "information_rumor" && matches!(payload(&row, "origin_agent_id"), Some(V::Str(origin)) if &**origin == agent_id);
        let claim = matches!(payload(&row, "message_id"), Some(V::Str(message)) if withdrawn.contains(message));
        if from_agent || claim {
            writer.settle(&row, minute, EventStatus::Suppressed, Some(&detail))?;
        }
    }
    Ok(())
}

/// `release_sleep_bed` and `release_temporary_room_admission`.
pub(crate) fn release_room(world: &Bound<'_, PyAny>, agent_id: &str) -> PyResult<()> {
    let py = world.py();
    if let Some(state) = namespace(world, OVERLAY)?.filter(is_dict) {
        for name in ["bed_reservations", "room_admissions"] {
            let map = get(&state, name)?;
            if is_dict(&map) {
                map.call_method1(intern!(py, "pop"), (agent_id, py.None()))?;
            }
        }
    }
    Ok(())
}


/// `state["support_requests"][:] = [item for item in ... if item.get("agent_id") != agent_id]`.
pub(crate) fn filter_support_requests(state: &Bound<'_, PyAny>, agent_id: &str) -> PyResult<()> {
    let py = state.py();
    let requests = state.get_item("support_requests")?;
    let kept = PyList::empty(py);
    for item in requests.try_iter()? {
        let item = item?;
        if !get(&item, "agent_id")?.eq(agent_id)? {
            kept.append(item)?;
        }
    }
    requests.set_item(PySlice::full(py), kept)
}

/// `world.support_requests = {...}` without this person's requests.
pub(crate) fn drop_support_requests(world: &Bound<'_, PyAny>, agent_id: &str) -> PyResult<()> {
    let py = world.py();
    let kept = PyDict::new(py);
    for item in attribute(world, intern!(py, "support_requests"))?.call_method0(intern!(py, "items"))?.try_iter()? {
        let (key, request): (Bound<'_, PyAny>, Bound<'_, PyAny>) = item?.extract()?;
        if !attribute(&request, intern!(py, "agent_id"))?.eq(agent_id)? {
            kept.set_item(key, request)?;
        }
    }
    world.setattr(intern!(py, "support_requests"), kept)
}

/// `raise ValueError(text)` of a validation, through `planning` (it is marked as made here).
pub(crate) fn invalid<T>(text: &str) -> PyResult<T> {
    Python::with_gil(|py| Err(errors::marked_value_error(py, text)))
}

/// A validation as a validator of `state_mirror._RUST_VALIDATORS`, which says yes or Unsupported:
/// its ValueError is the validation's own to raise where it runs.
pub(crate) fn as_refusal<T>(result: PyResult<T>) -> PyResult<T> {
    result.map_err(|error| {
        Python::with_gil(|py| {
            if errors::made_here(py, &error) && error.is_instance_of::<pyo3::exceptions::PyValueError>(py) {
                Unsupported::new_err(format!("the validation raises: {}", error.value(py)))
            } else {
                error
            }
        })
    })
}

/// `value != 1` is false: 1, True or 1.0.
pub(crate) fn is_one(value: Option<&V>) -> bool {
    matches!(value, Some(V::Int(1) | V::Bool(true))) || matches!(value, Some(V::Float(number)) if *number == 1.0)
}
