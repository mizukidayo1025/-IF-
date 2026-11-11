//! `PyHost`: the core's `Host` over a Python world and its controller (Issue #152, lane P8-0b;
//! design-p8.md section 2). Every method calls the Python the kernels call today, with the same
//! arguments, so a kernel that moves into the core and calls the host where it called Python
//! changes nothing.
//!
//! A transaction's host (`core/txn.rs`) has only the world and what the transaction keeps of the
//! Python containers; a tick's host also has the controller, its `Policies` (read once per
//! controller, `core/run.rs`), the phase observer and the random source. A method that needs what
//! the host was not given refuses (`Unsupported`).
//!
//! What the Run read of the controller is answered from its `Policies`: a refusal there is the
//! read site's `Unsupported`, and an error a read raised is raised again by reading again (policy
//! E14). Nothing of the world is borrowed while a method runs (rule R6), and no method holds a
//! borrow across a call into Python.

use std::sync::Arc;

use pyo3::exceptions::{PyKeyError, PyTypeError, PyValueError};
use pyo3::intern;
use pyo3::prelude::*;
use pyo3::types::{PyBytes, PyDict, PyFloat, PyString, PyTuple};

use sheltersim_core::error::Error as CoreError;
use sheltersim_core::host::{
    Contract, Decision, DecisionSink, ErrorClass, Host, HostFailure, Llm, Part, Phase, Stage, Trace, TraceLevel, TxnKind,
};
use sheltersim_core::rules::metrics::MetricSample;
use sheltersim_core::run::Policies;
use sheltersim_core::value::V;
use sheltersim_core::world::Snapshot;

use crate::core::census;
use crate::core::errors::{self, to_py_err};
use crate::core::row::{attribute, exact_dict, exact_list, exact_str};
use crate::core::run::run_of;
use crate::core::value::VPy;
use crate::core::world_core::{attached_state, core_of, mirror_of, raw_dict_get, raw_list_append};
use crate::eventlog::EventStore;
use crate::state::information::{NAME as INFORMATION, SCHEMA_VERSION as INFORMATION_VERSION};
use crate::state::events::QueueStore;
use crate::Unsupported;

/// A Python error, as the host's error.
pub(crate) struct HostError(pub(crate) PyErr);

impl From<CoreError> for HostError {
    fn from(error: CoreError) -> Self {
        HostError(to_py_err(error))
    }
}

impl From<PyErr> for HostError {
    fn from(error: PyErr) -> Self {
        HostError(error)
    }
}

/// The class by `isinstance`, as an `except` clause matches it.
impl HostFailure for HostError {
    fn class(&self) -> ErrorClass {
        Python::with_gil(|py| {
            let error = &self.0;
            if error.is_instance_of::<Unsupported>(py) {
                ErrorClass::Refused
            } else if error.is_instance_of::<PyValueError>(py) {
                ErrorClass::Value
            } else if error.is_instance_of::<PyKeyError>(py) {
                ErrorClass::Key
            } else if error.is_instance_of::<PyTypeError>(py) {
                ErrorClass::Type
            } else {
                ErrorClass::Other
            }
        })
    }
}

/// What a host method needs that this host was not given.
const NO_CONTROLLER: &str = "the host has no controller";

/// The native host.
pub(crate) struct PyHost<'py> {
    world: Bound<'py, PyAny>,
    controller: Option<Bound<'py, PyAny>>,
    /// The controller's `Policies`, shared with its Run so that no lock is taken here.
    policies: Option<Arc<Policies>>,
    /// `TickEngine.phase_observer`, or None.
    observer: Option<Bound<'py, PyAny>>,
    /// The tick's random source.
    rng: Option<Bound<'py, PyAny>>,
    /// What a transaction keeps of the Python containers (`_txn_canonicalized`).
    pub(crate) kept: Bound<'py, PyDict>,
}

impl<'py> PyHost<'py> {
    /// A transaction's host: the world and what the transaction keeps.
    pub(crate) fn of_world(world: &Bound<'py, PyAny>, kept: Bound<'py, PyDict>) -> Self {
        Self { world: world.clone(), controller: None, policies: None, observer: None, rng: None, kept }
    }

