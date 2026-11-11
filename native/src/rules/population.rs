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
//! `rules/population_lifecycle.rs` reads the same inputs (`PopulationIn`).

use std::sync::Arc;

use pyo3::exceptions::PyRuntimeError;
use pyo3::intern;
use pyo3::prelude::*;
use pyo3::types::{PyDict, PyFloat, PyInt, PyList, PyString, PyTuple};

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
use crate::core::row::{attribute, exact_int, exact_str, unsupported};
use crate::core::value::VPy;
use crate::core::world_core::{namespace_held, state_value, with_world, PyWorldCell};
use crate::host::PyHost;
use crate::rules::movement_geometry::{approach_routes, Geo, GeoPy};
use crate::state::events::{read_frozen, EventRow, EventRowPy, EventWriter, MAX_ROW_DEPTH};
use crate::Unsupported;

pub(crate) use sheltersim_core::rules::kit::{DYNAMICS, PRESENCE};
pub(crate) const MOVEMENT: &str = "movement_v1";
pub(crate) const INFORMATION: &str = "information_spread_v1";
pub(crate) const BODY: &str = "body_state_v1";
pub(crate) const AGENT_DYNAMICS: &str = "agent_dynamics_v1";
pub(crate) const SOCIAL_NETWORK: &str = "social_network_v1";
pub(crate) const SOCIAL_RUNTIME: &str = "social_propagation_v1";
pub(crate) const OVERLAY: &str = "geometry_overlay_v1";

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
pub(crate) fn plan_read(py: Python<'_>, error: &PyErr) -> CoreError {
    if error.is_instance_of::<Unsupported>(py) {
        CoreError::Unsupported(error.value(py).to_string().into())
    } else {
        CoreError::Unsupported(format!("the plan read: {error}").into())
    }
}

pub(crate) fn held(world: &Bound<'_, PyAny>, name: &str, other: &'static str) -> PyResult<Held> {
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
pub(crate) struct PopulationIn {
    pub(crate) inputs: Inputs,
    geo_error: Option<PyErr>,
}

impl PopulationIn {
    /// `routes`: whether the rule reads the approach routes.
    pub(crate) fn of(world: &Bound<'_, PyAny>, routes: bool) -> PyResult<Self> {
        Self::reading(world, routes, false)
    }

    /// `of`, with the floor plan read even without movement or the overlay (a relocation's route).
    pub(crate) fn reading(world: &Bound<'_, PyAny>, routes: bool, plan: bool) -> PyResult<Self> {
        let py = world.py();
        let namespaces = namespaces_of(world)?;
        let schema = configs::kept_section(world, |configs| &configs.overlay_schema, parse_overlay_schema)?;
        let policy = configs::kept_section(world, |configs| &configs.agent_dynamics_policy, config_policy)?;
        let awareness = configs::kept_section(world, |configs| &configs.facility_awareness, awareness)?;
        // The floor plan is read only where a rule can reach it: with movement or the overlay.
        let (geo, geo_error) = if !plan && matches!(namespaces.movement, Held::Absent) && matches!(namespaces.overlay, Held::Absent) {
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
    pub(crate) fn finish<T>(&self, py: Python<'_>, result: PyResult<T>) -> PyResult<T> {
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
pub(crate) fn with_rows<T>(world: &Bound<'_, PyAny>, kernel: impl FnOnce() -> PyResult<T>) -> PyResult<T> {
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

// ---- Readers of the glue ---------------------------------------------------------------------------

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

/// The world's event, as a row, or Unsupported.
pub(crate) fn event_row(event: &Bound<'_, PyAny>) -> PyResult<EventRow> {
    EventRow::from_event(event)?.ok_or_else(|| Unsupported::new_err("the event has no row"))
}

/// `Identifier` of contracts/extensions.py.
pub(crate) fn is_identifier(text: &str) -> bool {
    sheltersim_core::events::is_stable_id(text)
}

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
pub(crate) fn overlay_rooms(world: &Bound<'_, PyAny>) -> PyResult<OverlayRooms> {
    let py = world.py();
    let config = get_map(&attribute(world, intern!(py, "model_configs"))?, "geometry_overlay")?;
    let is_dict = is_dict(&config);
    let room_schema = if is_dict { room_schema(&config).map_err(|error| configs::fault_of(py, &error)) } else { Ok(false) };
    Ok(OverlayRooms { is_dict, room_schema })
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
