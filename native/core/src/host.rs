//! The `Host`: the world's outside, as the core's rules see it (Issue #152, lane P8-0b;
//! design-p8.md section 2, decision 2 and policies E6 and E15 of docs/rust-port/step5-plan.md).
//!
//! The rules move into the core; what is not a rule stays outside and is reached through this
//! trait:
//! - the validation of the whole world, which stays in Python in #152 (policy E6), and the views
//!   a transaction keeps in step (lane 6-2, `txn::Txn`);
//! - the pydantic contracts of inputs from a Scenario, a Branch or the LLM, whose texts reach saved
//!   data (policy E15): `contract` validates and gives back `model_dump(mode="json")`;
//! - the LLM boundary (decision 2): the social policy and the support priority;
//! - the outside sinks: metrics, decision records, trace documents and the phase observer;
//! - the Python views of the space, rebuilt after a geometry or structure change.
//!
//! The PyO3 crate's `PyHost` (`native/src/host.rs`) calls today's Python; a build without Python
//! gives its own host (the tests' `TestHost`). Rule R6 of the design holds for every call: no
//! borrow of the world is held while the host runs, a host call is made with owned values only,
//! and what a caller read before the call is read again after it, as the host may have written the
//! world (the LLM hooks write `model_state`, schedule events and take sequence numbers).

use std::sync::Arc;

use crate::error::Error;
use crate::events::EventRow;
use crate::mathx::MathError;
use crate::rules::common_operations::Logged;
use crate::rules::metrics::MetricSample;
use crate::value::V;
use crate::world::Snapshot;

pub use crate::run::sinks::{DecisionSink, Llm, TraceLevel};
pub use crate::txn::{Stage, TxnKind};

/// The class of a host's error, which a rule matches as Python's `except` does (an `except
/// ValueError` catches a pydantic `ValidationError`, a subclass).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ErrorClass {
    Value,
    Key,
    Type,
    /// `Unsupported`: the host leaves the case to Python.
    Refused,
    Other,
}

/// What a host's error says of itself.
pub trait HostFailure {
    fn class(&self) -> ErrorClass;
}

/// The core's errors by the Python class each becomes: every `EventSuppressed`,
/// `EventApplicationError`, `KernelRefusal`, `StateInvariantError` and `ResourceLedgerError` is a
/// `ValueError`.
impl HostFailure for Error {
    fn class(&self) -> ErrorClass {
        match self {
            Error::Unsupported(_) => ErrorClass::Refused,
            Error::Refused { .. }
            | Error::Outcome(..)
            | Error::MarkedValue(_)
            | Error::Value(_)
            | Error::Invariant(_)
            | Error::Ledger(_)
            | Error::Corrupt { .. }
            | Error::Math(MathError::NegativeToFraction) => ErrorClass::Value,
            Error::Key(_) => ErrorClass::Key,
            Error::Type(_) => ErrorClass::Type,
            Error::Runtime(_) | Error::Math(_) => ErrorClass::Other,
        }
    }
}

/// A pydantic contract of an input (policy E15), by the model it validates with.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Contract {
    /// `RouteAccessParameters` of `domain/geometry_overlay.py`.
    RouteAccess,
    /// `EquipmentTransferParameters` of `contracts/operations.py`.
    EquipmentTransfer,
    /// `MoveFacilityParameters` of `domain/geometry_overlay.py`.
    MoveFacility,
    /// `PartitionParameters` of `domain/geometry_overlay.py`.
    Partition,
    /// `TemporaryRoomGeometry` of `contracts/operations.py`.
    TemporaryRoom,
    /// `_PARAMETER_MODELS[operator]` of `events/effects.py`.
    EffectParameters(Arc<str>),
    /// `GuardExpression` of `events/effects.py`.
    Guard,
    /// `ScenarioZoneEnvironmentConfig` of `domain/environment.py`.
    ZoneEnvironment,
    /// `ScenarioInfrastructureSpec` of `domain/infrastructure.py`.
    Infrastructure,
    /// `PopulationSpec` of `contracts/extensions.py`.
    PopulationSpec,
    /// `DynamicArrivalPersonContext` of `contracts/population.py`.
    ArrivalContext,
    /// `PersonTraits` of `contracts/extensions.py`.
    PersonTraits,
    /// `PopulationPresenceCommand` of `domain/population_lifecycle.py`.
    PresenceCommand,
    /// `FacilityGeometry` of `contracts/extensions.py`.
    FacilityGeometry,
    /// `SpatialConstraints` of `contracts/spatial.py`.
    SpatialConstraints,
}