    /// A tick's host: the controller's world, its `Policies`, and nothing a transaction keeps.
    pub(crate) fn of_controller(controller: &Bound<'py, PyAny>) -> PyResult<Self> {
        let py = controller.py();
        let world = attribute(controller, intern!(py, "world"))?;
        let policies = match run_of(controller) {
            Ok(run) => Some(run.get().policies()?),
            Err(_) => None,
        };
        Ok(Self { world, controller: Some(controller.clone()), policies, observer: None, rng: None, kept: PyDict::new(py) })
    }

    /// The host of a holder of the update steps (lane P8 a3): its policies as given
    /// (`run::policies_of`), and its world if it has one, which only the methods that reach the
    /// world use (a test's namespace may hold no world).
    pub(crate) fn of_holder(controller: &Bound<'py, PyAny>, policies: Arc<Policies>) -> Self {
        let py = controller.py();
        let world = controller.getattr(intern!(py, "world")).unwrap_or_else(|_| py.None().into_bound(py));
        Self { world, controller: Some(controller.clone()), policies: Some(policies), observer: None, rng: None, kept: PyDict::new(py) }
    }

    /// With `TickEngine.phase_observer` (None for none).
    pub(crate) fn with_observer(mut self, observer: &Bound<'py, PyAny>) -> Self {
        self.observer = (!observer.is_none()).then(|| observer.clone());
        self
    }

    /// With the tick's random source.
    pub(crate) fn with_rng(mut self, rng: &Bound<'py, PyAny>) -> Self {
        self.rng = Some(rng.clone());
        self
    }

    fn py(&self) -> Python<'py> {
        self.world.py()
    }

    fn controller(&self) -> Result<&Bound<'py, PyAny>, HostError> {
        self.controller.as_ref().ok_or_else(|| HostError(Unsupported::new_err(NO_CONTROLLER)))
    }

    fn policies(&self) -> Result<&Policies, HostError> {
        self.policies.as_deref().ok_or_else(|| HostError(Unsupported::new_err(NO_CONTROLLER)))
    }

    /// A value the Run read of the controller: a refusal is the read site's `Unsupported`; any other
    /// fault is what reading again raises (policy E14).
    fn answer<T: Clone>(
        &self,
        read: &Result<T, sheltersim_core::config::Fault>,
        again: impl FnOnce(&Bound<'py, PyAny>) -> PyResult<()>,
    ) -> Result<T, HostError> {
        match read {
            Ok(value) => Ok(value.clone()),
            Err(fault) if fault.is_refusal() => Err(HostError(Unsupported::new_err(fault.text.to_string()))),
            Err(_) => match self.controller().map_err(|error| error.0).and_then(again) {
                Err(error) => Err(HostError(error)),
                Ok(()) => Err(HostError(Unsupported::new_err("a fault of the policies did not come again"))),
            },
        }
    }

    /// `controller.social_policy`, read now, as the kernels read it at each call.
    fn social_policy(&self) -> Result<Bound<'py, PyAny>, HostError> {
        Ok(attribute(self.controller()?, intern!(self.py(), "social_policy"))?)
    }

    /// `controller.artifacts`, read now.
    fn artifacts(&self) -> Result<Bound<'py, PyAny>, HostError> {
        Ok(self.controller()?.getattr(intern!(self.py(), "artifacts"))?)
    }

    /// The stores of the world's queue and log, which its core holds while a transaction is open.
    fn stores(&self) -> Result<(Option<Py<QueueStore>>, Option<Py<EventStore>>), HostError> {
        let core = core_of(&self.world)?;
        let stores = core.try_borrow().map_err(|error| HostError(error.into()))?.event_stores(self.py());
        Ok(stores)
    }

    fn state_mirror(&self, name: &str, argument: Option<bool>) -> Result<(), HostError> {
        let py = self.py();
        let function = py.import(intern!(py, "sheltersim.core.state_mirror"))?.getattr(name)?;
        match argument {
            Some(flag) => function.call1((&self.world, flag)),
            None => function.call1((&self.world, &self.kept)),
        }?;
        Ok(())
    }

    /// `social_policy.<name>(world, **options)`.
    fn consider(&self, name: &Bound<'py, PyString>, options: &[(&Bound<'py, PyString>, &str)]) -> Result<(), HostError> {
        let py = self.py();
        let kwargs = PyDict::new(py);
        for (key, value) in options {
            kwargs.set_item(*key, *value)?;
        }
        self.social_policy()?.call_method(name, (&self.world,), Some(&kwargs))?;
        Ok(())
    }
}

