//! The glue of the core's view (`sheltersim_core::rules::world_view`, lane P8 a1): the phases'
//! steps that make a `WorldView`, and the Python objects of a view.
//!
//! `view_after_movement` and `view_for_proposals` are the two steps of the phases that make a
//! view, which have no Python body (experiments/phases/segments.py, KERNEL_ONLY_SEGMENTS). They
//! gather what only Python knows (the model selection, `world.model_configs` while the world's slot
//! does not hold it, `simulator_extensions.enabled`), call the core with the world borrowed, and
//! return a real `WorldView` whose `agents`, `facilities`, `zone_members` and `group_members` are
//! made when a Python reader first asks (`WorldView.__getattr__`), from the core's `ViewData` kept
//! in its `_rust_view`. What the core turns down says `Unsupported` before anything has changed,
//! and the step raises KernelRefusal.
//!
//! What the view keeps from one view to the next (parsed person contexts, zone conditions) is in
//! the controller's `Run` (`controller._run_core`), by the stamps of the slots it was read from; a
//! holder without one (a test's namespace) keeps nothing. The people's IDs as Python strings are
//! kept there too.
//!
//! The psychology and social kernels take a view's snapshot through `view_of`; one Python made (a
//! Rust view was refused, or a test made it) is read here (`from_python_view`).

use std::sync::Arc;

use pyo3::intern;
use pyo3::prelude::*;
use pyo3::sync::GILOnceCell;
use pyo3::types::{PyDict, PyFloat, PyInt, PyString, PyTuple, PyType};
use pyo3::IntoPyObjectExt;
use sheltersim_core::config::{Fault, Selection};
use sheltersim_core::world::MODEL_CONFIGS;
use sheltersim_core::rules::world_view::{self as view, Inputs, ViewCache};

pub(crate) use sheltersim_core::rules::world_view::{
    status_text, BodyFields, DemandModifier, PersonFields, ViewAgent, ViewData, ViewZone,
};

use crate::core::configs;
use crate::core::errors::OrPy;
use crate::core::run::RunCore;
use crate::core::tracked::as_readable_dict;
use crate::core::value::{VPy, V};
use crate::core::world_core::{raw_dict_get, raw_dict_set, with_world};
use crate::host::PyHost;
use crate::state::entities::{raw_setattr, Activity, Member, Mobility, NeedRow};
use crate::Unsupported;

#[track_caller]
fn unsupported<T>(reason: &'static str) -> PyResult<T> {
    Err(Unsupported::new_err(reason))
}

// ---- The inputs only Python has ----------------------------------------------------------------

/// What a view reads with Python before the world is borrowed.
struct Gathered {
    selection: Result<Arc<Selection>, Fault>,
    /// `world.model_configs` as a `V` while the slot does not hold it (assigned whole).
    configs: Option<V>,
}

impl Gathered {
    fn inputs(&self) -> Inputs<'_> {
        Inputs { selection: &self.selection, configs: self.configs.as_ref() }
    }
}

/// `world.model_configs`, which must be a plain dict.
fn configs_of<'py>(world: &Bound<'py, PyAny>) -> PyResult<Bound<'py, PyAny>> {
    let configs = world.getattr(intern!(world.py(), "model_configs"))?;
    if as_readable_dict(&configs).is_none() {
        return unsupported("the configs are not a plain dict");
    }
    Ok(configs)
}

fn gather(world: &Bound<'_, PyAny>) -> PyResult<Gathered> {
    let raw = configs_of(world)?;
    let held = PyHost::of_entry(world).holding_now()?.field(MODEL_CONFIGS);
    let configs = if held { None } else { Some(<V as VPy>::from_py(&raw)?) };
    Ok(Gathered { selection: configs::selection(world)?, configs })
}

/// `misinformation_applies` of a world, for the shadow.
#[pyfunction]
fn misinformation_applies_of(world: &Bound<'_, PyAny>) -> PyResult<bool> {
    configs_of(world)?;
    view::misinformation_applies(&configs::selection(world)?).or_py()
}

