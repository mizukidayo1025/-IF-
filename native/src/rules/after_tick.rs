//! The PyO3 side of the end-of-tick check (`WorldState.validate_after_tick`): the core's
//! `rules::after_tick` (lane P8 d4 of Issue #152) holds the rules and the memo. This file reads what
//! only Python can say before the check (a rule patched, the maps and the table the world holds, the
//! plan of the configs from the Python contracts, which namespaces the world holds), answers the
//! core's `Checks` with the Python callables of the namespaces' validators, and makes the Python
//! values of the memo's facts.
//!
//! It holds no error text: a refusal leaves the tick to the Python check, `validate`, which passes or
//! raises its own (policy E6).

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use pyo3::exceptions::PyException;
use pyo3::intern;
use pyo3::prelude::*;
use pyo3::sync::GILOnceCell;
use pyo3::types::{PyBool, PyDict, PyFrozenSet, PyList, PyString};
use rustc_hash::FxHashSet;
use sheltersim_core::config::movement::RouteCapacities;
use sheltersim_core::config::{ResourceNumbers, Section, TickPlan};
use sheltersim_core::error::{Error as CoreError, Result as CoreResult};
use sheltersim_core::rules::after_tick::{self as core, Checks, HeldNamespaces, Inputs, Memo};

use crate::core::configs;
use crate::core::errors::OrPy;
use crate::core::row::{attribute, exact_int, exact_str, unsupported};
use crate::core::world_core::{core_of, field_held, namespace_held, raw_dict_get, with_world, NamespaceMirror, PyWorldCell};
use crate::state::entities::{entities_of, EntityStore};
use crate::rules::travel::read_queue_rows;
use crate::Unsupported;

/// What Python gives once, in `domain/simulator_state.py`.
struct Table {
    /// `(holder, name, as defined)` of what the Python check would call. A patch of one, or the
    /// `python_rules` mark, leaves the tick to the Python check.
    watched: Vec<(Py<PyAny>, Py<PyString>, Py<PyAny>)>,
    /// `_RUST_VALIDATORS`.
    validators: Py<PyDict>,
    rows_valid: Py<PyAny>,
    tick_valid: Py<PyAny>,
    effects_valid: Py<PyAny>,
    /// `core/tick_validation.py`: the contracts' reading of the configs.
    tick_plan: Py<PyAny>,
    body_config_ids: Py<PyAny>,
}

static TABLE: GILOnceCell<Table> = GILOnceCell::new();
static RAN: AtomicU64 = AtomicU64::new(0);
static REFUSALS: Mutex<BTreeMap<String, u64>> = Mutex::new(BTreeMap::new());

/// What the rules read from `model_configs`, from the contracts (`tick_plan`): the core's
/// `TickPlan`, which the configs keep by their stamp.
#[pyclass(frozen, module = "sheltersim_native.rules.after_tick")]
struct Plan {
    plan: TickPlan,
}

/// The parts of `ResourceServiceConfig` the entity rules read.
pub(crate) fn resource_numbers(config: &Bound<'_, PyAny>) -> PyResult<Option<ResourceNumbers>> {
    if config.is_none() {
        return Ok(None);
    }
    let py = config.py();
    let text = |name: &Bound<'_, PyString>| -> PyResult<Arc<str>> { Ok(Arc::from(exact_str(&attribute(config, name)?)?)) };
    Ok(Some(ResourceNumbers {
        water_ml_per_service: exact_int(&attribute(config, intern!(py, "water_ml_per_service"))?)?,
        food_meal_units_per_service: exact_int(&attribute(config, intern!(py, "food_meal_units_per_service"))?)?,
        water_staff_skill: text(intern!(py, "water_staff_skill"))?,
        meal_staff_skill: text(intern!(py, "meal_staff_skill"))?,
    }))
}