/// `TraceLevel` of the Run's trace, read again for its error (`Level::of` of `rules/trace.rs`).
fn read_level(controller: &Bound<'_, PyAny>) -> PyResult<()> {
    let artifacts = controller.getattr(intern!(controller.py(), "artifacts"))?;
    crate::rules::trace::Level::of(&artifacts).map(drop)
}

impl<'py> Host for PyHost<'py> {
    type Error = HostError;

    fn validate_candidate(&mut self, _kind: TxnKind, stage: Stage) -> Result<(), HostError> {
        self.state_mirror("_txn_validate", Some(stage == Stage::Candidate))
    }

    fn canonicalized(&mut self, _kind: TxnKind, before: &Snapshot) -> Result<(), HostError> {
        let py = self.py();
        if let (Some(queue), _) = self.stores()? {
            // The queue's order is the same before and after, under new serials.
            let moved = queue.get().with_queue(py, |now| {
                before.queue().ordered().iter().zip(now.ordered()).map(|((_, old), (_, new))| (*old, *new)).collect::<Vec<_>>()
            });
            queue.get().txn_remap(py, &moved?)?;
        }
        self.state_mirror("_txn_canonicalized", None)
    }

    fn restored(&mut self, _kind: TxnKind) -> Result<(), HostError> {
        let py = self.py();
        let (queue, log) = self.stores()?;
        if let Some(queue) = queue {
            queue.get().txn_rollback()?;
        }
        if let Some(log) = log {
            log.get().cut_to_log(py)?;
        }
        self.state_mirror("_txn_restored", None)
    }

    fn committed(&mut self, _kind: TxnKind) -> Result<(), HostError> {
        if let (Some(queue), _) = self.stores()? {
            queue.get().txn_commit()?;
        }
        self.state_mirror("_txn_committed", None)
    }

    /// `world.validate()`.
    fn validate_world(&mut self) -> Result<(), HostError> {
        self.world.call_method0(intern!(self.py(), "validate"))?;
        Ok(())
    }

    /// `validate_simulator_extensions(world)` of `domain/simulator_state.py`.
    fn validate_extensions(&mut self) -> Result<(), HostError> {
        let py = self.py();
        py.import(intern!(py, "sheltersim.domain.simulator_state"))?
            .getattr(intern!(py, "validate_simulator_extensions"))?
            .call1((&self.world,))?;
        Ok(())
    }

    /// `Model.model_validate(input).model_dump(mode="json")`, the model's error as it is.
    fn contract(&mut self, which: &Contract, input: &V) -> Result<V, HostError> {
        let py = self.py();
        let (module, name) = which.model();
        let found = py.import(module)?.getattr(name)?;
        let model = match which {
            Contract::EffectParameters(operator) => found.get_item(&**operator)?,
            _ => found,
        };
        let validated = model.call_method1(intern!(py, "model_validate"), (input.to_py(py)?,))?;
        let options = PyDict::new(py);
        options.set_item(intern!(py, "mode"), intern!(py, "json"))?;
        let dumped = validated.call_method(intern!(py, "model_dump"), (), Some(&options))?;
        Ok(V::from_py(&dumped)?)
    }

    fn llm(&self) -> Result<Llm, HostError> {
        let sinks = &self.policies()?.sinks;
        let py = self.py();
        let router = self.answer(&sinks.llm_router, |controller| controller.getattr(intern!(py, "llm_router")).map(drop))?;
        let social_policy = self.answer(&sinks.social_policy, |controller| controller.getattr(intern!(py, "social_policy")).map(drop))?;
        Ok(Llm { router, social_policy })
    }

    /// `controller._prioritize_support_request(request_id, proposal)`, whose proposal is read only
    /// for `agent_id` and `observed_state_hash`: a `SimpleNamespace` of the two stands for it.
    fn prioritize_support_request(&mut self, request_id: &str, agent_id: &str, observed_state_hash: &str) -> Result<(), HostError> {
        let py = self.py();
        let fields = PyDict::new(py);
        fields.set_item(intern!(py, "agent_id"), agent_id)?;
        fields.set_item(intern!(py, "observed_state_hash"), observed_state_hash)?;
        let proposal = py.import(intern!(py, "types"))?.getattr(intern!(py, "SimpleNamespace"))?.call((), Some(&fields))?;
        self.controller()?.call_method1(intern!(py, "_prioritize_support_request"), (request_id, proposal))?;
        Ok(())
    }

