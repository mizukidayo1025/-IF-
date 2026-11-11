//! The PyO3 side of the entity rules of the end-of-tick check and of the validators of
//! `population_dynamics_v1` and `population_presence_v1`: the rules are the core's
//! `rules::tick_validation` (lane P8 d4 of Issue #152), which reads the rows. This file reads what
//! only Python can say (the maps and namespaces the world holds, the parsed resource config), calls
//! the core with the world borrowed, and makes the Python values.
//!
//! None of these has an error text: a refusal (`Unsupported`) leaves the state to the Python rules,
//! which pass or raise their own (policy E6).

use pyo3::intern;
use pyo3::prelude::*;
use pyo3::types::{PyDict, PyList, PyString};
use sheltersim_core::config::movement::RouteCapacities;
use sheltersim_core::rules::kit::Held;
use sheltersim_core::rules::tick_validation::{self as core, Dynamics, EntityInputs, Requests};

use crate::core::configs;
use crate::core::errors::OrPy;
use crate::core::row::{attribute, unsupported};
use crate::core::world_core::{core_of, field_held, namespace_held, with_world};
use crate::rules::after_tick::resource_numbers;
use crate::rules::travel::read_queue_rows;
use crate::state::entities::{entities_of, EntityStore};

fn held(world: &Bound<'_, PyAny>, name: &'static str) -> PyResult<Held> {
    namespace_held(world, &PyString::intern(world.py(), name), "the world holds another tree than the namespace's")
}

/// The entity and event rules of `validate`, for a WorldState v5 or v6 at the end of a tick, of every
/// support request: the rows the world holds pass them, or this says `Unsupported`.
/// `resource_config` is the parsed `resource_service` config of a v6 world and None for a v5 one.
/// The result lists `(zone_id, occupancy)` of the zones whose capacity is the overlay's rule: their
/// temporary spaces and rooms count too.
#[pyfunction]
fn entity_rules<'py>(world: &Bound<'py, PyAny>, resource_config: &Bound<'py, PyAny>) -> PyResult<Bound<'py, PyList>> {
    let py = world.py();
    core_of(world)?;
    let state = attribute(world, intern!(py, "__dict__"))?;
    if !EntityStore::follows_every_map(&entities_of(world)?, state.downcast::<PyDict>()?)? {
        return unsupported("the maps of the world are not the ones the rows follow");
    }
    for name in [intern!(py, "resources"), intern!(py, "id_counters")] {
        if !field_held(world, name)? {
            return unsupported("a field of the world is not the one the row follows");
        }
    }
    let resource_config = resource_numbers(resource_config)?;
    let route_capacities = configs::kept_section(world, |configs| &configs.route_capacities, RouteCapacities::parse)?;
    let (ledger, service, spaces, overlay) = (held(world, core::LEDGER)?, held(world, core::SERVICE)?, held(world, core::SPACES)?, held(world, core::OVERLAY)?);
    let inputs = EntityInputs {
        requests: Requests::All,
        resource_config: resource_config.as_ref(),
        route_capacities: &route_capacities,
        ledger: &ledger,
        service: &service,
        spaces: &spaces,
        overlay: &overlay,
    };
    // The rules read the pending events as rows: those held only as objects get theirs.
    read_queue_rows(world)?;
    let left = with_world(world, |cell| core::entity_rules(cell, &inputs, &mut |_| {}))?.or_py()?;
    let out = PyList::empty(py);
    for (zone, occupancy) in left {
        out.append((PyString::new(py, &zone), occupancy))?;
    }
    Ok(out)
}

/// `validate_population_dynamics_state` of a state with no dynamic context and no one pending, the
/// parts of it that read `PersonContext`, `PersonTraits` and the approach routes; with someone
/// pending or a context, the population's own validation (`rules/population.rs`), whose ValueError
/// is the Python rule's to raise: this says Unsupported then.
#[pyfunction]
fn population_dynamics(world: &Bound<'_, PyAny>) -> PyResult<bool> {
    let dynamics = held(world, core::DYNAMICS)?;
    match with_world(world, |cell| core::population_dynamics(cell, &dynamics))?.or_py()? {
        Dynamics::Taken => Ok(true),
        Dynamics::Validate => {
            crate::rules::population::as_refusal(crate::rules::population::validate_population_dynamics_state(world))?;
            Ok(true)
        }
    }
}

/// `validate_presence_state` (`contracts/presence.py`), the validator of the declared namespace
/// `population_presence_v1`: true, or Unsupported for the contract and the rule to say.
#[pyfunction]
fn validate_presence(world: &Bound<'_, PyAny>) -> PyResult<bool> {
    let (presence, lifecycle) = (held(world, core::PRESENCE)?, held(world, core::LIFECYCLE)?);
    with_world(world, |cell| core::validate_presence(cell, &presence, &lifecycle))?.or_py()?;
    Ok(true)
}

pub fn register(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_function(wrap_pyfunction!(entity_rules, m)?)?;
    m.add_function(wrap_pyfunction!(population_dynamics, m)?)?;
    m.add_function(wrap_pyfunction!(validate_presence, m)?)?;
    Ok(())
}
