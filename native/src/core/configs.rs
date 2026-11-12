//! The configs as the kernels read them (design-p7.md §3, Issue #152, lane 7-1; policies E7 and
//! E14 of docs/rust-port/step5-plan.md).
//!
//! The core's `Configs` (`sheltersim_core::config`) is kept in the world's `World.derived` by the
//! stamp of the `model_configs` slot. This file fills its sections with today's Python contracts,
//! called on the same tree, at the first read of a section after the stamp changed. A parse never
//! raises here: what it raised is kept as a `Fault`, and the read site turns it into its own error
//! at the line it raises it today. A refusal keeps its reason there; an error the rule relays as it
//! is comes from a second parse (`reraise`), so its class and text are Python's own. The sections
//! of the geometry (`facility_geometry`, `spatial_constraints`) are filled by
//! `rules/movement_geometry.rs`, beside the readers of their parsed models.
//!
//! The stamp changes with every write to the configs, so an edit of `model_configs` in place is
//! seen at the next read. Inside a tick phase the Python scopes (`selected_models_scope`,
//! `geometry_config_scope`) saw it only at the next minute. Nothing in `src/` makes such an edit
//! (decision U1 of design-p7.md); a debug run counts any (`watch_config_writes`), and the all-path
//! gate reports a count above zero as a problem.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;

use pyo3::exceptions::{PyKeyError, PyTypeError, PyValueError};
use pyo3::intern;
use pyo3::prelude::*;
use pyo3::sync::GILOnceCell;
use pyo3::types::{PyDict, PyString};
use sheltersim_core::config::read::Read;
use sheltersim_core::config::body::{BodyModels, CareOutcome};
use sheltersim_core::config::environment::{Infrastructure, ZoneEnvironmentSpec};
use sheltersim_core::config::information::InformationSpread;
use sheltersim_core::config::overlay::OverlayRooms;
use sheltersim_core::config::proximity::Proximity;
use sheltersim_core::config::staff::StaffOperations;
use sheltersim_core::config::{parse_configs, Configs, Fault, FaultClass, Lazy, Section, SectionId, Selection};

use super::errors::{corrupt, OrPy};
use super::row::attribute;
use super::value::{VPy, V};
use super::world_core::field_stamp;
use crate::Unsupported;

const MODEL_CONFIGS: &str = "model_configs";

/// The configs of `world.model_configs` now: those kept for its stamp, or new ones with nothing
/// parsed. Without a stamp (no core, or a field assigned whole), new ones that serve one read.
pub(crate) fn current(world: &Bound<'_, PyAny>) -> PyResult<Arc<Configs>> {
    let raw = attribute(world, intern!(world.py(), "model_configs"))?;
    current_of(world, &raw)
}

/// `current`, for a caller that has read `world.model_configs` already.
pub(crate) fn current_of(world: &Bound<'_, PyAny>, raw: &Bound<'_, PyAny>) -> PyResult<Arc<Configs>> {
    match field_stamp(world, MODEL_CONFIGS, raw)? {
        Some((core, stamp)) => core.try_borrow()?.world().configs(stamp).or_py(),
        None => Ok(Arc::new(Configs::new(None))),
    }
}

/// What a parse gave, as a section: a value, or what it raised.
pub(crate) fn section_of<T: ?Sized>(py: Python<'_>, parsed: PyResult<Arc<T>>) -> Section<T> {
    match parsed {
        Ok(value) => Section::Ok(value),
        Err(error) => Section::Fault(fault_of(py, &error)),
    }
}

/// The class and text of a Python error.
pub(crate) fn fault_of(py: Python<'_>, error: &PyErr) -> Fault {
    let kind = error.get_type(py);
    let class = if kind.is(&py.get_type::<PyValueError>()) {
        FaultClass::Value
    } else if kind.is(&py.get_type::<PyTypeError>()) {
        FaultClass::Type
    } else if kind.is(&py.get_type::<PyKeyError>()) {
        FaultClass::Key
    } else {
        let module = kind.module().map(|module| module.to_string()).unwrap_or_default();
        let name = kind.name().map(|name| name.to_string()).unwrap_or_default();
        if name == "ValidationError" && module.starts_with("pydantic") {
            FaultClass::Validation
        } else {
            FaultClass::Other(format!("{module}.{name}").into())
        }
    };
    Fault::new(class, error.value(py).to_string())
}

/// The error of a fault, for a read site that relays it as it is: what a second parse raises,
/// which is the same class and text, since a parse is a pure function of the configs.
pub(crate) fn reraise<T>(parse: impl FnOnce() -> PyResult<T>) -> PyErr {
    match parse() {
        Err(error) => error,
        Ok(_) => corrupt(MODEL_CONFIGS, "a config fault did not reproduce"),
    }
}

// ---- The sections of the kernels (lane 7-5b) --------------------------------------------------------

/// A section kept for the configs' stamp, parsed by `parse` at its first read after the stamp
/// changed: its value, or the fault for the read site to raise (`raise`).
pub(crate) fn kept<T>(
    world: &Bound<'_, PyAny>,
    section: fn(&Configs) -> &Lazy<T>,
    parse: impl FnOnce() -> PyResult<Section<T>>,
) -> PyResult<Result<Arc<T>, Fault>> {
    let configs = current(world)?;
    let lazy = section(&configs);
    if lazy.get().is_none() {
        sheltersim_core::config::count_parse();
        // A parse that fails to run at all (no configs, a borrow) raises here, as it did.
        let parsed = parse()?;
        let _ = lazy.get_or_parse(|| parsed);
    }
    Ok(match lazy.get() {
        Some(Section::Ok(value)) => Ok(value.clone()),
        Some(Section::Fault(fault)) => Err(fault.clone()),
        Some(Section::Absent) | None => Err(Fault::refused("the configs have no such section")),
    })
}