// ---- Making a view -----------------------------------------------------------------------------

/// `controller._run_core`, if it has one.
fn run_of<'py>(controller: &Bound<'py, PyAny>) -> Option<Bound<'py, RunCore>> {
    let found = controller.getattr(intern!(controller.py(), "_run_core")).ok()?;
    found.downcast_into_exact::<RunCore>().ok()
}

/// `from_world` without its hash: the core's `build`, with what the Run keeps, then the extensions'
/// flag, which `from_world` read last.
fn build(run: Option<&Bound<'_, RunCore>>, world: &Bound<'_, PyAny>, gathered: &Gathered) -> PyResult<ViewData> {
    let mut cache = match run {
        Some(run) => run.get().take_views()?,
        None => ViewCache::default(),
    };
    let inputs = gathered.inputs();
    let built = with_world(world, |world| view::build(world, &inputs, &mut cache));
    if let Some(run) = run {
        run.get().put_views(cache)?;
    }
    let draft = built?.or_py()?;
    Ok(draft.finish(crate::core::configs::extensions_enabled_of(world)?))
}

/// The people's IDs of a view as Python strings, in its order.
fn keys_of(py: Python<'_>, run: Option<&Bound<'_, RunCore>>, data: &ViewData) -> PyResult<Vec<Py<PyString>>> {
    let ids = data.agents.iter().map(|agent| &agent.id);
    match run {
        Some(run) => run.get().keys(py, ids),
        None => Ok(ids.map(|id| PyString::new(py, id).unbind()).collect()),
    }
}

/// `_view_after_movement` of the update phase (experiments/phases/segments.py).
#[pyfunction]
pub(crate) fn view_after_movement<'py>(controller: &Bound<'py, PyAny>, world: &Bound<'py, PyAny>) -> PyResult<Bound<'py, PyAny>> {
    let py = world.py();
    let gathered = gather(world)?;
    let run = run_of(controller);
    let data = build(run.as_ref(), world, &gathered)?;
    let keys = keys_of(py, run.as_ref(), &data)?;
    new_view(py, data, keys, None)
}

/// `_view_for_proposals` of the propose phase: the update phase's view of this minute with what
/// that phase wrote read again, or a new view; with its audit hash.
#[pyfunction]
pub(crate) fn view_for_proposals<'py>(
    controller: &Bound<'py, PyAny>,
    world: &Bound<'py, PyAny>,
    minute: &Bound<'py, PyAny>,
) -> PyResult<Bound<'py, PyAny>> {
    let py = world.py();
    let slot = intern!(py, "_view_before_proposals");
    let prior = controller.getattr(slot)?;
    let gathered = gather(world)?;
    let (data, keys) = if !prior.is_none() && prior.getattr(intern!(py, "minute_index"))?.eq(minute)? {
        let fields = prior.getattr(intern!(py, "__dict__"))?;
        let Some(rust) = raw_dict_get(&fields, intern!(py, "_rust_view").as_any())? else {
            return unsupported("the prior view is Python's");
        };
        let rust = rust.downcast_into::<RustView>()?;
        let prior = rust.get();
        let data = with_world(world, |world| view::refresh(&prior.data, world))?.or_py()?;
        (data, prior.keys.iter().map(|key| key.clone_ref(py)).collect())
    } else {
        let run = run_of(controller);
        let data = build(run.as_ref(), world, &gathered)?;
        let keys = keys_of(py, run.as_ref(), &data)?;
        (data, keys)
    };
    let hash = with_world(world, |world| view::state_hash(&data, world, gathered.configs.as_ref()))?.or_py()?;
    let view = new_view(py, data, keys, Some(hash))?;
    controller.setattr(slot, py.None())?;
    Ok(view)
}

// ---- The Python view ----------------------------------------------------------------------------