#[pymethods]
impl Plan {
    #[new]
    #[pyo3(signature = (*, declared, required, independent_queues, information_enabled, mean_reverting, shared_configured, operational_enabled, environment_zones, infrastructure_configured, operational_events, infrastructure_events, resource_config))]
    #[allow(clippy::too_many_arguments)]
    fn new(
        declared: Vec<(String, i64)>,
        required: Vec<String>,
        independent_queues: bool,
        information_enabled: bool,
        mean_reverting: bool,
        shared_configured: bool,
        operational_enabled: Option<bool>,
        environment_zones: Option<Vec<String>>,
        infrastructure_configured: bool,
        operational_events: &Bound<'_, PyFrozenSet>,
        infrastructure_events: &Bound<'_, PyFrozenSet>,
        resource_config: &Bound<'_, PyAny>,
    ) -> PyResult<Self> {
        let texts = |items: &Bound<'_, PyFrozenSet>| -> PyResult<FxHashSet<Arc<str>>> {
            items.iter().map(|item| Ok(Arc::from(item.extract::<String>()?))).collect()
        };
        Ok(Self {
            plan: TickPlan {
                declared: declared.into_iter().map(|(name, version)| (Arc::from(name), version)).collect(),
                required: required.into_iter().map(Arc::from).collect(),
                independent_queues,
                information_enabled,
                mean_reverting,
                shared_configured,
                operational_enabled,
                environment_zones: environment_zones.map(|zones| zones.into_iter().map(Arc::from).collect()),
                infrastructure_configured,
                operational_events: texts(operational_events)?,
                infrastructure_events: texts(infrastructure_events)?,
                resource_config: resource_numbers(resource_config)?,
            },
        })
    }
}

/// The facts of the last check this kernel passed for one world (`world.__dict__["_tick_memo"]`):
/// the core's `Memo`, and the parsed geometry configs of the Python rules.
#[pyclass(module = "sheltersim_native.rules.after_tick")]
pub struct TickMemo {
    memo: Memo,
    /// `geometry_overlay._CONFIG_SCOPE` across ticks while the configs are unchanged: the geometry
    /// configs the Python rules of the check parsed (`_parsed_config`). The kernels read the
    /// configs' section `geometry` instead (`core/geometry.rs`).
    #[pyo3(get)]
    geometry_scope: Py<PyDict>,
}

impl TickMemo {
    fn empty(py: Python<'_>) -> Self {
        Self { memo: Memo::default(), geometry_scope: PyDict::new(py).unbind() }
    }

    /// The core's memo, for the rules that read which requests may be open (`rules/metrics.rs`).
    pub(crate) fn memo(&self) -> &Memo {
        &self.memo
    }
}

fn sorted(ids: &Option<FxHashSet<Arc<str>>>) -> Option<Vec<&str>> {
    ids.as_ref().map(|ids| {
        let mut ids: Vec<&str> = ids.iter().map(|id| &**id).collect();
        ids.sort_unstable();
        ids
    })
}

#[pymethods]
impl TickMemo {
    #[new]
    fn new(py: Python<'_>) -> Self {
        Self::empty(py)
    }

    /// The minute of the last passed check; -1 before one.
    #[getter]
    fn minute(&self) -> i64 {
        self.memo.minute
    }

    /// A copied world holds new rows, so it starts with nothing proven.
    fn __deepcopy__(&self, py: Python<'_>, _memo: &Bound<'_, PyAny>) -> Self {
        Self::empty(py)
    }

    /// What the memo holds, for the tests: a dict of copies. The requests are their keys.
    fn facts<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyDict>> {
        let memo = &self.memo;
        let facts = PyDict::new(py);
        facts.set_item("minute", memo.minute)?;
        facts.set_item("planned", memo.configs.is_some())?;
        facts.set_item("agent_ids", sorted(&memo.agent_ids))?;
        facts.set_item("zone_ids", sorted(&memo.zone_ids))?;
        let zones = memo
            .facility_zones
            .as_ref()
            .map(|zones| -> PyResult<Bound<'py, PyDict>> {
                let mut zones: Vec<(&Arc<str>, &Arc<str>)> = zones.iter().collect();
                zones.sort_unstable();
                let out = PyDict::new(py);
                for (key, zone) in zones {
                    out.set_item(&**key, &**zone)?;
                }
                Ok(out)
            })
            .transpose()?;
        facts.set_item("facility_zones", zones)?;
        facts.set_item("requests", memo.requests.iter().map(|key| &**key).collect::<Vec<_>>())?;
        facts.set_item("open", memo.open.iter().map(|key| &**key).collect::<Vec<_>>())?;
        facts.set_item("body_config_ids", memo.body_config.as_ref().map(|(_, ids)| ids.iter().map(|id| &**id).collect::<Vec<_>>()))?;
        facts.set_item("seen", memo.seen.iter().map(|seen| seen.is_some()).collect::<Vec<bool>>())?;
        facts.set_item("presence_is_self_contained", memo.presence_is_self_contained)?;
        let unchanged = PyDict::new(py);
        for (name, fact) in ["configs", "presence", "lifecycle", "dynamics", "overlay"].iter().zip(memo.unchanged) {
            unchanged.set_item(name, fact)?;
        }
        facts.set_item("unchanged", unchanged)?;
        Ok(facts)
    }

    /// The requests of `world.support_requests` that may not be settled, in its order, or None if
    /// that is not known and the caller has to read them all. For scans during a tick
    /// (`unsettled_support_requests`). The result may hold requests settled since the last check;
    /// the caller reads their status.
    fn unsettled_requests<'py>(&self, world: &Bound<'py, PyAny>) -> PyResult<Option<Bound<'py, PyList>>> {
        let py = world.py();
        let Ok(Ok(Some(keys))) = with_world(world, |cell| self.memo.unsettled_requests(cell)) else {
            return Ok(None);
        };
        let requests = attribute(world, intern!(py, "support_requests"))?;
        let result = PyList::empty(py);
        for key in keys {
            let Some(request) = raw_dict_get(&requests, PyString::new(py, &key).as_any())? else {
                return Ok(None);
            };
            result.append(request)?;
        }
        Ok(Some(result))
    }
}

