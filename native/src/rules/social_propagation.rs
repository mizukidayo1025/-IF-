//! The glue of the core's social model (`sheltersim_core::rules::social_propagation`, lane P8 a4):
//! `_social_propagation` of the update phase (experiments/phases/segments.py, KERNEL_ONLY_SEGMENTS).
//!
//! The step reads what only Python has: the policy from the controller's `Policies`
//! (`run::policies_of`), the step's `CounterRNG` and its seed, the minute and the view's minute as
//! the Python objects they are, `controller.zone_neighbors`, and the Python `AgentStateDelta`s,
//! which it reads into the core's `Delta`s. It calls the core with the world borrowed, and makes the
//! Python deltas at the edge: a delta whose stress the model kept is itself, any other a copy with
//! the new stress, as `AgentStateDelta.with_stress` made it. On the first tick of a Run it stores
//! the empty namespace in `model_state`, as the deleted Python rule did, before the core writes.
//! The trace's document goes through the host (`Host::trace`).

use std::collections::HashMap;
use std::sync::Arc;

use pyo3::intern;
use pyo3::prelude::*;
use pyo3::sync::GILOnceCell;
use pyo3::types::{PyDict, PyFloat, PyInt, PyString, PyTuple, PyType};

use sheltersim_core::host::{Host, TraceLevel};
use sheltersim_core::row::IdMap;
use sheltersim_core::rules::psychology::{Delta, BODY_NAMES};
use sheltersim_core::rules::social_propagation::{self as core, Neighbors, Person, Settings};
use sheltersim_core::run::stress::Model;
use sheltersim_core::state::social_propagation::{ContactRecord, NAME, SCHEMA_VERSION};
use sheltersim_core::value::V;

use crate::core::errors::OrPy;
use crate::core::mathx::Num;
use crate::core::row::PyField;
use crate::core::rng::{start_draw_log, take_draw_log};
use crate::core::run::policies_of;
use crate::core::tracked::{as_readable_dict, as_readable_list};
use crate::core::value::VPy;
use crate::core::world_core::{raw_dict_get, raw_dict_set, with_world};
use crate::host::PyHost;
use crate::rules::psychology::{bad_error, delta_types};
use crate::rules::world_view::view_of;
use crate::Unsupported;

#[track_caller]
fn unsupported<T>(reason: &'static str) -> PyResult<T> {
    Err(Unsupported::new_err(reason))
}

static COUNTER_RNG: GILOnceCell<Py<PyType>> = GILOnceCell::new();

fn counter_rng(py: Python<'_>) -> PyResult<&Py<PyType>> {
    COUNTER_RNG.get_or_try_init(py, || {
        Ok(py.import("sheltersim.core.rng")?.getattr("CounterRNG")?.downcast_into::<PyType>()?.unbind())
    })
}

/// An exact int that fits.
fn exact_int(value: &Bound<'_, PyAny>) -> PyResult<i64> {
    value
        .downcast_exact::<PyInt>()
        .map_err(|_| Unsupported::new_err("not an int"))?
        .extract()
        .map_err(|_| Unsupported::new_err("int out of range"))
}

/// `controller.zone_neighbors`, each zone's value read now and refused only where the step reads
/// it: `sorted(zone_neighbors.get(zone, ()))` needs strs in a tuple or a list.
fn neighbors_of(zone_neighbors: &Bound<'_, PyAny>) -> PyResult<Neighbors> {
    let Some(table) = as_readable_dict(zone_neighbors) else {
        return Ok(Err("the zone neighbors are not a dict"));
    };
    let mut found = HashMap::with_capacity(table.len());
    for (zone, names) in table.iter() {
        let Ok(zone) = zone.downcast_exact::<PyString>() else {
            // `get(zone)` with a str never finds it.
            continue;
        };
        let Ok(zone) = zone.to_str() else {
            continue;
        };
        found.insert(zone.to_owned(), texts_of(&names)?);
    }
    Ok(Ok(found))
}