    fn consider_support_request(&mut self, agent_id: &str, request_id: &str) -> Result<(), HostError> {
        let py = self.py();
        self.consider(
            intern!(py, "consider_support_request"),
            &[(intern!(py, "agent_id"), agent_id), (intern!(py, "request_id"), request_id)],
        )
    }

    fn consider_complaint_response(&mut self, agent_id: &str, request_id: &str) -> Result<(), HostError> {
        let py = self.py();
        self.consider(
            intern!(py, "consider_complaint_response"),
            &[(intern!(py, "agent_id"), agent_id), (intern!(py, "request_id"), request_id)],
        )
    }

    fn consider_cooperation(&mut self, initiator: &str, participant: &str, trigger_event_id: &str) -> Result<(), HostError> {
        let py = self.py();
        self.consider(
            intern!(py, "consider_cooperation"),
            &[
                (intern!(py, "initiator_agent_id"), initiator),
                (intern!(py, "participant_agent_id"), participant),
                (intern!(py, "trigger_event_id"), trigger_event_id),
            ],
        )
    }

    fn consider_information_reaction(
        &mut self,
        agent_id: &str,
        participant: &str,
        message_id: &str,
        trigger_event_id: &str,
    ) -> Result<(), HostError> {
        let py = self.py();
        self.consider(
            intern!(py, "consider_information_reaction"),
            &[
                (intern!(py, "agent_id"), agent_id),
                (intern!(py, "participant_agent_id"), participant),
                (intern!(py, "message_id"), message_id),
                (intern!(py, "trigger_event_id"), trigger_event_id),
            ],
        )
    }

    /// The tick driver's metrics phase: a `MetricsRecorder` the sink holds takes a snapshot of the
    /// sample (`MetricsRecorder.capture`, which the sink's `capture_metric` is); any other sink, or
    /// a refusal of the recorder's, gets `artifacts.capture_metric(world, minute=)`.
    fn capture_metric(&mut self, sample: &MetricSample, minute: i64) -> Result<(), HostError> {
        let py = self.py();
        let artifacts = self.artifacts()?;
        let Ok(minute) = minute.into_pyobject(py).map(|minute| minute.into_any());
        let sink = artifacts.get_type();
        let recorder = sink.getattr(intern!(py, "capture_metric_as_defined")).ok().filter(|defined| {
            sink.getattr(intern!(py, "capture_metric")).is_ok_and(|now| now.is(defined))
        });
        let own = artifacts.getattr(intern!(py, "__dict__"))?;
        let recorder = match recorder {
            Some(_) if raw_dict_get(&own, intern!(py, "capture_metric").as_any())?.is_none() => {
                let class = py.import(intern!(py, "sheltersim.experiments.metrics"))?.getattr(intern!(py, "MetricsRecorder"))?;
                raw_dict_get(&own, intern!(py, "metrics").as_any())?.filter(|recorder| recorder.get_type().is(&class))
            }
            _ => None,
        };
        let by_sink = || -> PyResult<()> {
            let options = PyDict::new(py);
            options.set_item(intern!(py, "minute"), &minute)?;
            artifacts.call_method(intern!(py, "capture_metric"), (&self.world,), Some(&options)).map(drop)
        };
        match &recorder {
            Some(recorder) => match crate::rules::metrics::record(&self.world, recorder, &minute, sample) {
                Err(error) if error.is_instance_of::<Unsupported>(py) => {
                    census::caught(py, "metrics.capture", &error);
                    by_sink()?
                }
                other => other?,
            },
            None => by_sink()?,
        }
        Ok(())
    }