/// The snapshot behind a `WorldView`, in its `_rust_view`.
#[pyclass(module = "sheltersim_native", frozen)]
pub struct RustView {
    data: Arc<ViewData>,
    /// The people's IDs as Python strings, whose hashes are kept, in the order of `data.agents`.
    keys: Vec<Py<PyString>>,
}

impl RustView {
    pub(crate) fn data(&self) -> &ViewData {
        &self.data
    }

    /// The snapshot, shared (the Run's scratch keeps it, lane P9-2a).
    pub(crate) fn shared(&self) -> Arc<ViewData> {
        self.data.clone()
    }

    pub(crate) fn key<'py>(&self, py: Python<'py>, index: usize) -> &Bound<'py, PyString> {
        self.keys[index].bind(py)
    }
}

struct PythonHelpers {
    world_view: Py<PyType>,
    need_view: Py<PyType>,
    agent_view: Py<PyType>,
    facility_view: Py<PyType>,
    demand_view: Py<PyType>,
    new: Py<PyAny>,
}

static HELPERS: GILOnceCell<PythonHelpers> = GILOnceCell::new();

fn helpers(py: Python<'_>) -> PyResult<&PythonHelpers> {
    HELPERS.get_or_try_init(py, || {
        let module = py.import("sheltersim.policies.world_view")?;
        let kind = |name: &str| -> PyResult<Py<PyType>> { Ok(module.getattr(name)?.downcast_into::<PyType>()?.unbind()) };
        Ok(PythonHelpers {
            world_view: kind("WorldView")?,
            need_view: kind("NeedView")?,
            agent_view: kind("AgentView")?,
            facility_view: kind("FacilityView")?,
            demand_view: py.import("sheltersim.domain.environment")?.getattr("ZoneDemandView")?.downcast_into::<PyType>()?.unbind(),
            new: py.import("builtins")?.getattr("object")?.getattr("__new__")?.unbind(),
        })
    })
}

fn tuple_of<'py>(py: Python<'py>, items: &[Arc<str>]) -> PyResult<Bound<'py, PyAny>> {
    if items.is_empty() {
        return Ok(PyTuple::empty(py).into_any());
    }
    Ok(PyTuple::new(py, items.iter().map(|item| &**item))?.into_any())
}

fn option_text<'py>(py: Python<'py>, text: &Option<Arc<str>>) -> Bound<'py, PyAny> {
    match text {
        Some(text) => PyString::new(py, text).into_any(),
        None => py.None().into_bound(py),
    }
}

fn option_int<'py>(py: Python<'py>, value: Option<i64>) -> PyResult<Bound<'py, PyAny>> {
    match value {
        Some(value) => value.into_bound_py_any(py),
        None => Ok(py.None().into_bound(py)),
    }
}

fn new_object<'py>(helpers: &PythonHelpers, kind: &Py<PyType>, fields: &Bound<'py, PyDict>) -> PyResult<Bound<'py, PyAny>> {
    let py = fields.py();
    let object = helpers.new.bind(py).call1((kind.bind(py),))?;
    raw_setattr(&object, intern!(py, "__dict__"), fields.as_ptr())?;
    Ok(object)
}

