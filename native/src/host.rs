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

use sheltersim_core::config::{Configs, Fault, Filled, Lazy, Section, SectionId};
use sheltersim_core::error::Error as CoreError;
use sheltersim_core::events::writer::WriteError;
use sheltersim_core::events::{EventRow, EventStatus};
use sheltersim_core::rules::behavior::Proposal;
use sheltersim_core::rules::common_operations::Logged;
use sheltersim_core::rules::decisions::Observed;
use sheltersim_core::rules::world_view::ViewData;
use sheltersim_core::host::{
    Again, MetricSink,
    Contract, Decision, DecisionSink, ErrorClass, Foreign, ForeignRecord, Holding, Host, HostFailure, Llm, Part, Phase, Stage, Trace,
    TraceLevel, TxnKind,
};
use sheltersim_core::rules::metrics::MetricSample;
use sheltersim_core::run::Policies;
use sheltersim_core::value::V;
use sheltersim_core::world::Snapshot;

use crate::core::census;
use crate::core::configs;
use crate::core::errors::{self, to_py_err, Outcome};
use crate::core::row::{attribute, exact_dict, exact_list, exact_str};
use crate::core::run::run_of;
use crate::core::value::VPy;
use crate::core::world_core::{attached_state, core_of, holding_of, mirror_of, raw_dict_get, raw_list_append};
use crate::eventlog::EventStore;
use crate::rules::movement_geometry::{Geo, GeoPy};
use crate::state::information::{NAME as INFORMATION, SCHEMA_VERSION as INFORMATION_VERSION};
use crate::state::events::{frozen, write_error, EventRowPy, QueueStore};
use crate::Unsupported;

/// A Python error, as the host's error.
pub(crate) struct HostError(pub(crate) PyErr);

impl From<CoreError> for HostError {
    fn from(error: CoreError) -> Self {
        HostError(to_py_err(error))
    }
}

impl From<WriteError> for HostError {
    fn from(error: WriteError) -> Self {
        HostError(Python::with_gil(|py| write_error(py, error)))
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

    /// `str(exc)`.
    fn text(&self) -> String {
        Python::with_gil(|py| self.0.value(py).to_string())
    }

    /// `isinstance(exc, (EventApplicationError, EventSuppressed))`.
    fn is_outcome(&self) -> bool {
        Python::with_gil(|py| errors::is_outcome(py, &self.0))
    }

    /// `except EventSuppressed`, then `except EventApplicationError`.
    fn outcome(&self) -> Option<sheltersim_core::error::Outcome> {
        Python::with_gil(|py| errors::outcome_of(py, &self.0))
    }
}

/// A contract's input as the Python rule gave it to `model_validate`. A guard and an effect's
/// parameters are an event's values, which the event holds frozen (`_freeze_json`: a mapping proxy
/// and tuples), so the contract's texts (`input_value=...`) are the same; `set_staff_role` builds a
/// dict around the payload's skills (lane P8 d3). The other inputs are plain dicts and lists.
fn contract_input<'py>(py: Python<'py>, which: &Contract, input: &V) -> PyResult<Bound<'py, PyAny>> {
    match (which, input) {
        (Contract::EffectParameters(operator), V::Map(fields)) if &**operator == "set_staff_role" => {
            let built = PyDict::new(py);
            for (key, value) in fields {
                built.set_item(&**key, frozen(py, value)?)?;
            }
            Ok(built.into_any())
        }
        (Contract::Guard | Contract::EffectParameters(_), _) => frozen(py, input),
        _ => input.to_py(py),
    }
}

/// What a host method needs that this host was not given.
const NO_CONTROLLER: &str = "the host has no controller";
/// The refusal of a section asked of configs that are not the world's now.
const CONFIGS_MOVED: &str = "the configs are not the world's now";
/// The refusal of a foreign event whose type no handler is registered for.
const NO_HANDLER: &str = "no handler is registered for the event's type";
/// The refusal of an observed state only the sink's own caller holds.
const NO_OBSERVED: &str = "an observed state the host cannot make";