    /// `sink_of` of `rules/decisions.rs`: the sink's declaration, read once, and whether a writer's
    /// trace is under way, read now.
    fn decision_sink(&self) -> Result<DecisionSink, HostError> {
        let py = self.py();
        let policies = self.policies()?;
        let kind = self.answer(&policies.sinks.decision_sink, |controller| {
            let artifacts = controller.getattr(intern!(py, "artifacts"))?;
            let kind = artifacts.get_type().getattr(intern!(py, "__dict__"))?.get_item(intern!(py, "decision_sink"))?;
            kind.downcast_exact::<PyString>()?;
            Ok(())
        })?;
        if kind != DecisionSink::Artifacts {
            return Ok(kind);
        }
        let level = self.trace_level()?;
        let artifacts = self.artifacts()?;
        let started = artifacts.getattr(intern!(py, "_started"))?.is_truthy()?;
        let finalized = artifacts.getattr(intern!(py, "_finalized"))?.is_truthy()?;
        Ok(match level {
            _ if !started || finalized => DecisionSink::Python,
            TraceLevel::Off => DecisionSink::Silent,
            TraceLevel::Summary | TraceLevel::Full => DecisionSink::Artifacts,
        })
    }

    fn decision(&mut self, decision: Decision) -> Result<(), HostError> {
        let py = self.py();
        let artifacts = self.artifacts()?;
        match decision {
            Decision::Document { document, observed } => {
                artifacts.call_method1(intern!(py, "record_decision_document"), (document.to_py(py)?, observed.to_py(py)?))?;
            }
            Decision::Indexed(raw) => {
                let observations = attribute(&artifacts, intern!(py, "observations"))?;
                exact_list(&observations)?;
                raw_list_append(&observations, PyBytes::new(py, &raw).as_any())?;
            }
        }
        Ok(())
    }

    /// `sink.world is world`, then `policy_sources.get(version)` of the indexed sink: a missing
    /// `world` raises Unsupported; anything else the bytes cannot write is the sink's own
    /// `record_decision`'s (None).
    fn decision_source(&mut self, policy_version: &str) -> Result<Option<Option<Arc<str>>>, HostError> {
        let py = self.py();
        let artifacts = self.artifacts()?;
        if !attribute(&artifacts, intern!(py, "world"))?.is(&self.world) {
            return Ok(None);
        }
        let Ok(sources) = attribute(&artifacts, intern!(py, "policy_sources")) else {
            return Ok(None);
        };
        let Ok(sources) = exact_dict(&sources) else {
            return Ok(None);
        };
        let source = raw_dict_get(sources.as_any(), PyString::new(py, policy_version).as_any())?;
        Ok(match source {
            None => Some(None),
            Some(source) if source.is_none() => Some(None),
            Some(source) => match exact_str(&source) {
                Ok(text) => Some(Some(Arc::from(text))),
                Err(_) => None,
            },
        })
    }

    fn trace_level(&self) -> Result<TraceLevel, HostError> {
        let policies = self.policies()?;
        self.answer(&policies.sinks.trace_level, read_level)
    }

    fn trace(&mut self, document: Trace) -> Result<(), HostError> {
        let py = self.py();
        let artifacts = self.artifacts()?;
        match document {
            Trace::Psychology { document, contributions } => {
                let options = PyDict::new(py);
                options.set_item(intern!(py, "agent_contributions"), contributions.to_py(py)?)?;
                artifacts.call_method(intern!(py, "record_psychology_trace"), (document.to_py(py)?,), Some(&options))?;
            }
            Trace::SocialContact(document) => {
                artifacts.call_method1(intern!(py, "record_social_contact_trace"), (document.to_py(py)?,))?;
            }
            Trace::InformationSpread(document) => {
                artifacts.call_method1(intern!(py, "record_information_spread_trace"), (document.to_py(py)?,))?;
            }
        }
        Ok(())
    }

    /// `observer(TickPhase(name), before)`.
    fn phase(&mut self, phase: Phase, before: bool) -> Result<(), HostError> {
        let Some(observer) = &self.observer else {
            return Ok(());
        };
        let py = self.py();
        let member = py.import(intern!(py, "sheltersim.core.engine"))?.getattr(intern!(py, "TickPhase"))?.call1((phase.name(),))?;
        observer.call1((member, before))?;
        Ok(())
    }

    /// `controller._refresh_space()`, as the dispatcher calls it after a geometry or structure event.
    fn space_refreshed(&mut self) -> Result<(), HostError> {
        let py = self.py();
        self.controller()?.call_method0(intern!(py, "_refresh_space"))?;
        Ok(())
    }