/// The Python callables the check asks, with the world given back (`Checks`). An error the Python
/// side raised is kept, for the check to report it as it is.
struct PyChecks<'a, 'py> {
    world: &'a Bound<'py, PyAny>,
    table: &'a Table,
    model_state: &'a Bound<'py, PyDict>,
    mirrors: &'a Bound<'py, PyAny>,
    raised: Option<PyErr>,
}

impl<'py> PyChecks<'_, 'py> {
    fn py(&self) -> Python<'py> {
        self.world.py()
    }

    /// `_rows_are_what_the_world_holds`, past its check of the entities, made once before the check.
    fn rows_hold(&self, name: &Bound<'py, PyString>) -> PyResult<bool> {
        let Some(mirror) = raw_dict_get(self.mirrors, name.as_any())? else {
            return Ok(true);
        };
        let mirror = mirror.downcast::<NamespaceMirror>()?;
        match raw_dict_get(self.model_state.as_any(), name.as_any())? {
            None => Ok(mirror.call_method0(intern!(self.py(), "row_json"))?.is_none()),
            Some(value) => Ok(mirror.try_borrow()?.holds_state(&value)),
        }
    }

    /// `rust_accepts`.
    fn accepts(&self, name: &str, kernel: &Bound<'py, PyAny>) -> PyResult<bool> {
        let name = PyString::intern(self.py(), name);
        if !self.rows_hold(&name)? {
            return Ok(false);
        }
        match kernel.call1((self.world,)) {
            Ok(answer) => Ok(answer.is_exact_instance_of::<PyBool>() && answer.is_truthy()?),
            Err(error) if error.is_instance_of::<Unsupported>(self.py()) => Ok(false),
            Err(error) => Err(error),
        }
    }

    /// The core's error for what Python raised, which is kept to be raised as it is.
    fn kept<T>(&mut self, result: PyResult<T>) -> CoreResult<T> {
        result.map_err(|error| {
            let text = error.value(self.py()).to_string();
            self.raised = Some(error);
            CoreError::unsupported(text)
        })
    }
}

impl Checks for PyChecks<'_, '_> {
    fn namespace(&mut self, name: &str) -> CoreResult<Option<bool>> {
        let kernel = self.table.validators.bind(self.py()).get_item(name);
        let answer = match kernel {
            Ok(None) => Ok(None),
            Ok(Some(kernel)) => self.accepts(name, &kernel).map(Some),
            Err(error) => Err(error),
        };
        self.kept(answer)
    }

    fn body_rows(&mut self) -> CoreResult<bool> {
        let answer = self.accepts(core::BODY, self.table.rows_valid.bind(self.py()));
        self.kept(answer)
    }

    fn shared_operations(&mut self) -> CoreResult<bool> {
        let answer = self.accepts(core::SHARED, self.table.tick_valid.bind(self.py()));
        self.kept(answer)
    }

    fn effects(&mut self) -> CoreResult<()> {
        let py = self.py();
        let answer = (|| {
            let options = PyDict::new(py);
            options.set_item(intern!(py, "validate_event_history"), false)?;
            self.table.effects_valid.bind(py).call((self.world,), Some(&options)).map(|_| ())
        })();
        self.kept(answer)
    }

