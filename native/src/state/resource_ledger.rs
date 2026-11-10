//! `resource_ledger_v1`: the row is the core's (`sheltersim_core::state::resource_ledger`); here are
//! its Python side and its validator, which reads the world's resources, people and zones.

use pyo3::intern;
use pyo3::prelude::*;
use pyo3::types::{PyInt, PyList, PyString};

pub(crate) use sheltersim_core::state::resource_ledger::*;

use crate::core::row::{attribute, load_row, namespace_is_absent, py_row, PyField, PyItems};
use crate::core::tracked::{as_readable_dict, as_readable_list};
use crate::core::world_core::{loads_items, mirror_of, raw_dict_get, Misfit, NamespaceMirror, Part, RowType, Write};
use crate::Unsupported;

py_row! { Reservation { reservation_id, resource_id, agent_id, quantity } }
py_row! { Account { initial, delivered, served, losses, remaining, reserved, source_ids, reservations } }
py_row! { Accounts { water_ml, food_meal_units } }
py_row! {
    AuditRecord {
        schema_version, minute, event_id, operation, resource_id, quantity, reservation_id, agent_id,
        source_id, source_ids, warehouse_id, initial, delivered, served, losses, remaining, reserved,
    }
}
py_row! { LedgerRow { schema_version, warehouse_id, zone_id, accounts, used_reservation_ids, audit } }

fn take(audit: &mut Audit, value: &Bound<'_, PyAny>) -> Result<(), Misfit> {
    audit.take(AuditRecord::from_py(value)?)
}

impl PyItems for Audit {
    fn slice_to_py<'py>(&self, py: Python<'py>, start: usize, stop: usize) -> PyResult<Bound<'py, PyList>> {
        loads_items(py, self.item_bytes()[start..stop].iter().map(|item| &**item))
    }
}

impl PyField for Audit {
    fn py_items(&self) -> Option<&dyn PyItems> {
        Some(self)
    }

    fn from_py(value: &Bound<'_, PyAny>) -> Result<Self, Misfit> {
        let list = as_readable_list(value).ok_or_else(|| Misfit::new("not a list"))?;
        let mut audit = Self::with_capacity(list.len());
        for item in list.iter() {
            take(&mut audit, &item)?;
        }
        Ok(audit)
    }

    fn to_py<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        let loads = py.import(intern!(py, "json"))?.getattr(intern!(py, "loads"))?;
        let list = PyList::empty(py);
        for item in self.item_bytes() {
            list.append(loads.call1((std::str::from_utf8(item)?,))?)?;
        }
        Ok(list.into_any())
    }

    fn apply(&mut self, path: &[Part<'_>], write: &Write<'_, '_>) -> Result<(), Misfit> {
        match (path, write) {
            ([], Write::Append { value }) => take(self, value),
            ([], Write::Extend { values }) => values.iter().try_for_each(|value| take(self, &value)),
            _ => Err(Misfit::new("the write does not fit the audit trail")),
        }
    }
}

impl PyField for UsedIds {
    fn from_py(value: &Bound<'_, PyAny>) -> Result<Self, Misfit> {
        Ok(Self::new(PyField::from_py(value)?))
    }

    fn to_py<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        PyField::to_py(self.ids(), py)
    }

    fn apply(&mut self, path: &[Part<'_>], write: &Write<'_, '_>) -> Result<(), Misfit> {
        self.edit(|ids| PyField::apply(ids, path, write))
    }
}

/// The world's dicts as `passes_rules_in` reads them: exact str keys only, so it runs no Python code.
struct WorldDicts<'a, 'py> {
    resources: Option<&'a Bound<'py, pyo3::types::PyDict>>,
    agents: &'a Bound<'py, PyAny>,
    zones: &'a Bound<'py, PyAny>,
}

impl LedgerWorld for WorldDicts<'_, '_> {
    type Error = PyErr;

    fn has_zone(&self, id: &str) -> PyResult<bool> {
        Ok(raw_dict_get(self.zones, PyString::new(self.zones.py(), id).as_any())?.is_some())
    }