/// How many sections `kept` (or the core, lane P9-0) has parsed in this process, for the tests: a
/// read that finds its section kept for the stamp adds nothing.
#[pyfunction]
pub(crate) fn config_parses() -> u64 {
    sheltersim_core::config::parses()
}

/// `kept` for a section parsed from the model selection too (`selection`): while a test has patched
/// `selected_models`, whose selection is not kept, the section is parsed at every read as well.
pub(crate) fn kept_with_selection<T>(
    world: &Bound<'_, PyAny>,
    section: fn(&Configs) -> &Lazy<T>,
    parse: impl FnOnce() -> PyResult<Section<T>>,
) -> PyResult<Result<Arc<T>, Fault>> {
    let py = world.py();
    let function = policies(py)?.module.bind(py).getattr(intern!(py, "selected_models"))?;
    if is_the_contract(&function)? {
        return kept(world, section, parse);
    }
    Ok(match parse()? {
        Section::Ok(value) => Ok(value),
        Section::Fault(fault) => Err(fault),
        Section::Absent => Err(Fault::refused("the configs have no such section")),
    })
}

/// What a parse that calls Python gave: a refusal (`Unsupported`) keeps its reason, any other
/// error its class and text.
pub(crate) fn parsed<T>(py: Python<'_>, parsed: PyResult<T>) -> Section<T> {
    match parsed {
        Ok(value) => Section::Ok(Arc::new(value)),
        Err(error) => Section::Fault(refusal_or_fault(py, &error)),
    }
}

fn refusal_or_fault(py: Python<'_>, error: &PyErr) -> Fault {
    if error.get_type(py).is(&py.get_type::<Unsupported>()) {
        Fault::refused(error.value(py).to_string())
    } else {
        fault_of(py, error)
    }
}

/// `parse` of the configs as a `V`: the row of the `model_configs` slot while it holds the field
/// (the core's `parse_configs`), or else the tree read now. A tree that is not JSON is refused for what it holds.
pub(crate) fn parsed_value<T>(world: &Bound<'_, PyAny>, parse: impl Fn(&V) -> Read<T>) -> PyResult<Section<T>> {
    let py = world.py();
    let raw = attribute(world, intern!(py, "model_configs"))?;
    if let Some((core, _)) = field_stamp(world, MODEL_CONFIGS, &raw)? {
        // The core's function on the world, borrowed shared: it calls no Python.
        if let Some(section) = parse_configs(core.try_borrow()?.world(), &parse).or_py()? {
            return Ok(section);
        }
    }
    Ok(match <V as VPy>::from_py(&raw) {
        Ok(configs) => parse(&configs).into(),
        Err(error) => Section::Fault(refusal_or_fault(py, &error)),
    })
}

/// A section the kernels parse from the configs as a `V` (`parsed_value`), kept for the stamp; its
/// refusal is said at the read site.
#[track_caller]
pub(crate) fn kept_value<T>(
    world: &Bound<'_, PyAny>,
    section: fn(&Configs) -> &Lazy<T>,
    parse: fn(&V) -> Read<T>,
) -> PyResult<Arc<T>> {
    match kept(world, section, || parsed_value(world, parse))? {
        Ok(value) => Ok(value),
        Err(fault) => read(Err(fault)),
    }
}

/// `kept_value` without its raise: the value, or the fault for a kernel of the core to raise where
/// its rule reads the section (`sheltersim_core::rules::kit::Kept`).
pub(crate) fn kept_section<T>(
    world: &Bound<'_, PyAny>,
    section: fn(&Configs) -> &Lazy<T>,
    parse: fn(&V) -> Read<T>,
) -> PyResult<Result<Arc<T>, Fault>> {
    kept(world, section, || parsed_value(world, parse))
}