/// What every person of a zone shares, in the order `from_world` gives their fields.
fn zone_python<'py>(helpers: &PythonHelpers, py: Python<'py>, zone: &ViewZone) -> PyResult<Bound<'py, PyAny>> {
    let dict = PyDict::new(py);
    let any = dict.as_any();
    macro_rules! set {
        ($name:literal, $value:expr) => {
            raw_dict_set(any, intern!(py, $name).as_any(), &$value)?
        };
    }
    let float = |value: f64| PyFloat::new(py, value).into_any();
    set!("environmental_stress_per_minute", float(zone.environmental_stress_per_minute));
    set!("environmental_stressor_event_ids", tuple_of(py, &zone.environmental_stressor_event_ids)?);
    set!("environmental_assumption_source_id", option_text(py, &zone.environmental_assumption_source_id));
    set!("ambient_noise_level", float(zone.ambient_noise_level));
    set!("rest_efficiency_multiplier", float(zone.rest_efficiency_multiplier));
    set!("zone_environment_modifier_event_ids", tuple_of(py, &zone.zone_environment_modifier_event_ids)?);
    set!("zone_environment_assumption_ids", tuple_of(py, &zone.zone_environment_assumption_ids)?);
    set!("ventilation_effectiveness", float(zone.ventilation_effectiveness));
    set!("lighting_level", float(zone.lighting_level));
    set!("privacy_level", float(zone.privacy_level));
    set!("temperature_c", float(zone.temperature_c));
    set!("ventilation_hygiene_growth_per_minute", float(zone.ventilation_hygiene_growth_per_minute));
    set!("privacy_discontent_per_minute", float(zone.privacy_discontent_per_minute));
    set!("condition_modifier_event_ids", tuple_of(py, &zone.condition_modifier_event_ids)?);
    set!("condition_assumption_ids", tuple_of(py, &zone.condition_assumption_ids)?);
    let mut demand = Vec::with_capacity(zone.demand_modifiers.len());
    for modifier in &zone.demand_modifiers {
        demand.push(demand_python(helpers, py, modifier)?);
    }
    set!("demand_modifiers", PyTuple::new(py, demand)?.into_any());
    set!("min_rest_decision_minutes", zone.min_rest_decision_minutes.into_bound_py_any(py)?);
    set!("max_rest_decision_minutes", zone.max_rest_decision_minutes.into_bound_py_any(py)?);
    set!("sanitation_level", float(zone.sanitation_level));
    Ok(dict.into_any())
}

/// A `ZoneDemandView`.
fn demand_python<'py>(helpers: &PythonHelpers, py: Python<'py>, modifier: &DemandModifier) -> PyResult<Bound<'py, PyAny>> {
    let fields = PyDict::new(py);
    fields.set_item(intern!(py, "action"), &*modifier.action)?;
    fields.set_item(intern!(py, "probability_multiplier"), modifier.probability_multiplier)?;
    fields.set_item(intern!(py, "duration_multiplier"), modifier.duration_multiplier)?;
    fields.set_item(intern!(py, "modifier_event_ids"), tuple_of(py, &modifier.modifier_event_ids)?)?;
    fields.set_item(intern!(py, "assumption_ids"), tuple_of(py, &modifier.assumption_ids)?)?;
    new_object(helpers, &helpers.demand_view, &fields)
}

fn copy_dict<'py>(dict: &Bound<'py, PyAny>) -> PyResult<Bound<'py, PyAny>> {
    // SAFETY: dict is a live dict; the call returns a new reference or sets an error.
    unsafe {
        let copy = pyo3::ffi::PyDict_Copy(dict.as_ptr());
        Bound::from_owned_ptr_or_err(dict.py(), copy)
    }
}

fn members_python<'py>(py: Python<'py>, members: Vec<(&Arc<str>, Vec<usize>)>, keys: &[Py<PyString>]) -> PyResult<Bound<'py, PyDict>> {
    let dict = PyDict::new(py);
    for (name, people) in members {
        let tuple = PyTuple::new(py, people.iter().map(|index| keys[*index].bind(py)))?;
        dict.set_item(&**name, tuple)?;
    }
    Ok(dict)
}