fn texts_of(names: &Bound<'_, PyAny>) -> PyResult<Result<Vec<String>, &'static str>> {
    let mut texts = Vec::with_capacity(4);
    let mut add = |name: Bound<'_, PyAny>| -> Result<(), &'static str> {
        let Ok(name) = name.downcast_exact::<PyString>() else {
            return Err("a neighbor is not a str");
        };
        texts.push(name.to_str().map_err(|_| "not UTF-8")?.to_owned());
        Ok(())
    };
    let added = if let Ok(names) = names.downcast_exact::<PyTuple>() {
        names.iter().try_for_each(&mut add)
    } else if let Some(names) = as_readable_list(names) {
        names.iter().try_for_each(&mut add)
    } else {
        Err("the neighbors of a zone are not a tuple or a list")
    };
    Ok(added.map(|()| texts))
}

/// The names of an `AgentStateDelta`'s floats: `NEEDS_AND_TRUST`.
fn need_names(py: Python<'_>) -> [&Bound<'_, PyString>; 6] {
    [
        intern!(py, "toilet_urgency"),
        intern!(py, "hunger"),
        intern!(py, "fatigue"),
        intern!(py, "hydration_need"),
        intern!(py, "stress"),
        intern!(py, "trust"),
    ]
}

/// A number of a delta that `__post_init__` checked: a float, or an int it let through.
fn number(value: &Bound<'_, PyAny>) -> PyResult<f64> {
    if let Ok(value) = value.downcast_exact::<PyFloat>() {
        return Ok(value.value());
    }
    if value.downcast_exact::<PyInt>().is_ok() {
        return value.extract::<f64>().map_err(|_| Unsupported::new_err("a delta value is not a number"));
    }
    unsupported("a delta value is not a number")
}

/// One Python delta, its `__dict__`, and the core's `Delta` of it.
struct Read<'py> {
    object: Bound<'py, PyAny>,
    fields: Bound<'py, PyAny>,
    delta: Delta,
}

/// The deltas of the step: an exact tuple of exact `AgentStateDelta`s with exact str IDs of the
/// minute, each stress an exact float within 0..1.
fn deltas_of<'py>(deltas: &Bound<'py, PyAny>, minute: i64) -> PyResult<Vec<Read<'py>>> {
    let py = deltas.py();
    let Ok(deltas) = deltas.downcast_exact::<PyTuple>() else {
        return unsupported("the deltas are not a tuple");
    };
    let class = &delta_types(py)?.0;
    let names = need_names(py);
    let mut found = Vec::with_capacity(deltas.len());
    for object in deltas.iter() {
        if !object.get_type().is(class) {
            return unsupported("a delta of another class");
        }
        let fields = object.getattr(intern!(py, "__dict__"))?;
        let item = |name: &Bound<'py, PyString>| -> PyResult<Bound<'py, PyAny>> {
            raw_dict_get(&fields, name.as_any())?.ok_or_else(|| Unsupported::new_err("a field is missing"))
        };
        let id = item(intern!(py, "agent_id"))?;
        let Ok(id) = id.downcast_into_exact::<PyString>() else {
            return unsupported("an id is not a str");
        };
        let Ok(id) = id.to_str() else {
            return unsupported("an id is not UTF-8");
        };
        if exact_int(&item(intern!(py, "minute_index"))?)? != minute {
            return unsupported("a delta is at another minute");
        }
        let Ok(stress) = item(names[4])?.downcast_into_exact::<PyFloat>() else {
            return unsupported("not a float");
        };
        let stress = stress.value();
        if !(0.0..=1.0).contains(&stress) {
            return unsupported("a stress is outside 0..1");
        }
        let mut values = [0.0; 6];
        for (place, name) in names.iter().enumerate() {
            values[place] = if place == 4 { stress } else { number(&item(name)?)? };
        }
        let mut body = [0.0; 7];
        let mut has_body = None;
        for (place, name) in BODY_NAMES.iter().enumerate() {
            let value = item(&PyString::intern(py, name))?;
            let present = !value.is_none();
            if *has_body.get_or_insert(present) != present {
                return unsupported("a delta has only some body values");
            }
            if present {
                body[place] = number(&value)?;
            }
        }
        let [toilet_urgency, hunger, fatigue, hydration_need, stress, trust] = values;
        let delta = Delta {
            agent_id: Arc::from(id),
            minute_index: minute,
            toilet_urgency,
            hunger,
            fatigue,
            hydration_need,
            stress,
            trust,
            body: has_body.unwrap_or(false).then_some(body),
        };
        found.push(Read { object, fields, delta });
    }
    Ok(found)
}