/// A section that a Python expression of the configs gives (`parse`, on the world), evaluated at
/// the first read after the stamp changed and kept: its value, or what it raised, raised here again
/// by a second evaluation, with Python's own class and text (policy E14).
#[track_caller]
pub(crate) fn kept_python<T>(
    world: &Bound<'_, PyAny>,
    section: fn(&Configs) -> &Lazy<T>,
    parse: impl Fn(&Bound<'_, PyAny>) -> PyResult<T>,
) -> PyResult<Arc<T>> {
    let py = world.py();
    match kept(world, section, || Ok(parsed(py, parse(world))))? {
        Ok(value) => Ok(value),
        Err(fault) => Err(raise(&fault, || parse(world))),
    }
}

/// The error a read site raises for a fault: a refusal its own `Unsupported`, any other what a
/// second parse raises (policy E14).
#[track_caller]
pub(crate) fn raise<R>(fault: &Fault, reparse: impl FnOnce() -> PyResult<R>) -> PyErr {
    if fault.is_refusal() {
        Unsupported::new_err(fault.text.to_string())
    } else {
        reraise(reparse)
    }
}

/// A value of a section as a read site takes it: a refusal is its `Unsupported`, said here.
#[track_caller]
pub(crate) fn read<T>(value: Read<T>) -> PyResult<T> {
    match value {
        Ok(value) => Ok(value),
        Err(fault) if fault.is_refusal() => Err(Unsupported::new_err(fault.text.to_string())),
        Err(_) => Err(corrupt(MODEL_CONFIGS, "a value of a section holds an error")),
    }
}

/// A section as its read site reads it, for the tests (`tests/integration/test_config_sections.py`):
/// the same accessor, so the value, the error and the parses are the read site's. `key` is a staff
/// member for `staff_fatigue`.
#[pyfunction]
#[pyo3(signature = (world, name, key=None))]
pub(crate) fn config_section<'py>(
    world: &Bound<'py, PyAny>,
    name: &str,
    key: Option<&str>,
) -> PyResult<Bound<'py, PyAny>> {
    use pyo3::IntoPyObjectExt;
    use sheltersim_core::config::{agent_dynamics, body, staff};

    use crate::rules::{agent_dynamics as agents, information_events, social_interaction, staff_dynamics};
    let py = world.py();
    match name {
        "staff_fatigue" => {
            let formula = staff_dynamics::fatigue_formula(world)?;
            (formula.saturating, read(formula.limit(key.unwrap_or_default()))?).into_bound_py_any(py)
        }
        "staff_operations" => {
            let formula = staff_operations(world)?;
            let (source, policy, event_source) = formula.provenance();
            (formula.travel_first(), source, policy, event_source).into_bound_py_any(py)
        }
        "staff_operations_known" => {
            kept_value(world, |configs| &configs.staff_operations_known, staff::known_staff_operations)?;
            Ok(py.None().into_bound(py))
        }
        "support_task_minutes" => {
            kept_value(world, |configs| &configs.support_task_minutes, staff::support_task_minutes)?.into_bound_py_any(py)
        }
        "support_cooldown_minutes" => support_cooldown_minutes(world)?.into_bound_py_any(py),
        "information_spread" => {
            let config = information_events::checked_config(world)?;
            read(config.query_pressure_per_claim.clone())?.into_bound_py_any(py)
        }
        "information_pressure_gain" => social_interaction::pressure_gain(world)?.into_bound_py_any(py),
        "body_models" => body_model_names(world)?.into_bound_py_any(py),
        "social_interaction_minutes" => {
            kept_value(world, |configs| &configs.social_interaction_minutes, body::social_interaction_minutes)?
                .into_bound_py_any(py)
        }
        "care_outcome" => match selected_care_formula(world)? {
            Some(formula) => (&*formula.model_id, &*formula.source_id).into_bound_py_any(py),
            None => Ok(py.None().into_bound(py)),
        },
        "agent_dynamics_policy" => crate::state::agent_dynamics::policy_of(world)?.into_bound_py_any(py),
        "facility_awareness" => match agents::awareness_of(world)?.as_ref() {
            Some(awareness) => {
                let initial: Vec<&str> = awareness.initial.iter().map(|id| &**id).collect();
                (initial, awareness.discover).into_bound_py_any(py)
            }
            None => Ok(py.None().into_bound(py)),
        },
        "knowledge_is_limited" => {
            kept_value(world, |configs| &configs.knowledge_is_limited, agent_dynamics::knowledge_is_limited)?
                .into_bound_py_any(py)
        }
        "zone_environment" => {
            let spec = zone_environment_spec(world)?;
            (&*spec.policy_version, &*spec.source_id).into_bound_py_any(py)
        }
        "infrastructure" => (&*infrastructure_config(world)?.source_id).into_bound_py_any(py),
        "proximity" => match proximity_config(world)?.as_ref() {
            Some(config) => (&*config.policy_version, read(config.max_distance_m.clone())?).into_bound_py_any(py),
            None => Ok(py.None().into_bound(py)),
        },
        // ---- Lane 7-5a: `key` names a person or a policy where a section has several.
        "movement" => {
            let movement = crate::rules::movement::movement_now(world)?;
            let model = read(movement.model.clone())?;
            let speeds = (model.independent, model.assisted, model.wheelchair, model.density_slowdown, model.maximum_slowdown);
            (movement.has_model, movement.has_geometry, speeds).into_bound_py_any(py)
        }
        "route_capacities" => {
            match &*kept_value(world, |configs| &configs.route_capacities, sheltersim_core::config::movement::RouteCapacities::parse)? {
                Some(routing) => {
                    let mut capacities = Vec::with_capacity(routing.capacities.len());
                    for (edge, capacity) in &routing.capacities {
                        capacities.push((&**edge, read(capacity.clone())?));
                    }
                    capacities.into_bound_py_any(py)
                }
                None => Ok(py.None().into_bound(py)),
            }
        }
        "transit_capacities" => transit_capacities_of(world)?.into_bound_py_any(py),
        "facility_queues" => crate::rules::queues::policy_text(world)?.into_bound_py_any(py),
        "overlay_schema" => crate::rules::overlay::overlay_schema(world)?.into_bound_py_any(py),
        "overlay_rooms" => overlay_rooms_of(world)?.into_bound_py_any(py),
        "resource_service" => {
            let service = crate::rules::service::resource_service_of(world)?;
            let skills = read(service.skills.clone())?;
            let keys: Option<Vec<&str>> = service.config.as_ref().map(|config| config.keys().map(|key| &**key).collect());
            (service.present, keys, skills.water.as_deref(), skills.meal.as_deref()).into_bound_py_any(py)
        }
        "accessibility_needs" => {
            let needs = kept_value(world, |configs| &configs.accessibility_needs, sheltersim_core::config::population::AccessibilityNeeds::parse)?;
            let needs: Vec<&str> = read(needs.of(key.unwrap_or_default()))?.iter().map(|need| &**need).collect();
            needs.into_bound_py_any(py)
        }
        "extensions_enabled" => extensions_enabled_of(world)?.into_bound_py_any(py),
        _ => Err(pyo3::exceptions::PyKeyError::new_err(name.to_owned())),
    }
}

