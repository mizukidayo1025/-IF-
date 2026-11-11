//! `_apply_effect` (`experiments/phases/propose.py`): the glue of the core's step
//! (`sheltersim_core::rules::proposals`, lane P8 e1 of Issue #152).
//!
//! The entry reads what only Python holds, by the proposal's action: the proposal's values, whether
//! the world holds each namespace the action reaches, the configs' sections, the floor plan, the
//! Run's router, the managers (a `request` a test set on one, the resource service's config), the
//! staff service's values and the information policy's threshold. It runs the core on the world
//! borrowed whole and makes the Python tuple of what comes back. A request (`seek_help`,
//! `seek_information`) is written on one borrow, and then the LLM's hooks run through the native
//! host (`Host::prioritize_support_request`, `Host::consider_support_request`) with nothing
//! borrowed.
//!
//! A value whose read raised is read again where the core says the rule reads it, and raises
//! there: the floor plan (`PLAN_UNREAD`), the resource service's manager (`MANAGER_UNREAD`), the
//! staff service's values (`support::AFTER_UNREAD` and the others) and the information policy's
//! threshold (`THRESHOLD_UNREAD`).

use std::sync::Arc;

use indexmap::IndexMap;
use pyo3::intern;
use pyo3::prelude::*;
use pyo3::types::{PyBool, PyString, PyTuple};
use sheltersim_core::config::movement::Movement;
use sheltersim_core::config::population::AccessibilityNeeds;
use sheltersim_core::rules::kit::{Held, Kept};
use sheltersim_core::rules::proposals::{
    self as core, Action, Applied, Asked, Information, Inputs, Proposal, Seek, Sleep, Trips, Walk, INFORMATION, MANAGER_UNREAD, THRESHOLD_UNREAD,
};
use sheltersim_core::rules::support::{RequestService, AFTER_UNREAD, COOLDOWN_UNREAD, NAMESPACE_IS_NONE, OTHER_DOCUMENT, POLICY_UNREAD};

use crate::core::configs;
use crate::core::row::{attribute, exact_dict, exact_float, exact_int, exact_str, namespace_is_absent, unsupported};
use crate::core::value::{VPy, V};
use crate::core::world_core::{field_held, namespace_held, raw_dict_get, with_world, PyWorldCell};
use crate::host::PyHost;
use crate::rules::travel::{movement_held, movement_section, read_queue_rows, rows_of, staff_held, Plan, RunGraph};
use crate::state::events::write_result;
use crate::state::shared_operations::NAME as SHARED;
use crate::Unsupported;

const OVERLAY_KEY: &str = "geometry_overlay_v1";

// ---- What Python holds ----------------------------------------------------------------------------

/// `"request" in manager.__dict__`: a method set on the manager (a test's patch) is not the queue's
/// kernel.
fn patched(manager: &Bound<'_, PyAny>) -> PyResult<bool> {
    let py = manager.py();
    Ok(raw_dict_get(&manager.getattr(intern!(py, "__dict__"))?, intern!(py, "request").as_any())?.is_some())
}

/// What `ResourceServiceManager.request` reads of the manager: its `config` dict, as values.
fn manager_config(manager: &Bound<'_, PyAny>, world: &Bound<'_, PyAny>) -> PyResult<IndexMap<Arc<str>, V>> {
    let py = manager.py();
    attribute(manager, intern!(py, "world"))?;
    let config = attribute(manager, intern!(py, "config"))?;
    exact_dict(&config)?;
    exact_dict(&attribute(world, intern!(py, "model_state"))?)?;
    match <V as VPy>::from_py(&config)? {
        V::Map(config) => Ok(config),
        _ => unsupported("the config is not a dict"),
    }
}

/// `exact_int(service.<name>)` as the rule reads it.
fn service_int(service: &Bound<'_, PyAny>, name: &Bound<'_, PyString>) -> PyResult<i64> {
    exact_int(&attribute(service, name)?)
}