#[pymethods]
impl RustView {
    /// (agents, facilities, zone_members, group_members) as `WorldView` holds them, each as a
    /// plain dict for it to wrap. Each call makes new objects.
    fn python_fields<'py>(
        &self,
        py: Python<'py>,
    ) -> PyResult<(Bound<'py, PyDict>, Bound<'py, PyDict>, Bound<'py, PyDict>, Bound<'py, PyDict>)> {
        let helpers = helpers(py)?;
        let data = &self.data;
        let mut zones = Vec::with_capacity(data.zones.len());
        for zone in &data.zones {
            zones.push(zone_python(helpers, py, zone)?);
        }
        let zone_ids: Vec<Bound<'py, PyString>> = data.zones.iter().map(|zone| PyString::new(py, &zone.id)).collect();
        let float = |value: f64| PyFloat::new(py, value).into_any();
        let enabled = data.resource_service_enabled.into_bound_py_any(py)?;
        let agents = PyDict::new(py);
        for (agent, key) in data.agents.iter().zip(&self.keys) {
            let fields = copy_dict(&zones[agent.zone])?;
            let any = &fields;
            macro_rules! set {
                ($name:literal, $value:expr) => {
                    raw_dict_set(any, intern!(py, $name).as_any(), &$value)?
                };
            }
            let person = &agent.person;
            set!("caregiver_ids", tuple_of(py, &person.caregiver_ids)?);
            set!("care_for_ids", tuple_of(py, &person.care_for_ids)?);
            set!("parent_ids", tuple_of(py, &person.parent_ids)?);
            set!("roles", tuple_of(py, &person.roles)?);
            set!("usual_wake_minute", option_int(py, person.usual_wake_minute)?);
            set!("usual_sleep_minute", option_int(py, person.usual_sleep_minute)?);
            set!("required_sleep_minutes", option_int(py, person.required_sleep_minutes)?);
            set!("personal_context_present", person.personal_context_present.into_bound_py_any(py)?);
            let key = key.bind(py);
            set!("id", key.as_any());
            set!("group_id", option_text(py, &agent.group_id));
            set!("zone_id", zone_ids[agent.zone].as_any());
            set!("mobility", agent.mobility.member(py)?);
            set!("support_need", float(agent.support_need));
            set!("stress_sensitivity", float(agent.stress_sensitivity));
            set!("rng_key", PyString::new(py, &agent.rng_key).into_any());
            set!("activity", agent.activity.member(py)?);
            set!("target_id", option_text(py, &agent.target_id));
            let needs = PyDict::new(py);
            let needs_any = needs.as_any();
            raw_dict_set(needs_any, intern!(py, "toilet_urgency").as_any(), &float(agent.needs.toilet_urgency))?;
            raw_dict_set(needs_any, intern!(py, "hunger").as_any(), &float(agent.needs.hunger))?;
            raw_dict_set(needs_any, intern!(py, "fatigue").as_any(), &float(agent.needs.fatigue))?;
            raw_dict_set(needs_any, intern!(py, "hydration_need").as_any(), &float(agent.needs.hydration_need))?;
            set!("needs", new_object(helpers, &helpers.need_view, &needs)?);
            set!("stress", float(agent.stress));
            set!("chronic_distress", float(agent.chronic_distress));
            set!("trust", float(agent.trust));
            set!("next_decision_at", agent.next_decision_at.into_bound_py_any(py)?);
            set!("resource_service_enabled", enabled);
            set!("queue_task_id", option_text(py, &agent.queue_task_id));
            set!("queue_kind", option_text(py, &agent.queue_kind));
            set!("queue_target_id", option_text(py, &agent.queue_target_id));
            set!("queue_wait_minutes", agent.queue_wait_minutes.into_bound_py_any(py)?);
            set!("support_request_id", option_text(py, &agent.support_request_id));
            set!(
                "support_request_status",
                match agent.support_request_status {
                    Some(status) => PyString::new(py, status_text(status)).into_any(),
                    None => py.None().into_bound(py),
                }
            );
            set!("support_assigned_staff_id", option_text(py, &agent.support_assigned_staff_id));
            set!("support_wait_minutes", agent.support_wait_minutes.into_bound_py_any(py)?);
            set!("support_cooldown_remaining_minutes", agent.support_cooldown_remaining_minutes.into_bound_py_any(py)?);
            set!("information_query_pressure", float(agent.information_query_pressure));
            set!("misinformation_belief_pressure", float(agent.misinformation_belief_pressure));
            let sources = if agent.misinformation_effect_sources.is_empty() {
                PyTuple::empty(py).into_any()
            } else {
                let mut items = Vec::with_capacity(agent.misinformation_effect_sources.len());
                for source in &agent.misinformation_effect_sources {
                    items.push((&*source.message_id, &*source.origin_event_id, &*source.last_event_id, &*source.source_id, source.time));
                }
                PyTuple::new(py, items)?.into_any()
            };
            set!("misinformation_effect_sources", sources);
            let body = &agent.body;
            set!("sleep_debt_minutes", float(body.sleep_debt_minutes));
            set!("uncertainty", float(body.uncertainty));
            set!("reassurance", float(body.reassurance));
            set!("discontent", float(body.discontent));
            set!("positive_affect", float(body.positive_affect));
            set!("negative_affect", float(body.negative_affect));
            set!("hygiene_need", float(body.hygiene_need));
            let Ok(fields) = fields.downcast::<PyDict>() else {
                return unsupported("a copy of a dict is not a dict");
            };
            let view = new_object(helpers, &helpers.agent_view, fields)?;
            raw_dict_set(agents.as_any(), key.as_any(), &view)?;
        }
        let facilities = PyDict::new(py);
        for facility in &data.facilities {
            let fields = PyDict::new(py);
            let any = fields.as_any();
            macro_rules! set {
                ($name:literal, $value:expr) => {
                    raw_dict_set(any, intern!(py, $name).as_any(), &$value)?
                };
            }
            let id = PyString::new(py, &facility.id);
            set!("id", id.as_any());
            set!("kind", PyString::new(py, &facility.kind).into_any());
            set!("zone_id", PyString::new(py, &facility.zone_id).into_any());
            set!("capacity", facility.capacity.into_bound_py_any(py)?);
            set!("working_units", facility.working_units.into_bound_py_any(py)?);
            set!("active_agent_ids", tuple_of(py, &facility.active_agent_ids)?);
            set!("queue_agent_ids", tuple_of(py, &facility.queue_agent_ids)?);
            set!("resource_active_agent_ids", tuple_of(py, &facility.resource_active_agent_ids)?);
            set!("resource_queue_agent_ids", tuple_of(py, &facility.resource_queue_agent_ids)?);
            set!("facility_event_context_ids", tuple_of(py, &facility.facility_event_context_ids)?);
            let view = new_object(helpers, &helpers.facility_view, &fields)?;
            raw_dict_set(facilities.as_any(), id.as_any(), &view)?;
        }
        let zone_members = members_python(py, data.zone_members(), &self.keys)?;
        let group_members = members_python(py, data.group_members(), &self.keys)?;
        Ok((agents, facilities, zone_members, group_members))
    }
}