// ---- The model selection (design-p7.md §1.1, rows 4–4e) --------------------------------------------

struct Policies {
    module: Py<PyModule>,
    /// `_SELECTION_SCOPE`: set while a tick phase runs (`OfflineSimulationController.__call__`).
    scope: Py<PyAny>,
}

static POLICIES: GILOnceCell<Policies> = GILOnceCell::new();
/// `selected_models` of the module, as first seen. A read that finds another function there (a
/// test's patch) calls that one each time and keeps nothing.
static SELECTED_MODELS: GILOnceCell<Py<PyAny>> = GILOnceCell::new();

fn policies(py: Python<'_>) -> PyResult<&Policies> {
    POLICIES.get_or_try_init(py, || {
        let module = py.import("sheltersim.contracts.policies")?;
        Ok(Policies { scope: module.getattr("_SELECTION_SCOPE")?.unbind(), module: module.unbind() })
    })
}

/// Whether `function` is the module's own `selected_models`, whose selection may be kept. The
/// first one seen is taken by its names, so a patch in place at the first read is not.
/// Whether `selected_models` is the contract's own, whose selection is kept for the configs' stamp;
/// a test's patch is read at every read (lane P9-1a: the host's `fill_section` of the selection).
pub(crate) fn selection_is_kept(py: Python<'_>) -> PyResult<bool> {
    let function = policies(py)?.module.bind(py).getattr(intern!(py, "selected_models"))?;
    is_the_contract(&function)
}

fn is_the_contract(function: &Bound<'_, PyAny>) -> PyResult<bool> {
    let py = function.py();
    if let Some(known) = SELECTED_MODELS.get(py) {
        return Ok(function.is(known));
    }
    let named = |name: &Bound<'_, PyString>, wanted: &str| -> PyResult<bool> {
        Ok(function.getattr(name).ok().and_then(|value| value.extract::<String>().ok()).as_deref() == Some(wanted))
    };
    let own = named(intern!(py, "__module__"), "sheltersim.contracts.policies")?
        && named(intern!(py, "__qualname__"), "selected_models")?;
    if own {
        let _ = SELECTED_MODELS.set(py, function.clone().unbind());
    }
    Ok(own)
}

/// `selected_models(world.model_configs)`, kept for the configs' stamp: the selection, or the fault
/// for the read site to replay. A fault whose class is `Refused` is a selection that is not a dict
/// of str to str; any other is what `selected_models` raised (`reraise_selection`).
pub(crate) fn selection(world: &Bound<'_, PyAny>) -> PyResult<Result<Arc<Selection>, Fault>> {
    let py = world.py();
    let raw = attribute(world, intern!(py, "model_configs"))?;
    let policies = policies(py)?;
    let function = policies.module.bind(py).getattr(intern!(py, "selected_models"))?;
    let configs = if is_the_contract(&function)? {
        current_of(world, &raw)?
    } else {
        Arc::new(Configs::new(None))
    };
    let section = configs.selection.get_or_parse(|| match function.call1((&raw,)) {
        Ok(selection) => match read_selection(&selection) {
            Some(selection) => Section::Ok(Arc::new(selection)),
            None => Section::Fault(Fault::refused("not a selection")),
        },
        Err(error) => Section::Fault(fault_of(py, &error)),
    });
    Ok(match section {
        Section::Ok(selection) => Ok(selection.clone()),
        Section::Fault(fault) => Err(fault.clone()),
        Section::Absent => Err(Fault::refused("not a selection")),
    })
}

/// What `selected_models(world.model_configs)` raises now, for a read site that relays it.
pub(crate) fn reraise_selection(world: &Bound<'_, PyAny>) -> PyErr {
    let py = world.py();
    reraise(|| {
        let raw = attribute(world, intern!(py, "model_configs"))?;
        policies(py)?.module.bind(py).getattr(intern!(py, "selected_models"))?.call1((raw,))
    })
}

/// A dict of exact str to exact str, in its order, or None.
fn read_selection(selection: &Bound<'_, PyAny>) -> Option<Selection> {
    let selection = selection.downcast_exact::<PyDict>().ok()?;
    let mut models = Vec::with_capacity(selection.len());
    for (domain, model) in selection.iter() {
        let text = |value: &Bound<'_, PyAny>| -> Option<Arc<str>> {
            Some(Arc::from(value.downcast_exact::<PyString>().ok()?.to_str().ok()?))
        };
        models.push((text(&domain)?, text(&model)?));
    }
    Some(Selection::new(models))
}

// ---- The debug count of configs written inside a tick (decision U1) ---------------------------------

static WATCHING: AtomicBool = AtomicBool::new(false);
static WRITES_IN_TICK: AtomicU64 = AtomicU64::new(0);

/// Count, from now on, the writes to a `model_configs` tree in place while a tick phase runs, and
/// start the count again from zero (off: stop counting).
#[pyfunction]
pub(crate) fn watch_config_writes(on: bool) {
    WRITES_IN_TICK.store(0, Ordering::Relaxed);
    crate::rules::movement_geometry::reset_stale_geometry_hits();
    WATCHING.store(on, Ordering::Relaxed);
}