/// `exact_str(service.policy_version)` as the rule reads it.
fn service_policy(service: &Bound<'_, PyAny>) -> PyResult<Arc<str>> {
    Ok(Arc::from(exact_str(&attribute(service, intern!(service.py(), "policy_version"))?)?))
}

/// `policy.config.query_threshold` as the rule reads it.
fn threshold_of(policy: &Bound<'_, PyAny>) -> PyResult<f64> {
    let py = policy.py();
    exact_float(&attribute(&attribute(policy, intern!(py, "config"))?, intern!(py, "query_threshold"))?)
}

/// `shared_operations_v1` as the request takes it: absent from `model_state`, None there
/// (`NAMESPACE_IS_NONE`), the slot's tree, or another document.
fn shared_held(world: &Bound<'_, PyAny>) -> PyResult<Held> {
    let py = world.py();
    let key = PyString::intern(py, SHARED);
    if namespace_is_absent(world, &key)? {
        let table = attribute(world, intern!(py, "model_state"))?;
        return Ok(if exact_dict(&table)?.contains(&key)? { Held::Other(NAMESPACE_IS_NONE) } else { Held::Absent });
    }
    namespace_held(world, &key, OTHER_DOCUMENT)
}

/// What the step reads of the Python world for a trip action, owned here and lent to the core.
struct TripsIn {
    plan: Plan,
    section: Kept<Movement>,
    movement: Held,
    staff: Held,
    graph: RunGraph,
    knows: Kept<bool>,
    queues: Held,
    needs: Kept<AccessibilityNeeds>,
    patched: bool,
    /// The resource service's config, or what reading it raised; None for the toilet.
    config: Option<PyResult<IndexMap<Arc<str>, V>>>,
    service: Held,
    ledger: Held,
}

/// What the step reads of the Python world for `sleep`.
struct SleepIn {
    plan: Plan,
    section: Kept<Movement>,
    movement: Held,
    schema: Kept<Option<i64>>,
}

/// What the step reads of the Python world, by the action.
struct Read {
    overlay: Held,
    metric_state_held: bool,
    social_minutes: Option<i64>,
    sleep: Option<SleepIn>,
    trips: Option<TripsIn>,
    seek: Option<Seek>,
}

impl Read {
    fn of(controller: &Bound<'_, PyAny>, world: &Bound<'_, PyAny>, action: Action) -> PyResult<Self> {
        let py = world.py();
        let overlay = namespace_held(world, intern!(py, OVERLAY_KEY), "the world holds another overlay state")?;
        let metric_state_held = field_held(world, intern!(py, "metric_state"))?;
        let social_minutes = if action.is_social() {
            Some(*configs::kept_value(world, |configs| &configs.social_interaction_minutes, sheltersim_core::config::body::social_interaction_minutes)?)
        } else {
            None
        };
        let sleep = if action == Action::Sleep {
            Some(SleepIn { plan: Plan::of(world), section: movement_section(world)?, movement: movement_held(world)?, schema: crate::rules::overlay::schema_of(world)? })
        } else {
            None
        };
        let trips = if action.trips() { trips_of(controller, world, action)? } else { None };
        let seek = if action.seeks() { Some(seek_of(controller, world)?) } else { None };
        Ok(Self { overlay, metric_state_held, social_minutes, sleep, trips, seek })
    }

    fn inputs(&self) -> Inputs<'_> {
        Inputs {
            overlay: self.overlay.clone(),
            metric_state_held: self.metric_state_held,
            social_minutes: self.social_minutes,
            sleep: self.sleep.as_ref().map(|sleep| Sleep {
                walk: Walk { movement: sleep.movement.clone(), section: &sleep.section, geo: sleep.plan.get() },
                schema: &sleep.schema,
            }),
            trips: self.trips.as_ref().map(|trips| Trips {
                walk: Walk { movement: trips.movement.clone(), section: &trips.section, geo: trips.plan.get() },
                staff: trips.staff.clone(),
                graph: &trips.graph,
                knows: &trips.knows,
                queues: trips.queues.clone(),
                needs: &trips.needs,
                asked: match &trips.config {
                    None => Asked::Toilet { patched: trips.patched },
                    Some(config) => Asked::Resource { patched: trips.patched, config: config.as_ref().ok(), service: trips.service.clone(), ledger: trips.ledger.clone() },
                },
            }),
            seek: self.seek.as_ref().map(|seek| Seek {
                service: seek.service.clone(),
                shared: seek.shared.clone(),
                information: seek.information.as_ref().map(|information| Information { threshold: information.threshold, held: information.held.clone() }),
            }),
        }
    }

    /// The floor plan the action read, if any.
    fn plan(&self) -> Option<&Plan> {
        self.sleep.as_ref().map(|sleep| &sleep.plan).or(self.trips.as_ref().map(|trips| &trips.plan))
    }
}