/// Where a read site's accessor left a section (`Host::fill_section`): in `configs`, which keep it;
/// else, for configs no slot follows, put there now; else in configs of its own.
fn finish<T>(configs: &Arc<Configs>, get: fn(&Configs) -> &Lazy<T>, read: Result<Arc<T>, Fault>) -> Filled {
    if get(configs).get().is_some() {
        return Filled::Kept;
    }
    let section = match read {
        Ok(value) => Section::Ok(value),
        Err(fault) => Section::Fault(fault),
    };
    if configs.stamp.is_none() {
        let _ = get(configs).get_or_parse(|| section);
        return Filled::Kept;
    }
    let once = Arc::new(Configs::new(None));
    let _ = get(&once).get_or_parse(|| section);
    Filled::Once(once)
}

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
    /// The object of the event a handler applies, which `validate_in_flight` hands Python as the
    /// rule did (lane P8 d2).
    in_flight: Option<Bound<'py, PyAny>>,
    /// What the world holds, as last read (`holding`, lane P9-0): None once a call of this host
    /// ran Python that may have written `model_state` (the dirty flag), so the next ask reads it
    /// again.
    holding: Option<Arc<Holding>>,
    /// How many times this host read the holding (for the tests).
    holding_reads: usize,
    /// The handler a foreign event goes to, where the caller holds it (lane P9-1a: a travel
    /// completion whose manager has its own `request`); else the dispatcher's registered one.
    handler: Option<Bound<'py, PyAny>>,
}

impl<'py> PyHost<'py> {
    /// A transaction's host: the world and what the transaction keeps.
    pub(crate) fn of_world(world: &Bound<'py, PyAny>, kept: Bound<'py, PyDict>) -> Self {
        Self { world: world.clone(), controller: None, policies: None, observer: None, rng: None, kept, in_flight: None, holding: None, holding_reads: 0, handler: None }
    }

    /// A tick's host: the controller's world, its `Policies`, and nothing a transaction keeps.
    pub(crate) fn of_controller(controller: &Bound<'py, PyAny>) -> PyResult<Self> {
        let py = controller.py();
        let world = attribute(controller, intern!(py, "world"))?;
        let policies = match run_of(controller) {
            Ok(run) => Some(run.get().policies()?),
            Err(_) => None,
        };
        Ok(Self { world, controller: Some(controller.clone()), policies, observer: None, rng: None, kept: PyDict::new(py), in_flight: None, holding: None, holding_reads: 0, handler: None })
    }

    /// The host of a holder of the update steps (lane P8 a3): its policies as given
    /// (`run::policies_of`), and its world if it has one, which only the methods that reach the
    /// world use (a test's namespace may hold no world).
    pub(crate) fn of_holder(controller: &Bound<'py, PyAny>, policies: Arc<Policies>) -> Self {
        let py = controller.py();
        let world = controller.getattr(intern!(py, "world")).unwrap_or_else(|_| py.None().into_bound(py));
        Self { world, controller: Some(controller.clone()), policies: Some(policies), observer: None, rng: None, kept: PyDict::new(py), in_flight: None, holding: None, holding_reads: 0, handler: None }
    }

    /// With `TickEngine.phase_observer` (None for none).
    pub(crate) fn with_observer(mut self, observer: &Bound<'py, PyAny>) -> Self {
        self.observer = (!observer.is_none()).then(|| observer.clone());
        self
    }

    /// With the object of the event a handler applies.
    pub(crate) fn with_in_flight(mut self, event: &Bound<'py, PyAny>) -> Self {
        self.in_flight = Some(event.clone());
        self
    }