/// Whether a debug run counts (`watch_config_writes`): the geometry plans then also check that a
/// stamp hit was built from the overlay held now (`rules/movement_geometry.rs`).
#[inline]
pub(crate) fn watching() -> bool {
    WATCHING.load(Ordering::Relaxed)
}

/// The writes counted since `watch_config_writes(True)`.
#[pyfunction]
pub(crate) fn config_writes_in_tick() -> u64 {
    WRITES_IN_TICK.load(Ordering::Relaxed)
}

/// A write from Python reached the tree of `name` (`NamespaceMirror::apply`). It holds no borrow.
#[inline]
pub(crate) fn note_write(py: Python<'_>, name: &str) -> PyResult<()> {
    if !WATCHING.load(Ordering::Relaxed) || name != MODEL_CONFIGS {
        return Ok(());
    }
    note_write_in_tick(py)
}

#[cold]
fn note_write_in_tick(py: Python<'_>) -> PyResult<()> {
    if !policies(py)?.scope.bind(py).call_method0(intern!(py, "get"))?.is_none() {
        WRITES_IN_TICK.fetch_add(1, Ordering::Relaxed);
    }
    Ok(())
}

// ---- The sections Python's contracts parse (lane P10-1c) --------------------------------------------
//
// The parses `PyHost::fill_section` and `read_again` run for the sections whose parser is Python's
// (`config::Parser::Host`), at the read site's first read after the stamp changed, and the readers of
// the sections' tests (`config_section`). They were in the rules' glue files (`native/src/rules/`)
// until lane P10-1c; their texts, their order of reads and the parses they count are the same.

/// `value.get(key)`.
fn get<'py>(value: &Bound<'py, PyAny>, key: &str) -> PyResult<Bound<'py, PyAny>> {
    value.call_method1(intern!(value.py(), "get"), (key,))
}

/// `value.get(key, {})`, for a read of a chain of mappings that may be absent.
fn get_map<'py>(value: &Bound<'py, PyAny>, key: &str) -> PyResult<Bound<'py, PyAny>> {
    value.call_method1(intern!(value.py(), "get"), (key, PyDict::new(value.py())))
}

/// An exact str, or Unsupported.
fn exact_text(value: &Bound<'_, PyAny>) -> PyResult<Arc<str>> {
    Ok(Arc::from(super::row::exact_str(value)?))
}

/// The native host of a reader of the sections' tests, with the holding read now.
fn host_now<'py>(world: &Bound<'py, PyAny>) -> PyResult<(crate::host::PyHost<'py>, Arc<sheltersim_core::host::Holding>)> {
    let mut host = crate::host::PyHost::of_entry(world);
    let holding = host.holding_now()?;
    Ok((host, holding))
}

// The body's models (`body_models`).

/// `selected_models(model_configs)`, kept for the configs' stamp. What it raises is the rule's to say.
pub(crate) fn body_selection(world: &Bound<'_, PyAny>) -> PyResult<Arc<Selection>> {
    match selection(world)? {
        Ok(selection) => Ok(selection),
        Err(fault) if fault.is_refusal() => Err(Unsupported::new_err("not a selection")),
        Err(_) => Err(Unsupported::new_err("the model selection raises")),
    }
}

/// `create_affect` without a selection: the config's, by the function that also refuses a bad one.
fn affect_of_the_config(world: &Bound<'_, PyAny>) -> PyResult<Arc<str>> {
    let py = world.py();
    let configs = attribute(world, intern!(py, "model_configs"))?;
    let raw = super::row::exact_dict(&configs)?.get_item(intern!(py, "body_dynamics"))?;
    let config = py
        .import(intern!(py, "sheltersim.domain.body_state"))?
        .getattr(intern!(py, "canonical_body_config"))?
        .call1((raw,))
        .map_err(|_| Unsupported::new_err("the body config raises"))?;
    exact_text(&config.get_item(intern!(py, "affect_policy"))?)
}

fn affect_of(world: &Bound<'_, PyAny>, selection: &Selection) -> PyResult<Arc<str>> {
    match selection.get("affect") {
        Some(affect) => Ok(affect.clone()),
        None => affect_of_the_config(world),
    }
}

/// What the read of the body's models raises again, for a fault of the section (the host's
/// `read_again`, lane P9-1c): the affect model's read of the configs.
pub(crate) fn raise_body_models(world: &Bound<'_, PyAny>) -> PyResult<()> {
    let selection = body_selection(world)?;
    affect_of(world, &selection).map(drop)
}

/// The section `body_models`: the operators the rule creates, or its fault (the host's
/// `fill_section`, lane P9-0). An ID the core doesn't know is left to Python's factory.
pub(crate) fn body_models_section(world: &Bound<'_, PyAny>, selection: &Selection) -> PyResult<Result<Arc<BodyModels>, Fault>> {
    let py = world.py();
    let relationship = selection.get("relationship").cloned();
    let parse = || match affect_of(world, selection) {
        Ok(affect) => parsed_value(world, |configs| BodyModels::parse(&affect, relationship.as_deref(), configs)),
        Err(error) => Ok(parsed(py, Err(error))),
    };
    kept_with_selection(world, |configs| &configs.body_models, parse)
}