/// What `request_toilet`, `fetch_water` and `collect_meal` read: None for a resource action without
/// a resource service, which reads nothing more.
fn trips_of(controller: &Bound<'_, PyAny>, world: &Bound<'_, PyAny>, action: Action) -> PyResult<Option<TripsIn>> {
    let py = world.py();
    let resource = action.resource().is_some();
    let manager = if resource {
        let manager = attribute(controller, intern!(py, "resource_service"))?;
        if manager.is_none() {
            return Ok(None);
        }
        manager
    } else {
        attribute(controller, intern!(py, "toilet_service"))?
    };
    let graph = rows_of(controller)?;
    let knows = configs::kept_section(world, |configs| &configs.knowledge_is_limited, sheltersim_core::config::agent_dynamics::knowledge_is_limited)?;
    // The rows of the events Python scheduled, for the core's reads of the whole queue (the edge
    // loads, the pending reservations).
    if graph.capacities.any || resource {
        read_queue_rows(world)?;
    }
    let config = if resource { Some(manager_config(&manager, world)) } else { None };
    Ok(Some(TripsIn {
        plan: Plan::of(world),
        section: movement_section(world)?,
        movement: movement_held(world)?,
        staff: staff_held(world)?,
        graph,
        knows,
        queues: crate::state::facility_queues::held(world)?,
        needs: configs::kept_section(world, |configs| &configs.accessibility_needs, AccessibilityNeeds::parse)?,
        patched: patched(&manager)?,
        config,
        service: namespace_held(world, intern!(py, "resource_service_v1"), "the world holds another service state")?,
        ledger: namespace_held(world, intern!(py, "resource_ledger_v1"), "the world holds another ledger")?,
    }))
}

/// What `seek_help` and `seek_information` read. A value whose read raised is read again where the
/// core says the rule reads it.
fn seek_of(controller: &Bound<'_, PyAny>, world: &Bound<'_, PyAny>) -> PyResult<Seek> {
    let py = world.py();
    let service = attribute(controller, intern!(py, "staff_service"))?;
    let policy = attribute(controller, intern!(py, "information_policy"))?;
    let information = if policy.is_none() {
        None
    } else {
        Some(Information {
            threshold: threshold_of(&policy).ok(),
            held: namespace_held(world, intern!(py, INFORMATION), "the information state is for the Python rule")?,
        })
    };
    Ok(Seek {
        service: RequestService {
            unresolved_after_minutes: service_int(&service, intern!(py, "unresolved_after_minutes")).ok(),
            cooldown_minutes: service_int(&service, intern!(py, "cooldown_minutes")).ok(),
            policy_version: service_policy(&service).ok(),
        },
        shared: shared_held(world)?,
        information,
    })
}