/// `AgentStateDelta.with_stress`: a copy of the delta's fields, in their order, with this stress.
fn with_stress<'py>(read: &Read<'py>, stress: f64) -> PyResult<Bound<'py, PyAny>> {
    let py = read.object.py();
    let types = delta_types(py)?;
    let object = types.2.bind(py).call1((types.0.bind(py),))?;
    let fields = object.getattr(intern!(py, "__dict__"))?;
    fields.downcast::<PyDict>()?.update(read.fields.downcast::<PyDict>()?.as_mapping())?;
    raw_dict_set(&fields, intern!(py, "stress").as_any(), PyFloat::new(py, stress).as_any())?;
    Ok(object)
}

/// `_social_propagation` of the update phase (experiments/phases/segments.py).
#[pyfunction]
pub(crate) fn update_social_propagation<'py>(
    controller: &Bound<'py, PyAny>,
    world: &Bound<'py, PyAny>,
    view: &Bound<'py, PyAny>,
    base_deltas: &Bound<'py, PyAny>,
    minute: &Bound<'py, PyAny>,
    rng: &Bound<'py, PyAny>,
) -> PyResult<Bound<'py, PyAny>> {
    let py = world.py();
    let policies = policies_of(controller)?;
    let stress = &policies.stress;
    if !core::enabled(stress).map_err(|bad| bad_error(controller, bad))? {
        return Ok(base_deltas.clone());
    }
    let mut host = PyHost::of_holder(controller, policies.clone());
    let traced = host.trace_level().map_err(|error| error.0)? != TraceLevel::Off;
    if !rng.get_type().is(counter_rng(py)?) {
        return unsupported("the RNG is not a CounterRNG");
    }
    let settings = core::settings(stress).map_err(|bad| bad_error(controller, bad))?;
    let minute_value = exact_int(minute)?;
    if minute_value < 0 || exact_int(&view.getattr(intern!(py, "minute_index"))?)? != minute_value {
        return unsupported("the view is at another minute");
    }
    let read = deltas_of(base_deltas, minute_value)?;
    let rust_view = view_of(view)?;
    let expressiveness = core::expressiveness(stress).map_err(|bad| bad_error(controller, bad))?;
    let neighbors = neighbors_of(&controller.getattr(intern!(py, "zone_neighbors"))?)?;
    let seed = rng.getattr(intern!(py, "seed"))?;
    if !seed.is_exact_instance_of::<PyInt>() {
        return unsupported("the seed is not an int");
    }
    let seed = seed.str()?;
    let deltas: Vec<Delta> = read.iter().map(|read| read.delta.clone()).collect();
    let inputs = core::Inputs {
        settings: &settings,
        expressiveness,
        view: rust_view.get().data(),
        deltas: &deltas,
        minute: minute_value,
        seed: seed.to_str()?,
        neighbors: &neighbors,
        traced,
    };
    let mut planned = with_world(world, |world| core::update(world, &inputs))?.or_py()?;
    if planned.first_use {
        // `_read_runtime_state(None)` of the first tick: the step makes the namespace.
        let state = PyDict::new(py);
        state.set_item(intern!(py, "schema_version"), SCHEMA_VERSION)?;
        state.set_item(intern!(py, "contacts"), PyDict::new(py))?;
        state.set_item(intern!(py, "agent_cooldown_until"), PyDict::new(py))?;
        world.getattr(intern!(py, "model_state"))?.set_item(NAME, state)?;
        with_world(world, |world| core::write(world, &mut planned))?.or_py()?;
    }
    if traced {
        let version = core::policy_version(stress).map_err(|bad| bad_error(controller, bad))?;
        core::record_social(&mut host, &planned, &<V as VPy>::from_py(minute)?, &version).map_err(|error| error.0)?;
    }

    // A delta that keeps its stress is itself, since a delta is frozen. The people are in id
    // order, as the Python rules sort them.
    let mut final_deltas = Vec::with_capacity(read.len());
    for (&place, &new_stress) in planned.order.iter().zip(&planned.plan.stresses) {
        let read = &read[place];
        final_deltas.push(if new_stress.to_bits() == read.delta.stress.to_bits() {
            read.object.clone()
        } else {
            with_stress(read, new_stress)?
        });
    }
    Ok(PyTuple::new(py, final_deltas)?.into_any())
}