/// A `WorldView` over a snapshot, whose Python parts are made when a Python reader asks.
fn new_view<'py>(py: Python<'py>, data: impl Into<Arc<ViewData>>, keys: Vec<Py<PyString>>, hash: Option<String>) -> PyResult<Bound<'py, PyAny>> {
    let data: Arc<ViewData> = data.into();
    let helpers = helpers(py)?;
    let fields = PyDict::new(py);
    fields.set_item(intern!(py, "minute_index"), data.minute_index)?;
    fields.set_item(intern!(py, "state_hash"), hash)?;
    fields.set_item(intern!(py, "resource_service_enabled"), data.resource_service_enabled)?;
    fields.set_item(intern!(py, "autonomy_enabled"), data.autonomy_enabled)?;
    fields.set_item(intern!(py, "_has_population_context"), data.has_population_context)?;
    fields.set_item(intern!(py, "_rust_view"), Py::new(py, RustView { data, keys })?)?;
    new_object(helpers, &helpers.world_view, &fields)
}

/// The `WorldView` of the update phase's snapshot the Run's scratch keeps (the getter of
/// `_view_before_proposals`, decision D4 of design-p9.md), with the Run's keys of its people.
pub(crate) fn view_of_data<'py>(py: Python<'py>, run: &RunCore, data: Arc<ViewData>) -> PyResult<Bound<'py, PyAny>> {
    let keys = run.keys(py, data.agents.iter().map(|agent| &agent.id))?;
    new_view(py, data, keys, None)
}