    /// `upgrade_information_runtime_state(state)` of an information state whose schema is not 2, as
    /// the information events did it first: its `ValueError` is the event's failure. Then the row is
    /// read again from the tree if the tree no longer fit it (`refit`).
    fn upgrade_information_state(&mut self) -> Result<(), HostError> {
        let py = self.py();
        let table = attribute(&self.world, intern!(py, "model_state"))?;
        let Some(state) = attached_state(&table, INFORMATION)? else {
            return Err(HostError(Unsupported::new_err("the namespace is absent")));
        };
        if state.call_method1(intern!(py, "get"), (intern!(py, "schema_version"),))?.ne(INFORMATION_VERSION)? {
            let upgrade = py.import(intern!(py, "sheltersim.domain.information"))?.getattr(intern!(py, "upgrade_information_runtime_state"))?;
            match upgrade.call1((&state,)) {
                Ok(_) => {}
                Err(error) if error.is_instance_of::<PyValueError>(py) => {
                    return errors::failed(py, format!("invalid information-spread state: {}", error.value(py).str()?)).map_err(HostError);
                }
                Err(error) => return Err(HostError(error)),
            }
        }
        let mirror = mirror_of(&self.world, INFORMATION)?;
        if !mirror.try_borrow().map_err(PyErr::from)?.root_is(&state) {
            return Err(HostError(Unsupported::new_err("the world holds another document than the row's")));
        }
        if !mirror.getattr(intern!(py, "has_row"))?.is_truthy()? {
            mirror.call_method0(intern!(py, "refit"))?;
        }
        Ok(())
    }

    /// `rng.random(*parts)` of a random source that is not a `CounterRNG`: a float, or a refusal.
    fn draw(&mut self, parts: &[Part]) -> Result<f64, HostError> {
        let py = self.py();
        let rng = self.rng.as_ref().ok_or_else(|| HostError(Unsupported::new_err("the host has no random source")))?;
        let mut arguments: Vec<Bound<'py, PyAny>> = Vec::with_capacity(parts.len());
        for part in parts {
            arguments.push(match part {
                Part::Text(text) => PyString::new(py, text).into_any(),
                Part::Int(number) => number.into_pyobject(py).map_or_else(|never| match never {}, |number| number.into_any()),
            });
        }
        let drawn = rng.call_method1(intern!(py, "random"), PyTuple::new(py, arguments)?)?;
        match drawn.downcast_exact::<PyFloat>() {
            Ok(drawn) => Ok(drawn.value()),
            Err(_) => Err(HostError(Unsupported::new_err("a draw that is not a float"))),
        }
    }
}

// ---- For tests ---------------------------------------------------------------------------------

fn texts(arguments: &Bound<'_, PyTuple>) -> PyResult<Vec<String>> {
    arguments.iter().map(|item| item.extract::<String>()).collect()
}