/// The core's refusal of a value whose read raised, as what reading it again raises.
fn read_again<T>(controller: &Bound<'_, PyAny>, world: &Bound<'_, PyAny>, read: &Read, result: PyResult<T>) -> PyResult<T> {
    let py = controller.py();
    let result = match read.plan() {
        Some(plan) => plan.finish(py, result),
        None => result,
    };
    let Err(error) = result else {
        return result;
    };
    if !error.is_instance_of::<Unsupported>(py) {
        return Err(error);
    }
    let reason = error.value(py).to_string();
    let again = match reason.as_str() {
        MANAGER_UNREAD => manager_config(&attribute(controller, intern!(py, "resource_service"))?, world).map(drop),
        AFTER_UNREAD => service_int(&attribute(controller, intern!(py, "staff_service"))?, intern!(py, "unresolved_after_minutes")).map(drop),
        COOLDOWN_UNREAD => service_int(&attribute(controller, intern!(py, "staff_service"))?, intern!(py, "cooldown_minutes")).map(drop),
        POLICY_UNREAD => service_policy(&attribute(controller, intern!(py, "staff_service"))?).map(drop),
        THRESHOLD_UNREAD => threshold_of(&attribute(controller, intern!(py, "information_policy"))?).map(drop),
        _ => return Err(error),
    };
    again?;
    Err(pyo3::exceptions::PyRuntimeError::new_err("a value read again did not raise"))
}

// ---- The step -------------------------------------------------------------------------------------

/// `(target_id, next_decision_at, committed)`.
fn applied_tuple<'py>(py: Python<'py>, applied: Applied) -> PyResult<Bound<'py, PyTuple>> {
    let target = match &applied.target {
        Some(target) => PyString::new(py, target).into_any(),
        None => py.None().into_bound(py),
    };
    PyTuple::new(py, [target, applied.next_decision_at.into_pyobject(py)?.into_any(), PyBool::new(py, applied.committed).to_owned().into_any()])
}

/// `_apply_effect`: the target, the next decision, and whether the decision committed.
#[pyfunction]
pub(crate) fn apply_effect<'py>(controller: &Bound<'py, PyAny>, proposal: &Bound<'py, PyAny>) -> PyResult<Bound<'py, PyTuple>> {
    let py = controller.py();
    let action = attribute(proposal, intern!(py, "action"))?;
    let action = attribute(&action, intern!(py, "_value_"))?;
    let Some(action) = Action::parse(exact_str(&action)?) else {
        return unsupported("an action unknown here");
    };
    let world = attribute(controller, intern!(py, "world"))?;
    let agent_id: Arc<str> = Arc::from(exact_str(&attribute(proposal, intern!(py, "agent_id"))?)?);
    let next_decision_at = exact_int(&attribute(proposal, intern!(py, "next_decision_at"))?)?;
    exact_dict(&attribute(&world, intern!(py, "model_state"))?)?;
    let read = Read::of(controller, &world, action)?;
    let target_id = if action.is_social() {
        let target = attribute(proposal, intern!(py, "target_id"))?;
        if target.is_none() { None } else { Some(Arc::from(exact_str(&target)?)) }
    } else {
        None
    };
    if !action.seeks() {
        let proposal = Proposal { action, agent_id, next_decision_at, target_id, observed_state_hash: None };
        let applied = with_world(&world, |world| core::apply(world, &read.inputs(), &proposal))?;
        let applied = read_again(controller, &world, &read, write_result(py, applied))?;
        return applied_tuple(py, applied);
    }
    let mut host = PyHost::of_controller(controller)?;
    // What the Provider's priority reads of the proposal besides the person (`Host`'s signature):
    // a proposal's hash is always a str.
    let observed_state_hash = proposal
        .getattr(intern!(py, "observed_state_hash"))
        .ok()
        .and_then(|hash| hash.downcast_exact::<PyString>().ok().and_then(|hash| hash.to_str().ok().map(Arc::from)));
    let proposal = Proposal { action, agent_id, next_decision_at, target_id, observed_state_hash };
    let inputs = read.inputs();
    let sought = with_world(&world, |world| core::seek_write(world, &inputs, &proposal))?;
    let sought = read_again(controller, &world, &read, write_result(py, sought))?;
    let applied = core::seek_after(&PyWorldCell(&world), &mut host, &inputs, &proposal, sought).map_err(|error| error.0)?;
    applied_tuple(py, applied)
}

pub fn register(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_function(wrap_pyfunction!(apply_effect, m)?)?;
    Ok(())
}