    /// On this world rather than the controller's (the dispatch phase's, lane P9-1c).
    pub(crate) fn on_world(mut self, world: &Bound<'py, PyAny>) -> Self {
        self.world = world.clone();
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

    /// A call of this host is about to run Python that may write the world: what it read of the
    /// holding is read again at the next ask (rule R6).
    pub(crate) fn ran_python(&mut self) {
        self.holding = None;
    }

    /// The object of an event as Python is handed it: the handler's own where this host was given
    /// it, else one made from the row (equal to the queue's).
    fn event_object(&self, event: &EventRow) -> PyResult<Bound<'py, PyAny>> {
        let py = self.py();
        if let Some(object) = &self.in_flight {
            if attribute(object, intern!(py, "event_id"))?.eq(&*event.event_id)? {
                return Ok(object.clone());
            }
        }
        // The queue's object of a pending event, made once if Python had none (`object_of_due`).
        if let Ok(writer) = crate::state::events::EventWriter::of(&self.world) {
            if let Some(object) = writer.queue().get().object_of_id(py, &event.event_id)? {
                return Ok(object.into_bound(py));
            }
        }
        event.to_py(py)
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

/// The `MetricsRecorder` a sink holds whose `capture_metric` is the recorder's own
/// (`_MetricSink.capture_metric` as defined, not set on the sink), or None: any other sink samples
/// on its own.
fn recorder_of<'py>(artifacts: &Bound<'py, PyAny>) -> PyResult<Option<Bound<'py, PyAny>>> {
    let py = artifacts.py();
    let sink = artifacts.get_type();
    let recorder = sink.getattr(intern!(py, "capture_metric_as_defined")).ok().filter(|defined| {
        sink.getattr(intern!(py, "capture_metric")).is_ok_and(|now| now.is(defined))
    });
    let own = artifacts.getattr(intern!(py, "__dict__"))?;
    Ok(match recorder {
        Some(_) if raw_dict_get(&own, intern!(py, "capture_metric").as_any())?.is_none() => {
            let class = py.import(intern!(py, "sheltersim.experiments.metrics"))?.getattr(intern!(py, "MetricsRecorder"))?;
            raw_dict_get(&own, intern!(py, "metrics").as_any())?.filter(|recorder| recorder.get_type().is(&class))
        }
        _ => None,
    })
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
        self.ran_python();
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
        self.ran_python();
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
        self.ran_python();
        if let (Some(queue), _) = self.stores()? {
            queue.get().txn_commit()?;
        }
        self.state_mirror("_txn_committed", None)
    }

    /// `_txn_check(world)` of `core/state_mirror.py`: Unsupported, with its reason, for a world whose
    /// Python views a transaction does not give back.
    fn txn_check(&mut self) -> Result<(), HostError> {
        let py = self.py();
        py.import(intern!(py, "sheltersim.core.state_mirror"))?.getattr(intern!(py, "_txn_check"))?.call1((&self.world,))?;
        Ok(())
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

    /// `world.validate(_in_flight_event=event)`: the handler's own object of the event where this
    /// host was given it, else the event's object made from its row (equal to the queue's).
    fn validate_in_flight(&mut self, event: &EventRow) -> Result<(), HostError> {
        let py = self.py();
        let object = self.event_object(event)?;
        let options = PyDict::new(py);
        options.set_item(intern!(py, "_in_flight_event"), object)?;
        self.world.call_method(intern!(py, "validate"), (), Some(&options))?;
        Ok(())
    }

    /// The objects of these slots of the world's queue go (`QueueStore::forget`): the store makes
    /// them again from their rows.
    fn slots_changed(&mut self, serials: &[u64]) -> Result<(), HostError> {
        if serials.is_empty() {
            return Ok(());
        }
        crate::state::events::EventWriter::of(&self.world)?.queue().get().forget(serials)?;
        Ok(())
    }

    /// `event_log.find(event_id)`, read as the core reads a record.
    fn find_record(&mut self, event_id: &str) -> Result<Option<Logged>, HostError> {
        let found = crate::rules::common_operations::found_records(&self.world, vec![Arc::from(event_id)])?;
        Ok(found.0.into_iter().next().and_then(|(_, logged)| logged))
    }

    /// `Geo::of(world)`, the plan the PyO3 crate keeps by the stamps of the configs and the overlay.
    fn floor_plan(&mut self) -> Result<Arc<Geo>, HostError> {
        Ok(<Geo as GeoPy>::of(&self.world)?)
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
        let validated = model.call_method1(intern!(py, "model_validate"), (contract_input(py, which, input)?,))?;
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
        self.ran_python();
        let fields = PyDict::new(py);
        fields.set_item(intern!(py, "agent_id"), agent_id)?;
        fields.set_item(intern!(py, "observed_state_hash"), observed_state_hash)?;
        let proposal = py.import(intern!(py, "types"))?.getattr(intern!(py, "SimpleNamespace"))?.call((), Some(&fields))?;
        self.controller()?.call_method1(intern!(py, "_prioritize_support_request"), (request_id, proposal))?;
        Ok(())
    }

    fn consider_support_request(&mut self, agent_id: &str, request_id: &str) -> Result<(), HostError> {
        let py = self.py();
        self.ran_python();
        self.consider(
            intern!(py, "consider_support_request"),
            &[(intern!(py, "agent_id"), agent_id), (intern!(py, "request_id"), request_id)],
        )
    }

    fn consider_complaint_response(&mut self, agent_id: &str, request_id: &str) -> Result<(), HostError> {
        let py = self.py();
        self.ran_python();
        self.consider(
            intern!(py, "consider_complaint_response"),
            &[(intern!(py, "agent_id"), agent_id), (intern!(py, "request_id"), request_id)],
        )
    }

    fn consider_cooperation(&mut self, initiator: &str, participant: &str, trigger_event_id: &str) -> Result<(), HostError> {
        let py = self.py();
        self.ran_python();
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
        self.ran_python();
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
        self.ran_python();
        let artifacts = self.artifacts()?;
        let Ok(minute) = minute.into_pyobject(py).map(|minute| minute.into_any());
        let recorder = recorder_of(&artifacts)?;
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
        self.ran_python();
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
        self.ran_python();
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
        let Some(observer) = self.observer.clone() else {
            return Ok(());
        };
        self.ran_python();
        let py = self.py();
        let member = py.import(intern!(py, "sheltersim.core.engine"))?.getattr(intern!(py, "TickPhase"))?.call1((phase.name(),))?;
        observer.call1((member, before))?;
        Ok(())
    }

    /// `controller._refresh_space()`, as the dispatcher calls it after a geometry or structure event.
    fn space_refreshed(&mut self) -> Result<(), HostError> {
        let py = self.py();
        self.ran_python();
        self.controller()?.call_method0(intern!(py, "_refresh_space"))?;
        Ok(())
    }

    /// What the world holds (H1): kept from the last read until a call of this host ran Python.
    fn holding(&mut self) -> Result<Option<Arc<Holding>>, HostError> {
        if let Some(holding) = &self.holding {
            return Ok(Some(holding.clone()));
        }
        let holding = Arc::new(holding_of(&self.world)?);
        self.holding_reads += 1;
        self.holding = Some(holding.clone());
        Ok(Some(holding))
    }

    /// `isinstance(model_state[name], dict)`, read past the table's own methods; a table the kernels
    /// do not read keeps the key's refusal (true).
    fn other_is_dict(&mut self, name: &str) -> Result<bool, HostError> {
        let py = self.py();
        let table = attribute(&self.world, intern!(py, "model_state"))?;
        let Some(table) = crate::core::tracked::as_readable_dict(&table) else {
            return Ok(true);
        };
        let value = crate::core::world_core::raw_dict_get(&table, PyString::new(py, name).as_any())?;
        Ok(value.is_some_and(|value| value.is_instance_of::<PyDict>()))
    }

    /// Today's parse of the section (H2), by the read site's own accessor, so it parses at the same
    /// first read and counts as it does (`config_parses`). The accessor fills the configs it reads
    /// (`configs::current`), which are these while the world holds the configs' stamp; configs no
    /// slot follows (stamp None) are filled with what it read, and a section the accessor did not
    /// keep (a test's patch of the model selection) comes back in configs of its own (`Once`).
    fn fill_section(&mut self, configs: &Arc<Configs>, which: SectionId) -> Result<Filled, HostError> {
        use sheltersim_core::config::{agent_dynamics, body, movement, overlay, population, queues, service, staff};
        use crate::rules;
        let world = &self.world;
        let py = self.py();
        if configs::current(world)?.stamp != configs.stamp {
            return Err(HostError(Unsupported::new_err(CONFIGS_MOVED)));
        }
        let raw = || attribute(world, intern!(py, "model_configs"));
        Ok(match which {
            SectionId::Selection => {
                let read = configs::selection(world)?;
                if configs::selection_is_kept(py)? {
                    finish(configs, |c| &c.selection, read)
                } else {
                    // A patched `selected_models` serves this read alone, also where the configs
                    // kept the contract's selection before (lane P9-1a).
                    let once = Arc::new(Configs::new(None));
                    let _ = once.selection.get_or_parse(|| match read {
                        Ok(value) => Section::Ok(value),
                        Err(fault) => Section::Fault(fault),
                    });
                    Filled::Once(once)
                }
            }
            SectionId::TickPlan => {
                rules::after_tick::fill_tick_plan(world, configs)?;
                Filled::Kept
            }
            SectionId::FacilityGeometry => {
                rules::movement_geometry::facility_geometry(configs, &raw()?);
                Filled::Kept
            }
            SectionId::SpatialConstraints => {
                rules::movement_geometry::spatial_constraints(configs, &raw()?);
                Filled::Kept
            }
            SectionId::StaffFatigue => finish(configs, |c| &c.staff_fatigue, configs::kept_section(world, |c| &c.staff_fatigue, staff::StaffFatigue::parse)?),
            SectionId::StaffOperations => finish(configs, |c| &c.staff_operations, rules::staff::staff_operations_section(world)?),
            SectionId::StaffOperationsKnown => {
                finish(configs, |c| &c.staff_operations_known, configs::kept_section(world, |c| &c.staff_operations_known, staff::known_staff_operations)?)
            }
            SectionId::SupportTaskMinutes => {
                finish(configs, |c| &c.support_task_minutes, configs::kept_section(world, |c| &c.support_task_minutes, staff::support_task_minutes)?)
            }
            SectionId::SupportCooldownMinutes => finish(configs, |c| &c.support_cooldown_minutes, rules::handlers::support_cooldown_section(world)?),
            SectionId::InformationSpread => finish(configs, |c| &c.information_spread, rules::information_events::information_spread(world)?),
            SectionId::BodyModels => {
                let selection = rules::body_state::selection_of(world)?;
                finish(configs, |c| &c.body_models, rules::body_state::models_section(world, &selection)?)
            }
            SectionId::SocialInteractionMinutes => finish(
                configs,
                |c| &c.social_interaction_minutes,
                configs::kept_section(world, |c| &c.social_interaction_minutes, body::social_interaction_minutes)?,
            ),
            SectionId::CareOutcome => finish(configs, |c| &c.care_outcome, rules::support::care_outcome_section(world)?),
            SectionId::AgentDynamicsPolicy => {
                finish(configs, |c| &c.agent_dynamics_policy, configs::kept_section(world, |c| &c.agent_dynamics_policy, agent_dynamics::policy)?)
            }
            SectionId::FacilityAwareness => {
                finish(configs, |c| &c.facility_awareness, configs::kept_section(world, |c| &c.facility_awareness, agent_dynamics::awareness)?)
            }
            SectionId::KnowledgeIsLimited => finish(
                configs,
                |c| &c.knowledge_is_limited,
                configs::kept_section(world, |c| &c.knowledge_is_limited, agent_dynamics::knowledge_is_limited)?,
            ),
            SectionId::ZoneEnvironment => finish(configs, |c| &c.zone_environment, rules::effects::zone_environment_section(world)?),
            SectionId::Infrastructure => finish(configs, |c| &c.infrastructure, rules::handlers::infrastructure_section(world)?),
            SectionId::Proximity => finish(configs, |c| &c.proximity, rules::handlers::proximity_section(world)?),
            SectionId::Movement => finish(configs, |c| &c.movement, configs::kept_section(world, |c| &c.movement, movement::Movement::parse)?),
            SectionId::RouteCapacities => {
                finish(configs, |c| &c.route_capacities, configs::kept_section(world, |c| &c.route_capacities, movement::RouteCapacities::parse)?)
            }
            SectionId::TransitCapacities => finish(
                configs,
                |c| &c.transit_capacities,
                configs::kept(world, |c| &c.transit_capacities, || Ok(configs::parsed(py, rules::population_lifecycle::transit_capacities(world))))?,
            ),
            SectionId::FacilityQueues => {
                finish(configs, |c| &c.facility_queues, configs::kept_section(world, |c| &c.facility_queues, queues::QueueConfig::parse)?)
            }
            SectionId::OverlaySchema => {
                finish(configs, |c| &c.overlay_schema, configs::kept_section(world, |c| &c.overlay_schema, overlay::overlay_schema)?)
            }
            SectionId::OverlayRooms => finish(
                configs,
                |c| &c.overlay_rooms,
                configs::kept(world, |c| &c.overlay_rooms, || Ok(configs::parsed(py, rules::population::overlay_rooms(world))))?,
            ),
            SectionId::ResourceService => {
                finish(configs, |c| &c.resource_service, configs::kept_section(world, |c| &c.resource_service, service::ResourceService::parse)?)
            }
            SectionId::AccessibilityNeeds => finish(
                configs,
                |c| &c.accessibility_needs,
                configs::kept_section(world, |c| &c.accessibility_needs, population::AccessibilityNeeds::parse)?,
            ),
            SectionId::ExtensionsEnabled => finish(
                configs,
                |c| &c.extensions_enabled,
                configs::kept(world, |c| &c.extensions_enabled, || Ok(configs::parsed(py, rules::world_view::extensions_enabled(world))))?,
            ),
        })
    }

    /// `controller.event_dispatcher.handlers[event.event_type].apply(world, event)` (H4): its
    /// `EventSuppressed` and `EventApplicationError` are the outcome, with `str(exc)`; the records
    /// it gives back are read as the core reads the EventLog's.
    fn apply_foreign(&mut self, event: &EventRow) -> Result<Foreign, HostError> {
        let py = self.py();
        let handler = match &self.handler {
            Some(handler) => handler.clone(),
            None => {
                let dispatcher = attribute(self.controller()?, intern!(py, "event_dispatcher"))?;
                attribute(&dispatcher, intern!(py, "handlers"))?.call_method1(intern!(py, "get"), (&*event.event_type,))?
            }
        };
        if handler.is_none() {
            return Err(HostError(Unsupported::new_err(NO_HANDLER)));
        }
        let object = self.event_object(event)?;
        self.ran_python();
        let (status, detail, returned) = match handler.call_method1(intern!(py, "apply"), (&self.world, &object)) {
            Ok(returned) => (EventStatus::Applied, None, Some(returned)),
            Err(error) => match errors::outcome_of(py, &error) {
                Some(Outcome::Suppressed) => (EventStatus::Suppressed, Some(error.value(py).str()?.to_string()), None),
                Some(Outcome::Failed) => (EventStatus::Failed, Some(error.value(py).str()?.to_string()), None),
                None => return Err(HostError(error)),
            },
        };
        let mut records = Vec::new();
        // `handler.apply(world, event) or ()`.
        if let Some(returned) = returned.filter(|returned| returned.is_truthy().unwrap_or(true)) {
            for record in returned.try_iter()? {
                let record = record?;
                let logged = crate::rules::common_operations::logged_of(&record)?;
                let detail = attribute(&record, intern!(py, "detail"))?;
                let detail = if detail.is_none() { None } else { Some(exact_str(&detail)?.to_owned()) };
                records.push(ForeignRecord { event: logged.event, realized_minute: logged.realized_minute, status: logged.status, detail });
            }
        }
        Ok(Foreign { status, detail, records })
    }

    /// `artifacts.record_decision(proposal.with_outcome(target_id, next_decision_at),
    /// observed_state=observed)` (H5), with the Python proposal made from the view's data as the
    /// rows make it.
    fn record_decision_python(
        &mut self,
        data: &ViewData,
        proposal: &Proposal,
        target_id: Option<&str>,
        next_decision_at: i64,
        observed: &Observed<'_>,
    ) -> Result<(), HostError> {
        let py = self.py();
        let made = crate::rules::behavior::proposal_of_data(py, data, &|index| PyString::new(py, &data.agents[index].id), proposal)?;
        let target = match target_id {
            Some(target) => PyString::new(py, target).into_any(),
            None => py.None().into_bound(py),
        };
        let recorded = made.call_method1(intern!(py, "with_outcome"), (target, next_decision_at))?;
        let observed = match observed {
            Observed::Row { data, index } => sheltersim_core::rules::behavior::observed_value(data, *index).to_py(py)?,
            Observed::Value(value) => value.to_py(py)?,
            Observed::Other => return Err(HostError(Unsupported::new_err(NO_OBSERVED))),
        };
        let artifacts = self.artifacts()?;
        self.ran_python();
        let options = PyDict::new(py);
        options.set_item(intern!(py, "observed_state"), observed)?;
        artifacts.call_method(intern!(py, "record_decision"), (recorded,), Some(&options))?;
        Ok(())
    }

    /// `upgrade_information_runtime_state(state)` of an information state whose schema is not 2, as
    /// the information events did it first: its `ValueError` is the event's failure. Then the row is
    /// read again from the tree if the tree no longer fit it (`refit`).
    fn upgrade_information_state(&mut self) -> Result<(), HostError> {
        let py = self.py();
        self.ran_python();
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

    // ---- A tick's phases 2-6 in the core (lane P9-2a) ----------------------------------------------

    /// The rows of the pending events Python scheduled (`QueueStore::read_rows`, H6).
    fn read_queue_rows(&mut self) -> Result<(), HostError> {
        crate::rules::travel::read_queue_rows(&self.world)?;
        Ok(())
    }

    /// A write's error as the PyO3 crate's `EventWriter` raises it (`DuplicateEventError`).
    fn write_error(&self, error: sheltersim_core::events::writer::WriteError) -> HostError {
        HostError(write_error(self.py(), error))
    }

    /// `world.model_configs` as a value (the view reads it past the slot that does not hold it), or
    /// None for configs that are not a plain dict, which the view refuses.
    fn configs_given(&mut self) -> Result<Option<V>, HostError> {
        let raw = attribute(&self.world, intern!(self.py(), "model_configs"))?;
        if crate::core::tracked::as_readable_dict(&raw).is_none() {
            return Ok(None);
        }
        Ok(Some(V::from_py(&raw)?))
    }

    /// The read again of a section's parse or of the controller's attributes, for its error.
    fn read_again(&mut self, again: Again<'_>) -> Result<(), HostError> {
        let py = self.py();
        match again {
            Again::Section(SectionId::ExtensionsEnabled) => {
                crate::rules::world_view::extensions_enabled(&self.world)?;
            }
            Again::Section(SectionId::Selection) => return Err(HostError(errors::relayed(py, configs::reraise_selection(&self.world)))),
            Again::Section(SectionId::StaffOperations) => {
                crate::rules::staff::staff_operations(&self.world)?;
            }
            Again::Section(SectionId::InformationSpread) => {
                crate::rules::information_events::parse_spread(&self.world)?;
            }
            Again::Section(SectionId::BodyModels) => {
                crate::rules::body_state::raise_models(&self.world)?;
            }
            Again::Section(SectionId::CareOutcome) => {
                crate::rules::support::selected_care_formula(&self.world)?;
            }
            Again::Section(SectionId::ZoneEnvironment) => {
                crate::rules::effects::zone_environment_spec(&self.world)?;
            }
            Again::Section(SectionId::SupportCooldownMinutes) => {
                crate::rules::handlers::support_cooldown_minutes(&self.world)?;
            }
            Again::Section(SectionId::Proximity) => {
                crate::rules::handlers::proximity_config(&self.world)?;
            }
            Again::Section(_) => {}
            Again::Policy(path) => {
                let mut found = self.controller()?.clone();
                for name in path {
                    found = found.getattr(*name)?;
                }
            }
        }
        Ok(())
    }

    /// `world.resources = {**world.resources, resource: remaining}`, a new dict.
    fn resources_changed(&mut self, index: usize, remaining: i64) -> Result<(), HostError> {
        crate::rules::resources::replace_resources(&self.world, sheltersim_core::rules::resources::Applied { index, remaining })?;
        Ok(())
    }

    /// `model_state[name] = value`.
    fn start_namespace(&mut self, name: &str, value: &V) -> Result<(), HostError> {
        let py = self.py();
        self.ran_python();
        attribute(&self.world, intern!(py, "model_state"))?.set_item(name, value.to_py(py)?)?;
        Ok(())
    }

    /// The recorder a sink holds takes the core's sample; any other sink samples on its own.
    fn metric_sink(&mut self) -> Result<MetricSink, HostError> {
        let artifacts = self.artifacts()?;
        Ok(match recorder_of(&artifacts)? {
            Some(_) => MetricSink::Recorder,
            None => MetricSink::Sink,
        })
    }

    /// `artifacts.capture_metric(world, minute=)`.
    fn capture_by_sink(&mut self, minute: i64) -> Result<(), HostError> {
        let py = self.py();
        self.ran_python();
        let artifacts = self.artifacts()?;
        let options = PyDict::new(py);
        options.set_item(intern!(py, "minute"), minute)?;
        artifacts.call_method(intern!(py, "capture_metric"), (&self.world,), Some(&options))?;
        Ok(())
    }

    /// `admit_pending_returns(world, minute)` or `admit_pending_arrivals(world, minute)` of the
    /// population rules' kernels, whose inputs this crate reads (lane P9-1c).
    fn admit_pending(&mut self, returns: bool, minute: i64) -> Result<(), HostError> {
        let py = self.py();
        self.ran_python();
        let Ok(minute) = minute.into_pyobject(py).map(|minute| minute.into_any());
        if returns {
            crate::rules::population_lifecycle::admit_pending_returns(&self.world, &minute)?;
        } else {
            crate::rules::population::admit_pending_arrivals(&self.world, &minute)?;
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
    let mut host = PyHost::of_controller(controller)?;
    if let Some(observer) = observer {
        host = host.with_observer(observer);
    }
    if let Some(rng) = rng {
        host = host.with_rng(rng);
    }
    call_on(&mut host, controller, method, arguments)
}

/// Several methods of one native host of `controller`, in turn: `[(method, arguments), ...]`, each
/// as `host_call` takes it, the answers in order. The host keeps what it read between them (lane
/// P9-0: the holding, until a call runs Python).
#[pyfunction]
pub(crate) fn host_calls<'py>(controller: &Bound<'py, PyAny>, calls: Vec<(String, Bound<'py, PyTuple>)>) -> PyResult<Vec<Bound<'py, PyAny>>> {
    let mut host = PyHost::of_controller(controller)?;
    calls.iter().map(|(method, arguments)| call_on(&mut host, controller, method, arguments)).collect()
}

/// What `Holding` answers and what the per-call reads answer, for each name: `{name: (holding,
/// read now)}` of `held`/`namespace_held` (with the refusal "other"), `is_absent`/`namespace_is_absent`
/// and, for `fields`, `field`/`field_held` (lane P9-0).
#[pyfunction]
pub(crate) fn holding_and_reads<'py>(world: &Bound<'py, PyAny>, names: Vec<String>, fields: Vec<String>) -> PyResult<Bound<'py, PyDict>> {
    use crate::core::row::namespace_is_absent;
    use crate::core::world_core::{field_held, namespace_held};
    let py = world.py();
    let holding = holding_of(world)?;
    let out = PyDict::new(py);
    let said = |answer: PyResult<bool>| match answer {
        Ok(flag) => flag.to_string(),
        Err(error) => format!("raises {}", error.value(py)),
    };
    for name in &names {
        let key = PyString::new(py, name);
        let now = (held_text(&namespace_held(world, &key, "other")?), said(namespace_is_absent(world, &key)));
        let kept = (held_text(&holding.held(name, "other")), match holding.is_absent(name) {
            Ok(flag) => flag.to_string(),
            Err(reason) => format!("raises {reason}"),
        });
        out.set_item(name, (kept, now))?;
    }
    for name in &fields {
        let key = PyString::new(py, name);
        out.set_item(name, (holding.field(name), field_held(world, &key)?))?;
    }
    Ok(out)
}

fn held_text(held: &sheltersim_core::rules::kit::Held) -> String {
    use sheltersim_core::rules::kit::Held;
    match held {
        Held::Absent => "absent".to_owned(),
        Held::Held => "held".to_owned(),
        Held::Other(reason) => (*reason).to_owned(),
    }
}

fn call_on<'py>(host: &mut PyHost<'py>, controller: &Bound<'py, PyAny>, method: &str, arguments: &Bound<'py, PyTuple>) -> PyResult<Bound<'py, PyAny>> {
    let py = controller.py();
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
        // ---- Lane P9-0 ----
        "holding" => {
            let name: String = item(0)?.extract()?;
            let holding = host.holding().map_err(|error| error.0)?.expect("the native host reads it");
            let absent = match holding.is_absent(&name) {
                Ok(flag) => flag.to_string(),
                Err(reason) => format!("raises {reason}"),
            };
            Ok((held_text(&holding.held(&name, "other")), absent, holding.field(&name)).into_pyobject(py)?.into_any())
        }
        "holding_reads" => Ok(host.holding_reads.into_pyobject(py)?.into_any()),
        "fill_section" => {
            let name: String = item(0)?.extract()?;
            let which = SectionId::ALL.iter().copied().find(|which| which.name() == name).ok_or_else(|| PyValueError::new_err("no such section"))?;
            let world = attribute(controller, intern!(py, "world"))?;
            let given = configs::current(&world)?;
            let filled = host.fill_section(&given, which).map_err(|error| error.0)?;
            let (how, held) = match &filled {
                Filled::Kept => ("kept", which.is_filled(&given)),
                Filled::Once(other) => ("once", which.is_filled(other)),
            };
            Ok((how, held, which.is_filled(&given)).into_pyobject(py)?.into_any())
        }
        "apply_foreign" => {
            let Some(event) = EventRow::from_event(&item(0)?)? else {
                return Err(PyValueError::new_err("the event has no row"));
            };
            let foreign = host.apply_foreign(&event).map_err(|error| error.0)?;
            let records: Vec<(String, &str, Option<String>, i64)> = foreign
                .records
                .iter()
                .map(|record| (record.event.event_id.to_string(), record.status.as_str(), record.detail.clone(), record.realized_minute))
                .collect();
            Ok((foreign.status.as_str(), foreign.detail, records).into_pyobject(py)?.into_any())
        }
        "record_decision_python" => {
            let row = item(0)?;
            let row = row.downcast::<crate::rules::decisions::ProposalRow>()?.get();
            let (view, proposal) = row.parts();
            let target: Option<String> = item(1)?.extract()?;
            let next: i64 = item(2)?.extract()?;
            // A plain value as it is; anything else stands for the person's row of the view.
            let observed = match crate::core::run::plain_of(&item(3)?, 0) {
                Some(value) => Observed::Value(value),
                None => Observed::Row { data: view.data(), index: proposal.index },
            };
            done(host.record_decision_python(view.data(), proposal, target.as_deref(), next, &observed)).and_then(|()| none())
        }
        // The core's `Error::Clock` as the wrapper raises it.
        "clock_error" => Err(to_py_err(CoreError::Clock)),
        _ => Err(PyValueError::new_err(format!("no host method {method:?}"))),
    }
}