impl Contract {
    /// The module and the name of the model; for `EffectParameters`, of the table of models.
    pub fn model(&self) -> (&'static str, &'static str) {
        match self {
            Contract::RouteAccess => ("sheltersim.domain.geometry_overlay", "RouteAccessParameters"),
            Contract::EquipmentTransfer => ("sheltersim.contracts.operations", "EquipmentTransferParameters"),
            Contract::MoveFacility => ("sheltersim.domain.geometry_overlay", "MoveFacilityParameters"),
            Contract::Partition => ("sheltersim.domain.geometry_overlay", "PartitionParameters"),
            Contract::TemporaryRoom => ("sheltersim.contracts.operations", "TemporaryRoomGeometry"),
            Contract::EffectParameters(_) => ("sheltersim.events.effects", "_PARAMETER_MODELS"),
            Contract::Guard => ("sheltersim.events.effects", "GuardExpression"),
            Contract::ZoneEnvironment => ("sheltersim.domain.environment", "ScenarioZoneEnvironmentConfig"),
            Contract::Infrastructure => ("sheltersim.domain.infrastructure", "ScenarioInfrastructureSpec"),
            Contract::PopulationSpec => ("sheltersim.contracts.extensions", "PopulationSpec"),
            Contract::ArrivalContext => ("sheltersim.contracts.population", "DynamicArrivalPersonContext"),
            Contract::PersonTraits => ("sheltersim.contracts.extensions", "PersonTraits"),
            Contract::PresenceCommand => ("sheltersim.domain.population_lifecycle", "PopulationPresenceCommand"),
            Contract::FacilityGeometry => ("sheltersim.contracts.extensions", "FacilityGeometry"),
            Contract::SpatialConstraints => ("sheltersim.contracts.spatial", "SpatialConstraints"),
        }
    }
}

/// A trace document, as `artifacts.record_*_trace` takes it.
#[derive(Clone, Debug, PartialEq)]
pub enum Trace {
    /// `record_psychology_trace(document, agent_contributions=contributions)`.
    Psychology { document: V, contributions: V },
    /// `record_social_contact_trace(document)`.
    SocialContact(V),
    /// `record_information_spread_trace(document)`.
    InformationSpread(V),
}

/// A decision's record, as the sink of `DecisionSink` takes it. A sink that runs
/// `record_decision` in Python (`DecisionSink::Python`) takes the Python proposal, which only the
/// PyO3 crate makes.
#[derive(Clone, Debug, PartialEq)]
pub enum Decision {
    /// `artifacts.record_decision_document(document, observed_state)` of a writer whose trace is on.
    Document { document: V, observed: V },
    /// The record's JSON bytes, appended to `artifacts.observations` (`_IndexedMetricSink`).
    Indexed(Vec<u8>),
}

/// A phase of the tick (`TickPhase` of `core/engine.py`), in its order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Phase {
    DispatchDueEvents,
    CompleteScheduledTasks,
    UpdateAgentState,
    ProposeActions,
    ArbitrateActions,
    SampleEndogenousEvents,
    CollectMetrics,
}

impl Phase {
    pub const ORDER: [Phase; 7] = [
        Phase::DispatchDueEvents,
        Phase::CompleteScheduledTasks,
        Phase::UpdateAgentState,
        Phase::ProposeActions,
        Phase::ArbitrateActions,
        Phase::SampleEndogenousEvents,
        Phase::CollectMetrics,
    ];