// ---- For the bit comparisons of tests/integration/test_social_propagation_row.py ---------------

/// `plan` from plain values: `ids` sorted, `rows` of (group or None, zone, stress, sensitivity,
/// expressiveness, base stress), ints (max contacts, minimum minutes, cooldown) and floats (spread,
/// recovery, pair cap, agent cap, family, same zone, nearby). Returns the stresses, the contacts
/// and cooldowns as lists in order, and the observed pairs. It reads the state as a row does.
#[pyfunction]
#[allow(clippy::too_many_arguments, clippy::type_complexity)]
fn plan_of_values(
    policy: &str,
    seed: &str,
    minute: i64,
    ints: [i64; 3],
    floats: [f64; 7],
    ids: Vec<String>,
    rows: Vec<(Option<String>, String, f64, f64, f64, f64)>,
    neighbors: HashMap<String, Vec<String>>,
    previous: &Bound<'_, PyAny>,
    cooldowns: &Bound<'_, PyAny>,
) -> PyResult<(Vec<f64>, Vec<(String, i64, i64)>, Vec<(String, i64)>, Vec<(usize, usize)>)> {
    let misfit = |error: crate::core::world_core::Misfit| Unsupported::new_err(error.0);
    let previous = <IdMap<ContactRecord> as PyField>::from_py(previous).map_err(misfit)?;
    let cooldowns = <IdMap<i64> as PyField>::from_py(cooldowns).map_err(misfit)?;
    let settings = Settings {
        policy: match policy {
            "sparse" => Model::Sparse,
            "channel" => Model::Channel,
            "degroot" => Model::DeGroot,
            _ => return unsupported("a policy this kernel does not know"),
        },
        maximum: ints[0],
        minimum_contact_minutes: ints[1],
        cooldown_minutes: ints[2],
        spread: floats[0],
        recovery: floats[1],
        pair_cap: Num::Float(floats[2]),
        agent_cap: Num::Float(floats[3]),
        family_strength: floats[4],
        zone_strength: floats[5],
        nearby_strength: floats[6],
    };
    let people: Vec<Person<'_>> = ids
        .iter()
        .zip(&rows)
        .map(|(id, (group, zone, stress, sensitivity, expressiveness, base))| Person {
            id,
            group: group.as_deref(),
            zone,
            stress: *stress,
            sensitivity: *sensitivity,
            expressiveness: *expressiveness,
            base: *base,
        })
        .collect();
    let plan = core::plan(seed, minute, &settings, &people, &neighbors, &previous, &cooldowns, false).or_py()?;
    let new_cooldowns = plan.cooldowns.as_ref().unwrap_or(&cooldowns);
    Ok((
        plan.stresses,
        plan.contacts
            .iter()
            .map(|(key, record)| (key.to_string(), record.last_seen_minute, record.consecutive_minutes))
            .collect(),
        new_cooldowns.iter().map(|(id, due)| (id.to_string(), *due)).collect(),
        plan.observed,
    ))
}

pub fn register(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_function(wrap_pyfunction!(update_social_propagation, m)?)?;
    m.add_function(wrap_pyfunction!(plan_of_values, m)?)?;
    m.add_function(wrap_pyfunction!(start_draw_log, m)?)?;
    m.add_function(wrap_pyfunction!(take_draw_log, m)?)?;
    Ok(())
}