/// The affect and relationship models the rule creates, by name, for the tests (`config_section`).
fn body_model_names(world: &Bound<'_, PyAny>) -> PyResult<(&'static str, &'static str)> {
    use sheltersim_core::config::body::{AffectModel, TrustModel, DISTANCE_TRUST, DURATION_TRUST, LINEAR_AFFECT, TRUST_WEIGHTED_AFFECT};
    let selection = body_selection(world)?;
    let models = match body_models_section(world, &selection)? {
        Ok(models) => models,
        Err(fault) => return Err(raise(&fault, || affect_of(world, &selection))),
    };
    let affect = match models.affect {
        AffectModel::Linear => LINEAR_AFFECT,
        AffectModel::TrustWeighted => TRUST_WEIGHTED_AFFECT,
    };
    let trust = match models.trust {
        TrustModel::DistanceDurationRepeat { .. } => DISTANCE_TRUST,
        TrustModel::DurationOnly => DURATION_TRUST,
    };
    Ok((affect, trust))
}

// The zone environment (`zone_environment`).

/// `ScenarioZoneEnvironmentConfig.model_validate(model_configs["zone_environment"])`, kept for the
/// configs' stamp; what it raises is relayed as it is.
pub(crate) fn zone_environment_spec(world: &Bound<'_, PyAny>) -> PyResult<Arc<ZoneEnvironmentSpec>> {
    match zone_environment_section(world)? {
        Ok(spec) => Ok(spec),
        Err(fault) => Err(raise(&fault, || parse_zone_environment(world))),
    }
}

/// `zone_environment_spec` without its raise (the host's `fill_section`, lane P9-0).
pub(crate) fn zone_environment_section(world: &Bound<'_, PyAny>) -> PyResult<Result<Arc<ZoneEnvironmentSpec>, Fault>> {
    kept(world, |configs| &configs.zone_environment, || Ok(parsed(world.py(), parse_zone_environment(world))))
}

fn parse_zone_environment(world: &Bound<'_, PyAny>) -> PyResult<ZoneEnvironmentSpec> {
    let py = world.py();
    let config_data = attribute(world, intern!(py, "model_configs"))?.get_item("zone_environment")?;
    let config = py
        .import(intern!(py, "sheltersim.domain.environment"))?
        .getattr(intern!(py, "ScenarioZoneEnvironmentConfig"))?
        .call_method1(intern!(py, "model_validate"), (config_data,))?;
    let keywords = PyDict::new(py);
    keywords.set_item("mode", "json")?;
    let dumped = config.call_method(intern!(py, "model_dump"), (), Some(&keywords))?;
    read(ZoneEnvironmentSpec::from_v(&<V as VPy>::from_py(&dumped)?))
}

// The proximity contacts (`proximity`), the support cooldown and the infrastructure.

/// `canonical_proximity_contact_config(model_configs.get("social_proximity"))`, kept for the
/// configs' stamp; what it raises is relayed as it is.
pub(crate) fn proximity_config(world: &Bound<'_, PyAny>) -> PyResult<Arc<Option<Proximity>>> {
    match proximity_section(world)? {
        Ok(config) => Ok(config),
        Err(fault) => Err(raise(&fault, || parse_proximity(world))),
    }
}

/// `proximity_config` without its raise (the host's `fill_section`, lane P9-0).
pub(crate) fn proximity_section(world: &Bound<'_, PyAny>) -> PyResult<Result<Arc<Option<Proximity>>, Fault>> {
    kept(world, |configs| &configs.proximity, || Ok(parsed(world.py(), parse_proximity(world))))
}

fn parse_proximity(world: &Bound<'_, PyAny>) -> PyResult<Option<Proximity>> {
    let py = world.py();
    let configs = attribute(world, intern!(py, "model_configs"))?;
    let config = py
        .import("sheltersim.domain.social_network")?
        .getattr("canonical_proximity_contact_config")?
        .call1((get(&configs, "social_proximity")?,))?;
    read(Proximity::from_v(&<V as VPy>::from_py(&config)?))
}

/// `model_configs.get("staff", {}).get("support_cooldown_minutes", 30)`, kept for the configs'
/// stamp; what the reads raise is relayed as it is.
pub(crate) fn support_cooldown_minutes(world: &Bound<'_, PyAny>) -> PyResult<Arc<i64>> {
    match support_cooldown_section(world)? {
        Ok(minutes) => Ok(minutes),
        Err(fault) => Err(raise(&fault, || parse_support_cooldown(world))),
    }
}

/// `support_cooldown_minutes` without its raise (the host's `fill_section`, lane P9-0).
pub(crate) fn support_cooldown_section(world: &Bound<'_, PyAny>) -> PyResult<Result<Arc<i64>, Fault>> {
    kept(world, |configs| &configs.support_cooldown_minutes, || Ok(parsed(world.py(), parse_support_cooldown(world))))
}

fn parse_support_cooldown(world: &Bound<'_, PyAny>) -> PyResult<i64> {
    let py = world.py();
    let configs = attribute(world, intern!(py, "model_configs"))?;
    let cooldown = configs
        .call_method1(intern!(py, "get"), ("staff", PyDict::new(py)))?
        .call_method1(intern!(py, "get"), ("support_cooldown_minutes", 30))?;
    super::row::exact_int(&cooldown)
}

/// `ScenarioInfrastructureSpec.model_validate(configs["infrastructure"])`, kept for the configs'
/// stamp, for the sections' tests. The rules read it through the host (`Host::contract`, policy
/// E15).
pub(crate) fn infrastructure_config(world: &Bound<'_, PyAny>) -> PyResult<Arc<Infrastructure>> {
    match infrastructure_section(world)? {
        Ok(spec) => Ok(spec),
        Err(fault) => Err(raise(&fault, || parse_infrastructure(world))),
    }
}