    /// The value of the `TickPhase` member.
    pub fn name(self) -> &'static str {
        match self {
            Phase::DispatchDueEvents => "dispatch_due_events",
            Phase::CompleteScheduledTasks => "complete_scheduled_tasks",
            Phase::UpdateAgentState => "update_agent_state",
            Phase::ProposeActions => "propose_actions",
            Phase::ArbitrateActions => "arbitrate_actions",
            Phase::SampleEndogenousEvents => "sample_endogenous_events",
            Phase::CollectMetrics => "collect_metrics",
        }
    }
}

/// A part of a keyed draw's key, as `rng.random(*parts)` takes it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Part {
    Text(Arc<str>),
    Int(i64),
}

/// The text of a host that cannot draw (`Host::draw` of any host but a test's).
pub const NO_DRAW: &str = "not a CounterRNG";
/// The text of a host that cannot look up the EventLog's records (`Host::find_record`).
pub const NO_RECORDS: &str = "the host has no records";

/// The outside of the world. Each call runs with nothing of the world borrowed (rule R6).
pub trait Host {
    type Error: From<Error> + HostFailure;

    // ---- The whole world's validation and a transaction's views (policy E6, lane 6-2) ----------

    /// Validate the whole world (`WorldState.validate` in the native host) as a transaction's
    /// stage asks.
    fn validate_candidate(&mut self, kind: TxnKind, stage: Stage) -> Result<(), Self::Error>;
    /// The world was put in canonical order: the views follow it. `before` is the world as it was
    /// (the queue's old serials, by which a host keeps its objects of the events). On an error the
    /// transaction gives the snapshot back and calls `restored`.
    fn canonicalized(&mut self, _kind: TxnKind, _before: &Snapshot) -> Result<(), Self::Error> {
        Ok(())
    }
    /// The snapshot came back: the views follow it.
    fn restored(&mut self, _kind: TxnKind) -> Result<(), Self::Error> {
        Ok(())
    }
    /// The transaction kept its writes.
    fn committed(&mut self, _kind: TxnKind) -> Result<(), Self::Error> {
        Ok(())
    }
    /// `world.validate()` in place, where a rule validates the world it changed (`validate_state`).
    fn validate_world(&mut self) -> Result<(), Self::Error>;
    /// `validate_simulator_extensions(world)` of `domain/simulator_state.py`.
    fn validate_extensions(&mut self) -> Result<(), Self::Error>;
    /// `world.validate(_in_flight_event=event)`: the whole world, with this pending event in flight
    /// (lane P8 d2, the operational events). A host that validates nothing validates this as the
    /// world.
    fn validate_in_flight(&mut self, _event: &EventRow) -> Result<(), Self::Error> {
        self.validate_world()
    }

    /// The world's queue gave these slots a new row, or took them out (lane P8 d2): the host's views
    /// of their events go, to be made again from the rows. A rule calls it before a host call that
    /// reads the queue.
    fn slots_changed(&mut self, _serials: &[u64]) -> Result<(), Self::Error> {
        Ok(())
    }

    // ---- The EventLog's records (lane P8 d2) ------------------------------------------------------

    /// `event_log.find(event_id)` as the core reads a record: its status, minute and event, or None
    /// for no such record. The core's EventLog does not keep the bytes of a record once Python has
    /// taken them, so a rule that reads an older record's event (a continuity receipt's cause and the
    /// request's head) asks the host. A host without the records refuses.
    fn find_record(&mut self, _event_id: &str) -> Result<Option<Logged>, Self::Error> {
        Err(Error::unsupported(NO_RECORDS).into())
    }

    // ---- Input contracts (policy E15) ------------------------------------------------------------

    /// `Model.model_validate(input).model_dump(mode="json")`; the contract's error as it is.
    fn contract(&mut self, which: &Contract, input: &V) -> Result<V, Self::Error>;