    fn body_config_ids(&mut self) -> CoreResult<Vec<Arc<str>>> {
        let answer = (|| {
            let named: Vec<String> = self.table.body_config_ids.bind(self.py()).call1((self.world,))?.extract()?;
            Ok(named.into_iter().map(Arc::from).collect())
        })();
        self.kept(answer)
    }
}

/// Whether the Python world holds each namespace the check reads.
fn held_namespaces(world: &Bound<'_, PyAny>, plan: &TickPlan) -> PyResult<HeldNamespaces> {
    let py = world.py();
    let mut held = HeldNamespaces::default();
    let names = core::READ.iter().copied().chain(plan.declared.iter().map(|(name, _)| &**name)).chain(plan.required.iter().map(|name| &**name));
    for name in names {
        let found = namespace_held(world, &PyString::new(py, name), "the world holds another tree than the namespace's")?;
        held.insert(Arc::from(name), found);
    }
    Ok(held)
}

fn check(table: &Table, world: &Bound<'_, PyAny>, memo: &Bound<'_, TickMemo>) -> PyResult<()> {
    let py = world.py();
    for (holder, name, defined) in &table.watched {
        if !holder.bind(py).getattr(name.bind(py))?.is(defined.bind(py)) {
            return unsupported("a rule of the check is patched");
        }
    }

    // ---- What `_rows_are_what_the_world_holds` asks of the whole world: the rows the core reads are
    // what the world holds.
    let world_state = attribute(world, intern!(py, "__dict__"))?;
    let world_state = world_state.downcast::<PyDict>()?;
    core_of(world)?;
    let store = entities_of(world)?;
    if !EntityStore::follows_every_map(&store, world_state)? {
        return unsupported("the maps of the world are not the ones the rows follow");
    }
    let Some(model_state) = raw_dict_get(world_state, intern!(py, "model_state"))? else {
        return unsupported("the world has no model state");
    };
    if !crate::core::tracked::is_table_type(model_state.get_type().as_ptr().cast()) {
        return unsupported("the model state is not the table");
    }
    let model_state = model_state.downcast::<PyDict>()?;
    let mirrors = attribute(model_state, intern!(py, "_mirrors"))?;
    for name in [intern!(py, "resources"), intern!(py, "id_counters")] {
        if !field_held(world, name)? {
            return unsupported("a field of the world is not the one the row follows");
        }
    }

    // ---- The configs: the plan of the configs the world holds, kept for their stamp with the other
    // sections the kernels read (`core/configs.rs`).
    let held = configs::current(world)?;
    let Some(stamp) = held.stamp else {
        return unsupported("no mirror follows the configs");
    };
    let plan = match held.tick_plan.get_or_parse(|| configs::section_of(py, plan_of(table, world))) {
        Section::Ok(plan) => plan.clone(),
        Section::Fault(_) => return Err(configs::reraise(|| plan_of(table, world))),
        Section::Absent => return unsupported("no plan"),
    };
    if memo.borrow().memo.configs != Some(stamp) {
        memo.borrow().geometry_scope.bind(py).clear();
    }
    let route_capacities = configs::kept_section(world, |configs| &configs.route_capacities, RouteCapacities::parse)?;
    let overlay_schema = crate::rules::overlay::schema_now(world)?;
    let namespaces = held_namespaces(world, &plan)?;
    // The entity rules read the pending events as rows: those held only as objects get theirs.
    read_queue_rows(world)?;
    let inputs = Inputs { plan: &plan, configs: stamp, held: &namespaces, route_capacities: &route_capacities, overlay_schema: &overlay_schema };

    // The memo is taken out while Python is asked, and put back whatever the answer.
    let mut kept = std::mem::take(&mut memo.borrow_mut().memo);
    let mut checks = PyChecks { world, table, model_state, mirrors: &mirrors, raised: None };
    let answer = core::check(&PyWorldCell(world), &mut kept, &inputs, &mut checks);
    memo.borrow_mut().memo = kept;
    match (answer, checks.raised) {
        (Ok(()), _) => Ok(()),
        (Err(_), Some(raised)) => Err(raised),
        (Err(error), None) => Err(error).or_py(),
    }
}