// ---- A view Python made ---------------------------------------------------------------------------

fn exact_float(value: &Bound<'_, PyAny>) -> PyResult<f64> {
    match value.downcast_exact::<PyFloat>() {
        Ok(number) => Ok(number.value()),
        Err(_) => unsupported("a view value is not a float"),
    }
}

fn exact_text(value: &Bound<'_, PyAny>) -> PyResult<Arc<str>> {
    match value.downcast_exact::<PyString>() {
        Ok(text) => Ok(Arc::from(text.to_str().map_err(|_| Unsupported::new_err("not UTF-8"))?)),
        Err(_) => unsupported("a view value is not a str"),
    }
}

fn optional_text(value: &Bound<'_, PyAny>) -> PyResult<Option<Arc<str>>> {
    if value.is_none() {
        Ok(None)
    } else {
        exact_text(value).map(Some)
    }
}

fn optional_i64(value: &Bound<'_, PyAny>) -> PyResult<Option<i64>> {
    if value.is_none() {
        return Ok(None);
    }
    match value.downcast_exact::<PyInt>() {
        Ok(number) => number.extract().map(Some).map_err(|_| Unsupported::new_err("an int is too large")),
        Err(_) => unsupported("a view value is not an int"),
    }
}

/// `record[name]`; a missing one raises in Python.
fn field<'py>(record: &Bound<'py, PyDict>, name: &Bound<'py, PyString>) -> PyResult<Bound<'py, PyAny>> {
    record.get_item(name)?.ok_or_else(|| Unsupported::new_err("a record's field is missing"))
}