/// `infrastructure_config` without its raise (the host's `fill_section`, lane P9-0).
pub(crate) fn infrastructure_section(world: &Bound<'_, PyAny>) -> PyResult<Result<Arc<Infrastructure>, Fault>> {
    kept(world, |configs| &configs.infrastructure, || Ok(parsed(world.py(), parse_infrastructure(world))))
}

fn parse_infrastructure(world: &Bound<'_, PyAny>) -> PyResult<Infrastructure> {
    let py = world.py();
    let raw = attribute(world, intern!(py, "model_configs"))?.get_item("infrastructure")?;
    let spec = py
        .import("sheltersim.domain.infrastructure")?
        .getattr("ScenarioInfrastructureSpec")?
        .call_method1("model_validate", (raw,))?;
    let keywords = PyDict::new(py);
    keywords.set_item("mode", "json")?;
    let dumped = spec.call_method("model_dump", (), Some(&keywords))?;
    read(Infrastructure::from_v(&<V as VPy>::from_py(&dumped)?))
}

// The information spread (`information_spread`).

/// `InformationSpreadConfig.from_mapping(model_configs.get("information_spread"))`, as the rules
/// read it.
pub(crate) fn parse_spread(world: &Bound<'_, PyAny>) -> PyResult<InformationSpread> {
    let py = world.py();
    let configs = attribute(world, intern!(py, "model_configs"))?;
    let config = py
        .import("sheltersim.domain.models")?
        .getattr("InformationSpreadConfig")?
        .call_method1("from_mapping", (get(&configs, "information_spread")?,))?;
    spread_of(&config, configs.contains("information_spread")?)
}

/// What the rules read of an `InformationSpreadConfig`.
pub(crate) fn spread_of(config: &Bound<'_, PyAny>, present: bool) -> PyResult<InformationSpread> {
    let mut fields = indexmap::IndexMap::with_capacity(InformationSpread::FIELDS.len());
    for name in InformationSpread::FIELDS {
        let value = if name == "present" { V::Bool(present) } else { <V as VPy>::from_py(&config.getattr(name)?)? };
        fields.insert(Arc::from(name), value);
    }
    read(InformationSpread::from_v(&V::Map(fields)))
}

/// The config, kept for the configs' stamp, or what its parse raised (the host's `fill_section`).
pub(crate) fn information_spread(world: &Bound<'_, PyAny>) -> PyResult<Result<Arc<InformationSpread>, Fault>> {
    kept(world, |configs| &configs.information_spread, || Ok(parsed(world.py(), parse_spread(world))))
}

// The arrivals' sections (`overlay_rooms`, `transit_capacities`).

/// `geometry_overlay.get("schema_version") not in {3, 4}`, as the set membership.
fn room_schema(config: &Bound<'_, PyAny>) -> PyResult<bool> {
    let py = config.py();
    pyo3::types::PySet::new(py, [3, 4])?.contains(get(config, "schema_version")?)
}

/// `room_schema` of the overlay's config, as the section holds it: what it raised is raised again.
fn rooms_schema(world: &Bound<'_, PyAny>, rooms: &OverlayRooms) -> PyResult<bool> {
    match &rooms.room_schema {
        Ok(schema) => Ok(*schema),
        Err(fault) => Err(raise(fault, || room_schema(&get_map(&attribute(world, intern!(world.py(), "model_configs"))?, "geometry_overlay")?))),
    }
}

/// What reading the section `overlay_rooms` raised, read again for its error (policy E14): the
/// read of the config, then for a dict the room schema's.
pub(crate) fn reraise_rooms(world: &Bound<'_, PyAny>) -> PyResult<()> {
    let rooms = overlay_rooms(world)?;
    if rooms.is_dict && rooms.room_schema.is_err() {
        room_schema(&get_map(&attribute(world, intern!(world.py(), "model_configs"))?, "geometry_overlay")?)?;
    }
    Ok(())
}

/// The section `overlay_rooms` as the arrivals read it, for the tests: whether the config is a
/// dict, and its room schema for a dict.
fn overlay_rooms_of(world: &Bound<'_, PyAny>) -> PyResult<(bool, bool)> {
    let (mut host, holding) = host_now(world)?;
    let rooms = host.host_value(&holding, SectionId::OverlayRooms, |configs| &configs.overlay_rooms)?;
    Ok((rooms.is_dict, rooms.is_dict && rooms_schema(world, &rooms)?))
}

/// `model_configs.get("geometry_overlay", {})` as the arrivals read it: whether it is a dict, and
/// for a dict `room_schema` of it or what that raised. The section `overlay_rooms`.
pub(crate) fn overlay_rooms(world: &Bound<'_, PyAny>) -> PyResult<OverlayRooms> {
    let py = world.py();
    let config = get_map(&attribute(world, intern!(py, "model_configs"))?, "geometry_overlay")?;
    let is_dict = config.is_instance_of::<PyDict>();
    let room_schema = if is_dict { room_schema(&config).map_err(|error| fault_of(py, &error)) } else { Ok(false) };
    Ok(OverlayRooms { is_dict, room_schema })
}

/// Whether `model_configs.get("layout_routing", {}).get("max_agents_in_transit_by_edge", {})` is
/// truthy: the section `transit_capacities`.
pub(crate) fn transit_capacities(world: &Bound<'_, PyAny>) -> PyResult<bool> {
    let configs = attribute(world, intern!(world.py(), "model_configs"))?;
    get_map(&get_map(&configs, "layout_routing")?, "max_agents_in_transit_by_edge")?.is_truthy()
}