/// The plan of the configs the world holds, from the contracts, which raise for configs the check
/// would turn down.
fn plan_of(table: &Table, world: &Bound<'_, PyAny>) -> PyResult<Arc<TickPlan>> {
    let plan = table.tick_plan.bind(world.py()).call1((world,))?;
    Ok(Arc::new(plan.downcast_exact::<Plan>()?.get().plan.clone()))
}

/// Fill the plan of these configs, the world's now, as the check does at its first read (the host's
/// `fill_section`, lane P9-0).
pub(crate) fn fill_tick_plan(world: &Bound<'_, PyAny>, configs: &sheltersim_core::config::Configs) -> PyResult<()> {
    let py = world.py();
    let Some(table) = TABLE.get(py) else {
        return unsupported("domain/simulator_state.py has not installed the table");
    };
    let _ = configs.tick_plan.get_or_parse(|| configs::section_of(py, plan_of(table, world)));
    Ok(())
}

/// The check, with the memo moved on, or Unsupported with the memo as it was but for the configs'
/// stamp.
#[pyfunction]
fn after_tick(world: &Bound<'_, PyAny>, memo: &Bound<'_, TickMemo>) -> PyResult<()> {
    let py = world.py();
    let answer = match TABLE.get(py) {
        None => unsupported("domain/simulator_state.py has not installed the table"),
        Some(table) => check(table, world, memo),
    };
    match answer {
        Ok(()) => {
            RAN.fetch_add(1, Ordering::Relaxed);
            Ok(())
        }
        // A rule or contract that raises, called from here or read here, is the Python check's to raise.
        Err(error) if error.is_instance_of::<PyException>(py) => {
            let reason = error.value(py).to_string();
            if let Ok(mut refusals) = REFUSALS.lock() {
                *refusals.entry(reason.clone()).or_default() += 1;
            }
            Err(Unsupported::new_err(reason))
        }
        Err(error) => Err(error),
    }
}

/// What `domain/simulator_state.py` gives once.
#[pyfunction]
fn install(
    py: Python<'_>,
    watched: Vec<(Py<PyAny>, Py<PyString>)>,
    validators: Py<PyDict>,
    rows_valid: Py<PyAny>,
    tick_valid: Py<PyAny>,
    effects_valid: Py<PyAny>,
    configs: (Py<PyAny>, Py<PyAny>),
) -> PyResult<()> {
    let mut defined = Vec::with_capacity(watched.len());
    for (holder, name) in watched {
        let value = holder.bind(py).getattr(name.bind(py))?.unbind();
        defined.push((holder, name, value));
    }
    let (tick_plan, body_config_ids) = configs;
    TABLE.get_or_init(py, || Table { watched: defined, validators, rows_valid, tick_valid, effects_valid, tick_plan, body_config_ids });
    Ok(())
}

/// What has run in this process: `(checks taken, refusals by reason)`.
#[pyfunction]
fn stats<'py>(py: Python<'py>) -> PyResult<(u64, Bound<'py, PyDict>)> {
    let refusals = PyDict::new(py);
    if let Ok(known) = REFUSALS.lock() {
        for (reason, count) in known.iter() {
            refusals.set_item(reason, count)?;
        }
    }
    Ok((RAN.load(Ordering::Relaxed), refusals))
}

/// The keys of the support requests whose rows were written or filled since the last passed check,
/// in the order of their first write: what the memo reads again (for the tests).
#[pyfunction]
fn written_requests(world: &Bound<'_, PyAny>) -> PyResult<Vec<String>> {
    with_world(world, |cell| -> CoreResult<Vec<String>> {
        let rows = cell.rows()?;
        let table = &rows.support_requests;
        let mut keys: Vec<(u32, &str)> = table.slots().filter(|(_, slot)| table.is_marked(*slot)).map(|(key, slot)| (slot, &**key)).collect();
        let order: Vec<u32> = table.marked().to_vec();
        keys.sort_by_key(|(slot, _)| order.iter().position(|marked| marked == slot));
        Ok(keys.into_iter().map(|(_, key)| key.to_owned()).collect())
    })?
    .or_py()
}

pub fn register(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_class::<Plan>()?;
    m.add_class::<TickMemo>()?;
    m.add_function(wrap_pyfunction!(after_tick, m)?)?;
    m.add_function(wrap_pyfunction!(install, m)?)?;
    m.add_function(wrap_pyfunction!(stats, m)?)?;
    m.add_function(wrap_pyfunction!(written_requests, m)?)?;
    Ok(())
}