    fn has_agent(&self, id: &str) -> PyResult<bool> {
        Ok(raw_dict_get(self.agents, PyString::new(self.agents.py(), id).as_any())?.is_some())
    }

    fn resource_count(&self) -> Option<usize> {
        self.resources.map(|resources| resources.len())
    }

    fn resource_is(&self, id: &str, expected: i64) -> PyResult<bool> {
        let Some(resources) = self.resources else {
            return Ok(false);
        };
        let Some(value) = raw_dict_get(resources.as_any(), PyString::new(resources.py(), id).as_any())? else {
            return Ok(false);
        };
        let Ok(number) = value.downcast_exact::<PyInt>() else {
            return Ok(false);
        };
        Ok(number.extract::<i64>().is_ok_and(|number| number == expected))
    }
}

/// `passes_rules_in` with the dicts of `world_inputs`.
pub(crate) trait LedgerChecks {
    fn passes_rules<'py>(
        &self,
        resources: &Bound<'py, PyAny>,
        agents: &Bound<'py, PyAny>,
        zones: &Bound<'py, PyAny>,
    ) -> PyResult<bool>;
}

impl LedgerChecks for LedgerRow {
    fn passes_rules<'py>(
        &self,
        resources: &Bound<'py, PyAny>,
        agents: &Bound<'py, PyAny>,
        zones: &Bound<'py, PyAny>,
    ) -> PyResult<bool> {
        self.passes_rules_in(&WorldDicts { resources: as_readable_dict(resources), agents, zones })
    }
}

/// The mirror of the ledger the world really holds: a row left behind by a write past the table
/// answers for nothing, so the kernels say Unsupported and the Python rules read what is there.
pub(crate) fn held_mirror<'py>(world: &Bound<'py, PyAny>) -> PyResult<Bound<'py, NamespaceMirror>> {
    let py = world.py();
    let mirror = mirror_of(world, NAME)?;
    let table = attribute(world, intern!(py, "model_state"))?;
    if as_readable_dict(&table).is_none() {
        return Err(Unsupported::new_err("model_state is another kind of mapping"));
    }
    let held = raw_dict_get(&table, intern!(py, "resource_ledger_v1").as_any())?;
    if !held.is_some_and(|held| mirror.try_borrow().is_ok_and(|mirror| mirror.holds_state(&held))) {
        return Err(Unsupported::new_err("the world holds another ledger"));
    }
    Ok(mirror)
}

/// The dicts a check reads, in the order `passes_rules` takes them.
pub(crate) fn world_inputs<'py>(
    world: &Bound<'py, PyAny>,
) -> PyResult<(Bound<'py, PyAny>, Bound<'py, PyAny>, Bound<'py, PyAny>)> {
    let py = world.py();
    let agents = attribute(world, intern!(py, "agents"))?;
    let zones = attribute(world, intern!(py, "zones"))?;
    if as_readable_dict(&agents).is_none() || as_readable_dict(&zones).is_none() {
        return Err(Unsupported::new_err("the world's maps are another kind of mapping"));
    }
    Ok((attribute(world, intern!(py, "resources"))?, agents, zones))
}

/// `validate_resource_ledger_state`: accepts, or leaves it to the Python rule and its error text.
#[pyfunction]
fn validate(world: &Bound<'_, PyAny>) -> PyResult<bool> {
    if namespace_is_absent(world, intern!(world.py(), NAME))? {
        return Ok(false);
    }
    let mirror = held_mirror(world)?;
    let (resources, agents, zones) = world_inputs(world)?;
    NamespaceMirror::with_row::<LedgerRow, _>(&mirror, |row| {
        LedgerChecks::passes_rules(row, &resources, &agents, &zones)
    })
}

pub fn register(m: &Bound<'_, PyModule>) -> PyResult<()> {
    let py = m.py();
    m.add("ROW", Py::new(py, RowType::new("resource_ledger", load_row::<LedgerRow>))?)?;
    m.add_function(wrap_pyfunction!(validate, m)?)?;
    Ok(())
}