/// What the kernels read of a view Python made (a Rust view was refused, or a test made it). Its
/// `__dict__` is read with the checks of a typed field. Only what the update kernels read is filled:
/// `ViewData::partial` says so. Nothing refuses it for a rule: one that reads more must check it.
fn from_python_view<'py>(view: &Bound<'py, PyAny>) -> PyResult<Bound<'py, RustView>> {
    let py = view.py();
    let items = view.getattr(intern!(py, "agents"))?.call_method0(intern!(py, "items"))?;
    let mut agents = Vec::with_capacity(items.len()?);
    let mut zones = Vec::with_capacity(agents.capacity());
    let mut keys: Vec<Py<PyString>> = Vec::with_capacity(agents.capacity());
    for item in items.try_iter()? {
        let item = item?;
        let item = item.downcast_exact::<PyTuple>()?;
        let key = item.get_item(0)?;
        let Ok(key) = key.downcast_into_exact::<PyString>() else {
            return unsupported("an id is not a str");
        };
        // A lone surrogate has no UTF-8.
        let id: Arc<str> = Arc::from(key.to_str().map_err(|_| Unsupported::new_err("an id is not UTF-8"))?);
        if keys.last().is_some_and(|last| last.bind(py).to_str().is_ok_and(|last| last >= &*id)) {
            return unsupported("the view is not in ID order");
        }
        let fields = item.get_item(1)?.getattr(intern!(py, "__dict__"))?;
        let Some(fields) = as_readable_dict(&fields) else {
            return unsupported("a person's fields are not a dict");
        };
        let float = |name: &Bound<'py, PyString>| -> PyResult<f64> { exact_float(&field(fields, name)?) };
        zones.push(Arc::new(ViewZone {
            id: exact_text(&field(fields, intern!(py, "zone_id"))?)?,
            environmental_stress_per_minute: float(intern!(py, "environmental_stress_per_minute"))?,
            environmental_stressor_event_ids: Vec::new(),
            environmental_assumption_source_id: None,
            ambient_noise_level: 0.0,
            rest_efficiency_multiplier: float(intern!(py, "rest_efficiency_multiplier"))?,
            zone_environment_modifier_event_ids: Vec::new(),
            zone_environment_assumption_ids: Vec::new(),
            ventilation_effectiveness: 0.0,
            lighting_level: 0.0,
            privacy_level: 0.0,
            temperature_c: 0.0,
            ventilation_hygiene_growth_per_minute: float(intern!(py, "ventilation_hygiene_growth_per_minute"))?,
            privacy_discontent_per_minute: float(intern!(py, "privacy_discontent_per_minute"))?,
            condition_modifier_event_ids: Vec::new(),
            condition_assumption_ids: Vec::new(),
            demand_modifiers: Vec::new(),
            min_rest_decision_minutes: 0,
            max_rest_decision_minutes: 0,
            sanitation_level: float(intern!(py, "sanitation_level"))?,
        }));
        agents.push(ViewAgent {
            zone: agents.len(),
            group_id: optional_text(&field(fields, intern!(py, "group_id"))?)?,
            mobility: Mobility::Independent,
            support_need: 0.0,
            stress_sensitivity: 0.0,
            rng_key: id.clone(),
            activity: Activity::Idle,
            target_id: None,
            needs: NeedRow { toilet_urgency: 0.0, hunger: 0.0, fatigue: 0.0, hydration_need: 0.0 },
            stress: 0.0,
            chronic_distress: 0.0,
            trust: 0.0,
            next_decision_at: 0,
            queue_task_id: None,
            queue_kind: None,
            queue_target_id: None,
            queue_wait_minutes: 0,
            support_request_id: None,
            support_request_status: None,
            support_assigned_staff_id: None,
            support_wait_minutes: 0,
            support_cooldown_remaining_minutes: 0,
            information_query_pressure: float(intern!(py, "information_query_pressure"))?,
            misinformation_belief_pressure: float(intern!(py, "misinformation_belief_pressure"))?,
            misinformation_effect_sources: Vec::new(),
            body: BodyFields {
                sleep_debt_minutes: float(intern!(py, "sleep_debt_minutes"))?,
                uncertainty: float(intern!(py, "uncertainty"))?,
                reassurance: float(intern!(py, "reassurance"))?,
                discontent: float(intern!(py, "discontent"))?,
                positive_affect: float(intern!(py, "positive_affect"))?,
                negative_affect: float(intern!(py, "negative_affect"))?,
                hygiene_need: float(intern!(py, "hygiene_need"))?,
            },
            person: Arc::new(PersonFields {
                required_sleep_minutes: optional_i64(&field(fields, intern!(py, "required_sleep_minutes"))?)?,
                ..PersonFields::default()
            }),
            id,
        });
        keys.push(key.unbind());
    }
    let data = ViewData {
        minute_index: 0,
        resource_service_enabled: false,
        autonomy_enabled: false,
        has_population_context: false,
        partial: true,
        zones,
        agents,
        facilities: Vec::new(),
    };
    Bound::new(py, RustView { data: Arc::new(data), keys })
}

/// The snapshot of a view, for the update kernels. They read the same tick's rows as well.
pub(crate) fn view_of<'py>(view: &Bound<'py, PyAny>) -> PyResult<Bound<'py, RustView>> {
    let py = view.py();
    let fields = view.getattr(intern!(py, "__dict__"))?;
    match raw_dict_get(&fields, intern!(py, "_rust_view").as_any())? {
        Some(found) => Ok(found.downcast_into::<RustView>()?),
        None => from_python_view(view),
    }
}

pub fn register(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_class::<RustView>()?;
    m.add_function(wrap_pyfunction!(view_after_movement, m)?)?;
    m.add_function(wrap_pyfunction!(view_for_proposals, m)?)?;
    m.add_function(wrap_pyfunction!(misinformation_applies_of, m)?)?;
    Ok(())
}