/// One method of the native host of `controller`, by name, with plain arguments: for the tests of
/// each method's Python call (`tests/integration/test_native_host.py`).
#[pyfunction]
#[pyo3(signature = (controller, method, arguments, *, observer=None, rng=None))]
pub(crate) fn host_call<'py>(
    controller: &Bound<'py, PyAny>,
    method: &str,
    arguments: &Bound<'py, PyTuple>,
    observer: Option<&Bound<'py, PyAny>>,
    rng: Option<&Bound<'py, PyAny>>,
) -> PyResult<Bound<'py, PyAny>> {
    let py = controller.py();
    let mut host = PyHost::of_controller(controller)?;
    if let Some(observer) = observer {
        host = host.with_observer(observer);
    }
    if let Some(rng) = rng {
        host = host.with_rng(rng);
    }
    let none = || Ok(py.None().into_bound(py));
    let item = |index: usize| arguments.get_item(index);
    let done = |result: Result<(), HostError>| result.map_err(|error| error.0);
    match method {
        "validate_world" => done(host.validate_world()).and_then(|()| none()),
        "validate_extensions" => done(host.validate_extensions()).and_then(|()| none()),
        "contract" => {
            let name: String = item(0)?.extract()?;
            let which = match name.as_str() {
                "route_access" => Contract::RouteAccess,
                "equipment_transfer" => Contract::EquipmentTransfer,
                "move_facility" => Contract::MoveFacility,
                "partition" => Contract::Partition,
                "temporary_room" => Contract::TemporaryRoom,
                "guard" => Contract::Guard,
                "zone_environment" => Contract::ZoneEnvironment,
                "infrastructure" => Contract::Infrastructure,
                "population_spec" => Contract::PopulationSpec,
                "arrival_context" => Contract::ArrivalContext,
                "person_traits" => Contract::PersonTraits,
                "presence_command" => Contract::PresenceCommand,
                "facility_geometry" => Contract::FacilityGeometry,
                "spatial_constraints" => Contract::SpatialConstraints,
                operator => Contract::EffectParameters(Arc::from(operator)),
            };
            let input = V::from_py(&item(1)?)?;
            host.contract(&which, &input).map_err(|error| error.0)?.to_py(py)
        }
        "llm" => {
            let llm = host.llm().map_err(|error| error.0)?;
            Ok((llm.router, llm.social_policy).into_pyobject(py)?.into_any())
        }
        "prioritize_support_request" => {
            let [request, agent, hash]: [String; 3] = texts(arguments)?.try_into().map_err(|_| PyValueError::new_err("three texts"))?;
            done(host.prioritize_support_request(&request, &agent, &hash)).and_then(|()| none())
        }
        "consider_support_request" | "consider_complaint_response" => {
            let [agent, request]: [String; 2] = texts(arguments)?.try_into().map_err(|_| PyValueError::new_err("two texts"))?;
            let result = if method == "consider_support_request" {
                host.consider_support_request(&agent, &request)
            } else {
                host.consider_complaint_response(&agent, &request)
            };
            done(result).and_then(|()| none())
        }
        "consider_cooperation" => {
            let [initiator, participant, trigger]: [String; 3] = texts(arguments)?.try_into().map_err(|_| PyValueError::new_err("three texts"))?;
            done(host.consider_cooperation(&initiator, &participant, &trigger)).and_then(|()| none())
        }
        "consider_information_reaction" => {
            let [agent, participant, message, trigger]: [String; 4] =
                texts(arguments)?.try_into().map_err(|_| PyValueError::new_err("four texts"))?;
            done(host.consider_information_reaction(&agent, &participant, &message, &trigger)).and_then(|()| none())
        }
        "capture_metric" => {
            // The core's sample of the world's minute, as the metrics phase will take it.
            let world = attribute(controller, intern!(py, "world"))?;
            let minute = item(0)?;
            let sample = crate::rules::metrics::unsettled_sample(&world, minute.extract()?)?;
            done(host.capture_metric(&sample, minute.extract()?)).and_then(|()| none())
        }
        "decision_sink" => {
            let sink = host.decision_sink().map_err(|error| error.0)?;
            Ok(format!("{sink:?}").into_pyobject(py)?.into_any())
        }
        "decision" => {
            let decision = match item(0)?.downcast::<PyBytes>() {
                Ok(raw) => Decision::Indexed(raw.as_bytes().to_vec()),
                Err(_) => Decision::Document { document: V::from_py(&item(0)?)?, observed: V::from_py(&item(1)?)? },
            };
            done(host.decision(decision)).and_then(|()| none())
        }
        "trace_level" => {
            let level = host.trace_level().map_err(|error| error.0)?;
            Ok(format!("{level:?}").into_pyobject(py)?.into_any())
        }
        "trace" => {
            let kind: String = item(0)?.extract()?;
            let document = V::from_py(&item(1)?)?;
            let trace = match kind.as_str() {
                "psychology" => Trace::Psychology { document, contributions: V::from_py(&item(2)?)? },
                "social_contact" => Trace::SocialContact(document),
                _ => Trace::InformationSpread(document),
            };
            done(host.trace(trace)).and_then(|()| none())
        }
        "phase" => {
            let name: String = item(0)?.extract()?;
            let phase = Phase::ORDER.into_iter().find(|phase| phase.name() == name).ok_or_else(|| PyValueError::new_err("no such phase"))?;
            done(host.phase(phase, item(1)?.extract()?)).and_then(|()| none())
        }
        "space_refreshed" => done(host.space_refreshed()).and_then(|()| none()),
        "draw" => {
            let mut parts = Vec::with_capacity(arguments.len());
            for part in arguments.iter() {
                parts.push(match part.extract::<i64>() {
                    Ok(number) if !part.is_instance_of::<PyString>() => Part::Int(number),
                    _ => Part::Text(Arc::from(part.extract::<String>()?)),
                });
            }
            let drawn = host.draw(&parts).map_err(|error| error.0)?;
            Ok(PyFloat::new(py, drawn).into_any())
        }
        _ => Err(PyValueError::new_err(format!("no host method {method:?}"))),
    }
}