    // ---- The LLM boundary (decision 2) -------------------------------------------------------

    /// Whether the Run has an LLM router and a social policy (read once per Run).
    fn llm(&self) -> Result<Llm, Self::Error>;
    /// `controller._prioritize_support_request(request_id, agent_id, observed_state_hash)`.
    fn prioritize_support_request(&mut self, request_id: &str, agent_id: &str, observed_state_hash: &str) -> Result<(), Self::Error>;
    /// `social_policy.consider_support_request(world, agent_id=, request_id=)`.
    fn consider_support_request(&mut self, agent_id: &str, request_id: &str) -> Result<(), Self::Error>;
    /// `social_policy.consider_complaint_response(world, agent_id=, request_id=)`.
    fn consider_complaint_response(&mut self, agent_id: &str, request_id: &str) -> Result<(), Self::Error>;
    /// `social_policy.consider_cooperation(world, initiator_agent_id=, participant_agent_id=,
    /// trigger_event_id=)`.
    fn consider_cooperation(&mut self, initiator: &str, participant: &str, trigger_event_id: &str) -> Result<(), Self::Error>;
    /// `social_policy.consider_information_reaction(world, agent_id=, participant_agent_id=,
    /// message_id=, trigger_event_id=)`.
    fn consider_information_reaction(
        &mut self,
        agent_id: &str,
        participant: &str,
        message_id: &str,
        trigger_event_id: &str,
    ) -> Result<(), Self::Error>;

    // ---- Outside sinks ---------------------------------------------------------------------------

    /// The minute's metrics: a `MetricSnapshot` of the sample for a `MetricsRecorder`, or
    /// `artifacts.capture_metric(world, minute=)` for any other sink.
    fn capture_metric(&mut self, sample: &MetricSample, minute: i64) -> Result<(), Self::Error>;
    /// Where the decisions go (read once per Run).
    fn decision_sink(&self) -> Result<DecisionSink, Self::Error>;
    /// A decision's record.
    fn decision(&mut self, decision: Decision) -> Result<(), Self::Error>;
    /// The `source_id` an indexed sink writes for a policy version (`policy_sources.get(version)`,
    /// None for None), or None when the sink takes this record only through its own
    /// `record_decision`: its `world` is another, or what it keeps for the version is not a str.
    fn decision_source(&mut self, _policy_version: &str) -> Result<Option<Option<Arc<str>>>, Self::Error> {
        Ok(Some(None))
    }
    /// The trace's level (read once per Run).
    fn trace_level(&self) -> Result<TraceLevel, Self::Error>;
    /// A trace document.
    fn trace(&mut self, document: Trace) -> Result<(), Self::Error>;
    /// `TickEngine.phase_observer(phase, before)`, if any.
    fn phase(&mut self, phase: Phase, before: bool) -> Result<(), Self::Error>;
    /// The space changed (a geometry or structure event applied): the Python views of it follow.
    fn space_refreshed(&mut self) -> Result<(), Self::Error>;

    // ---- An older state (policy E11) ---------------------------------------------------------------

    /// `upgrade_information_runtime_state(model_state["information_spread_v1"])` of a state whose
    /// schema is not 2, and the namespace's row read again from the tree: what an information event
    /// does first, after the model's checks, to a state its row does not hold as schema 2. Only a
    /// test writes such a state in memory; a save of it is view-only (`LEGACY_SHAPES_TICKABLE`).
    /// Its `ValueError` is the event's failure. A host without the Python rule refuses.
    fn upgrade_information_state(&mut self) -> Result<(), Self::Error> {
        Err(Error::unsupported(crate::rules::information_events::OLDER_STATE).into())
    }

    // ---- A test's hook ---------------------------------------------------------------------------

    /// `rng.random(*parts)` of a random source that is not a `CounterRNG`: only a test's.
    fn draw(&mut self, _parts: &[Part]) -> Result<f64, Self::Error> {
        Err(Error::unsupported(NO_DRAW).into())
    }
}