/// The section `transit_capacities` as its read site reads it, for the tests.
fn transit_capacities_of(world: &Bound<'_, PyAny>) -> PyResult<bool> {
    let (mut host, holding) = host_now(world)?;
    Ok(*host.host_value(&holding, SectionId::TransitCapacities, |configs| &configs.transit_capacities)?)
}

// The staff's operations (`staff_operations`) and the care (`care_outcome`).

/// `create_staff_operations_formula(model_configs)` and whether `policy_registry` selects it, kept
/// for the configs' stamp as their section. What the factory raises is relayed as it is. A build
/// without Python takes the built-in formulas (`StaffOperations::parse`).
pub(crate) fn staff_operations(world: &Bound<'_, PyAny>) -> PyResult<Arc<StaffOperations>> {
    match staff_operations_section(world)? {
        Ok(formula) => Ok(formula),
        Err(fault) => Err(raise(&fault, || parse_staff_operations(world))),
    }
}

/// `staff_operations` without its raise (the host's `fill_section`, lane P9-0).
pub(crate) fn staff_operations_section(world: &Bound<'_, PyAny>) -> PyResult<Result<Arc<StaffOperations>, Fault>> {
    kept(world, |configs| &configs.staff_operations, || Ok(parsed(world.py(), parse_staff_operations(world))))
}

fn parse_staff_operations(world: &Bound<'_, PyAny>) -> PyResult<StaffOperations> {
    let py = world.py();
    let configs = attribute(world, intern!(py, "model_configs"))?;
    let formula = py.import("sheltersim.policies.registry")?.getattr("create_staff_operations_formula")?.call1((&configs,))?;
    let selected = get(&configs, "policy_registry")?;
    let selected = !selected.is_none() && selected.contains("staff_operations")?;
    let model_id = exact_text(&attribute(&formula, intern!(py, "model_id"))?)?;
    let selected_source = match selected {
        true => Some(exact_text(&attribute(&formula, intern!(py, "source_id"))?)?),
        false => None,
    };
    Ok(StaffOperations { model_id, selected_source })
}

/// `create_care_outcome_formula(configs)`, which the registry makes from a selected model, kept for
/// the configs' stamp as their section `care_outcome`: read by the core through the native host
/// (`sheltersim_core::rules::support::care_formula`, lane P9-1a), its fault raised here as a second
/// parse raises it (policy E14). A build without Python takes the built-in formulas
/// (`CareOutcome::parse`).
pub(crate) fn selected_care_formula(world: &Bound<'_, PyAny>) -> PyResult<Option<Arc<CareOutcome>>> {
    care_formula(world, &mut crate::host::PyHost::of_entry(world))
}

/// `selected_care_formula` through the host an entry has.
pub(crate) fn care_formula<'py>(world: &Bound<'py, PyAny>, host: &mut crate::host::PyHost<'py>) -> PyResult<Option<Arc<CareOutcome>>> {
    let cell = super::world_core::PyWorldCell(world);
    match sheltersim_core::rules::support::care_formula(&cell, host).map_err(|error| error.0)? {
        None => Ok(None),
        Some(Ok(formula)) => Ok(Some(formula)),
        Some(Err(fault)) => Err(raise(&fault, || parse_care_outcome(world))),
    }
}

/// The section `care_outcome`, parsed if the selection names no care model too (the host's
/// `fill_section`, lane P9-0; a rule asks it only where the selection names one).
pub(crate) fn care_outcome_section(world: &Bound<'_, PyAny>) -> PyResult<Result<Arc<CareOutcome>, Fault>> {
    kept_with_selection(world, |configs| &configs.care_outcome, || Ok(parsed(world.py(), parse_care_outcome(world))))
}

fn parse_care_outcome(world: &Bound<'_, PyAny>) -> PyResult<CareOutcome> {
    let py = world.py();
    let configs = attribute(world, intern!(py, "model_configs"))?;
    let formula = py
        .import("sheltersim.policies.registry")?
        .getattr("create_care_outcome_formula")?
        .call1((configs,))
        .map_err(|_| Unsupported::new_err("the formula refuses"))?;
    let (model_id, source_id) = (exact_text(&attribute(&formula, intern!(py, "model_id"))?)?, exact_text(&attribute(&formula, intern!(py, "source_id"))?)?);
    let config = <V as VPy>::from_py(&attribute(&formula, intern!(py, "config"))?)?;
    read(CareOutcome::new(model_id, source_id, config))
}

// The extensions (`extensions_enabled`).

/// `simulator_extensions.get("enabled")` of the configs, truthy: the section `extensions_enabled`.
pub(crate) fn extensions_enabled(world: &Bound<'_, PyAny>) -> PyResult<bool> {
    let py = world.py();
    let configs = world.getattr(intern!(py, "model_configs"))?;
    let Some(configs) = super::tracked::as_readable_dict(&configs) else {
        return Err(Unsupported::new_err("the configs are not a plain dict"));
    };
    Ok(match configs.get_item(intern!(py, "simulator_extensions"))? {
        Some(extensions) => match extensions.call_method1(intern!(py, "get"), (intern!(py, "enabled"),))? {
            enabled if enabled.is_none() => false,
            enabled => enabled.is_truthy()?,
        },
        None => false,
    })
}

/// The section `extensions_enabled` as its read site reads it (the view's `build`).
pub(crate) fn extensions_enabled_of(world: &Bound<'_, PyAny>) -> PyResult<bool> {
    let (mut host, holding) = host_now(world)?;
    Ok(*host.host_value(&holding, SectionId::ExtensionsEnabled, |configs| &configs.extensions_enabled)?)
}
