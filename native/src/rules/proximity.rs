//! `_sample_proximity_contacts` (`experiments/phases/sample.py`) and the sampling of
//! `DistanceSampledProximityPolicy`: the glue of the core's kernel (`sheltersim_core::rules::
//! proximity`, lane c6 of Issue #152). The config and the policy are the Run's `Policies` (read when
//! the controller starts; a holder without a Run has them read now). The entry reads whether the
//! world holds `movement_v1`, runs the core on the world borrowed whole, and raises a policy whose
//! read raised as reading it again raises (policy E14).

use pyo3::intern;
use pyo3::prelude::*;
use pyo3::types::{PyDict, PyInt, PyList, PyString, PyTuple};
use sheltersim_core::rules::kit::POLICY_FAULT;
use sheltersim_core::rules::proximity::{self as core, Placed};

use crate::core::errors::OrPy;
use crate::core::run::policies_of;
use crate::core::value::{VPy, V};
use crate::core::world_core::with_world;
use crate::rules::travel::movement_held;
use crate::state::events::write_result;
use crate::Unsupported;

/// A refusal of the Run's policies whose read raised, as reading them again raises.
fn policy_fault<T>(controller: &Bound<'_, PyAny>, result: PyResult<T>) -> PyResult<T> {
    let py = controller.py();
    match result {
        Err(error) if error.is_instance_of::<Unsupported>(py) && error.value(py).to_string() == POLICY_FAULT => {
            controller.getattr(intern!(py, "proximity_contact_config"))?;
            controller.getattr(intern!(py, "proximity_policy"))?.getattr(intern!(py, "config"))?;
            Err(error)
        }
        result => result,
    }
}

/// `_sample_proximity_contacts`, the step of the sample phase.
#[pyfunction]
pub(crate) fn sample_contacts<'py>(controller: &Bound<'py, PyAny>, minute: &Bound<'py, PyAny>) -> PyResult<()> {
    let py = controller.py();
    let policies = policies_of(controller)?;
    let minute = minute.downcast_exact::<PyInt>().ok().and_then(|number| number.extract::<i64>().ok());
    let Some(minute) = policy_fault(controller, core::when(&policies.proximity, minute).or_py())? else {
        return Ok(());
    };
    let world = controller.getattr(intern!(py, "world"))?;
    let movement = movement_held(&world)?;
    let sampled = with_world(&world, |cell| core::sample_contacts(cell, &policies.proximity, minute, &movement))?;
    policy_fault(controller, write_result(py, sampled))
}

/// What `policy.sample(world)` returns, as `ProximityPair` fields, for the tests.
#[pyfunction]
fn sample_of<'py>(world: &Bound<'py, PyAny>, policy_config: &Bound<'py, PyAny>) -> PyResult<Bound<'py, PyList>> {
    let py = world.py();
    let config = <V as VPy>::from_py(policy_config)?;
    let movement = movement_held(world)?;
    let (points, pairs) = with_world(world, |cell| core::sample(cell, &config, &movement))?.or_py()?;
    let result = PyList::empty(py);
    let point_of = |point: &Placed| -> PyResult<Bound<'py, PyDict>> {
        let map = PyDict::new(py);
        map.set_item("x", point.x)?;
        map.set_item("y", point.y)?;
        Ok(map)
    };
    for (distance, first, second) in pairs {
        let (first, second) = (&points[first], &points[second]);
        result.append(PyTuple::new(
            py,
            [
                distance.into_pyobject(py)?.into_any(),
                PyString::new(py, &first.id).into_any(),
                PyString::new(py, &second.id).into_any(),
                PyString::new(py, &first.zone).into_any(),
                point_of(first)?.into_any(),
                point_of(second)?.into_any(),
            ],
        )?)?;
    }
    Ok(result)
}

pub fn register(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_function(wrap_pyfunction!(sample_contacts, m)?)?;
    m.add_function(wrap_pyfunction!(sample_of, m)?)?;
    Ok(())
}
