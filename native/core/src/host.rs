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

use indexmap::IndexMap;

use crate::config::{Configs, Filled, SectionId};
use crate::error::Error;
use crate::events::writer::WriteError;
use crate::events::{EventRow, EventStatus};
use crate::geometry::Geo;
use crate::mathx::MathError;
use crate::rules::behavior::Proposal;
use crate::rules::common_operations::Logged;
use crate::rules::decisions::Observed;
use crate::rules::kit::Held;
use crate::rules::metrics::MetricSample;
use crate::rules::world_view::ViewData;
use crate::value::V;
use crate::row::ValueRow;
use crate::world::{Place, Snapshot, World};

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
    /// `str(exc)`, for a rule that wraps the error's text in its own (`except ValueError as exc:
    /// raise EventApplicationError(f"...{exc}")`, lane P8 d3).
    fn text(&self) -> String;
    /// Whether it is an `EventSuppressed` or an `EventApplicationError`, which a rule lets through
    /// before it wraps a ValueError (`except (EventApplicationError, EventSuppressed): raise`).
    fn is_outcome(&self) -> bool {
        false
    }
    /// Which outcome it is, if it is an `EventSuppressed` (`Suppressed`) or an
    /// `EventApplicationError` (`Failed`), as `dispatch_one`'s `except` clauses ask in that order
    /// (lane P9-1c).
    fn outcome(&self) -> Option<crate::error::Outcome> {
        None
    }
    /// The error as Rust's `Display` of a Python exception writes it, `{type}: {str(exc)}`, for a
    /// text that names what a read raised (`the plan read: ...` of the population rules, lane
    /// P9-3a). A host without Python writes its own text.
    fn shown(&self) -> String {
        self.text()
    }
    /// Whether the rules made it with their own text (an outcome or a marked `ValueError`), which a
    /// plan's read lets through as it is (`errors::made_here` of the PyO3 crate, lane P9-3a).
    fn made_here(&self) -> bool {
        self.is_outcome()
    }
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
            | Error::Validation(_)
            | Error::Invariant(_)
            | Error::Ledger(_)
            | Error::Corrupt { .. }
            | Error::Math(MathError::NegativeToFraction) => ErrorClass::Value,
            Error::Key(_) => ErrorClass::Key,
            Error::Type(_) => ErrorClass::Type,
            // `TickExecutionError` is a RuntimeError.
            Error::Runtime(_) | Error::Math(_) | Error::Clock => ErrorClass::Other,
        }
    }

    /// The text of the Python error it becomes. A `KeyError`'s `str` quotes its text; this gives it
    /// bare, as `Display` does.
    fn text(&self) -> String {
        self.to_string()
    }

    fn is_outcome(&self) -> bool {
        matches!(self, Error::Outcome(..))
    }

    fn outcome(&self) -> Option<crate::error::Outcome> {
        match self {
            Error::Outcome(outcome, _) => Some(*outcome),
            _ => None,
        }
    }

    fn made_here(&self) -> bool {
        matches!(self, Error::Outcome(..) | Error::MarkedValue(_))
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

// ---- What the world holds (lane P9-0, H1 of design-p9.md) -----------------------------------------

/// The refusal of a `model_state` that is not a dict the kernels read (`namespace_held`).
pub const TABLE_MAPPING: &str = "model_state is another kind of mapping";
/// The refusal of the same table where a rule asks whether a namespace is absent
/// (`namespace_is_absent`).
pub const TABLE_NOT_PLAIN: &str = "a value is not a plain dict";
/// The refusal of a key of `model_state` no slot owns.
pub const NOT_OWNED: &str = "the namespace is not owned";

/// What `model_state` holds under a key, against the key's slot.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Hold {
    /// The slot's tree, or the placeholder of the detached slot.
    Held,
    /// Another value.
    Other,
    /// A key no slot owns.
    NotOwned,
}

/// A key of `model_state`: what it holds, whether the value is None, and whether it is a dict
/// (`isinstance(value, dict)`; the slot's tree is one, lane P9-1a).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Key {
    pub hold: Hold,
    pub none: bool,
    pub dict: bool,
}

impl Key {
    /// A key whose value is a dict where it holds the slot's tree.
    pub fn new(hold: Hold, none: bool) -> Self {
        Self { hold, none, dict: hold == Hold::Held && !none }
    }

    /// A key of a value that is a dict or not (lane P9-1a).
    pub fn with_dict(hold: Hold, none: bool, dict: bool) -> Self {
        Self { hold, none, dict }
    }
}

/// Whether the world holds each namespace and owned field: what the PyO3 crate's
/// `namespace_held`, `namespace_is_absent` and `field_held` read at each call, read once
/// (`Host::holding`). A rule says its own refusal for a namespace the world holds another value
/// of (`held`).
///
/// It is good until the next host call that runs Python (an LLM hook, `space_refreshed`, a foreign
/// handler: the native host then reads it again), or a write of the core that adds or takes away
/// a key of `model_state`, after which a rule asks for it again.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Holding {
    /// Whether `model_state` is a dict the kernels read.
    pub table: bool,
    /// The keys of `model_state`, in its order, and what each holds.
    pub keys: IndexMap<Arc<str>, Key>,
    /// The owned fields (`world.metric_state` and the others) and whether each holds its slot's
    /// tree. A field not listed is held (`field_held` of a name that is not owned).
    pub fields: IndexMap<Arc<str>, bool>,
}

impl Default for Holding {
    fn default() -> Self {
        Self { table: true, keys: IndexMap::new(), fields: IndexMap::new() }
    }
}

impl Holding {
    /// What a world that no Python view holds holds: every slot of a namespace with a row (or a
    /// taint, whose tree stays) is held, every other is absent, and every owned field is held. A
    /// host without Python answers `holding` with this (the core's `kit::holding`).
    pub fn of_world(world: &World) -> crate::error::Result<Self> {
        let mut holding = Self::default();
        for name in world.slots.names() {
            let slot = world.slots.get(name).expect("a slot of its own name").try_borrow()?;
            match slot.place {
                Place::Field => {
                    holding.fields.insert(name.clone(), true);
                }
                Place::Namespace | Place::Value => {
                    if slot.row.is_some() || slot.taint.is_some() {
                        let none = matches!(slot.row.as_deref().and_then(|row| row.as_any().downcast_ref::<ValueRow>()), Some(ValueRow(V::Null)));
                        holding.keys.insert(name.clone(), Key::new(Hold::Held, none));
                    }
                }
            }
        }
        Ok(holding)
    }

    /// `namespace_held(world, name, other)`: absent, held, or the refusal `other` of a namespace
    /// the world holds another value of (or the table's or the slot's own).
    pub fn held(&self, name: &str, other: &'static str) -> Held {
        if !self.table {
            return Held::Other(TABLE_MAPPING);
        }
        match self.keys.get(name).map(|key| key.hold) {
            None => Held::Absent,
            Some(Hold::Held) => Held::Held,
            Some(Hold::Other) => Held::Other(other),
            Some(Hold::NotOwned) => Held::Other(NOT_OWNED),
        }
    }

    /// `namespace_is_absent(world, name)`: the key is missing or None. The refusal of a table
    /// that is not a plain dict.
    pub fn is_absent(&self, name: &str) -> Result<bool, &'static str> {
        if !self.table {
            return Err(TABLE_NOT_PLAIN);
        }
        Ok(self.keys.get(name).is_none_or(|key| key.none))
    }

    /// `held`, where a value that is not a dict is no state (`Absent`): the read of the information
    /// and staff operations namespaces (lane P9-1a).
    pub fn held_dict(&self, name: &str, other: &'static str) -> Held {
        match self.held(name, other) {
            Held::Other(_) if self.table && !self.keys.get(name).is_some_and(|key| key.dict) => Held::Absent,
            held => held,
        }
    }

    /// Whether `model_state` has the key, whatever its value (`name in model_state`).
    pub fn contains(&self, name: &str) -> bool {
        self.keys.contains_key(name)
    }

    /// `field_held(world, name)`: whether `world.<name>` is its slot's tree.
    pub fn field(&self, name: &str) -> bool {
        self.fields.get(name).copied().unwrap_or(true)
    }
}

// ---- A handler the core does not know (lane P9-0, H4 of design-p9.md) ------------------------------

/// A record a handler gave back for the dispatcher to append (`EventLogRecord`).
#[derive(Clone, Debug, PartialEq)]
pub struct ForeignRecord {
    pub event: EventRow,
    pub realized_minute: i64,
    pub status: EventStatus,
    pub detail: Option<String>,
}

impl ForeignRecord {
    /// The record as `event_log.find` reads it.
    pub fn logged(&self) -> Logged {
        Logged { status: self.status, realized_minute: self.realized_minute, event: self.event.clone() }
    }
}

/// What a foreign handler's `apply` did: its status, `str(exc)` of an `EventSuppressed` or
/// `EventApplicationError`, and the records it gave back, which the core settles, compares and takes
/// in as its own handlers'.
#[derive(Clone, Debug, PartialEq)]
pub struct Foreign {
    pub status: EventStatus,
    pub detail: Option<String>,
    pub records: Vec<ForeignRecord>,
}

/// The text of a host that runs no foreign handler (`Host::apply_foreign`).
pub const NO_FOREIGN: &str = "the host runs no foreign handler";
/// The text of a host that fills no section of the configs (`Host::fill_section`).
pub const NO_SECTIONS: &str = "the host fills no section of the configs";
/// The text of a host without a decision sink that runs Python (`Host::record_decision_python`).
pub const NO_PYTHON_SINK: &str = "the host has no sink that records in Python";

/// The text of a host that cannot draw (`Host::draw` of any host but a test's).
pub const NO_DRAW: &str = "not a CounterRNG";
/// The text of a host that cannot look up the EventLog's records (`Host::find_record`).
pub const NO_RECORDS: &str = "the host has no records";
/// The text of a host that cannot read the floor plan (`Host::floor_plan`).
pub const NO_PLAN: &str = "the host has no floor plan";
/// The text of a host whose sink samples no metrics itself (`Host::capture_by_sink`).
pub const NO_SINK: &str = "the host has no sink that samples";

// ---- What a tick's phases 2-6 ask of the host (lane P9-2a) ----------------------------------------

/// A read of Python to make again, for the error it raised (policy E14).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Again<'a> {
    /// The section's parse, as its read site parses it.
    Section(SectionId),
    /// The controller's attributes along this path (`controller.psychology.source_id`).
    Policy(&'a [&'a str]),
}

/// Where a minute's metrics go (`Host::metric_sink`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MetricSink {
    /// A `MetricsRecorder` the sink holds, whose `capture` is the sink's: it takes the core's sample.
    Recorder,
    /// Any other sink: `artifacts.capture_metric(world, minute=)` samples on its own.
    Sink,
}

/// The outside of the world. Each call runs with nothing of the world borrowed (rule R6).
pub trait Host {
    /// A write's error is the host's as `From<WriteError>` makes it: the native host's
    /// `DuplicateEventError` for a duplicate (lane P9-3b; P9-2a's `write_error`).
    type Error: From<Error> + From<WriteError> + HostFailure;

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
    /// Whether this host's views of the world are all ones a transaction gives back, asked before a
    /// rule's transaction opens (`txn::rule`, lane P9-1b): Unsupported with the reason otherwise
    /// (the native host's `_txn_check` of `core/state_mirror.py`: a tainted entity store, a map of
    /// entities that is not the rows', a queue held in Python, ...), with nothing changed. A host
    /// with no views of its own has nothing to refuse.
    fn txn_check(&mut self) -> Result<(), Self::Error> {
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

    /// The canonical JSON of the log's record at each of these positions, given to `each`, for a
    /// reader of records whose bytes the core's log no longer keeps (Python took them): a frame
    /// and its digest (#264). A host without Python has every record in the core's log, and
    /// refuses.
    fn read_records(&mut self, _positions: &[usize], _each: &mut dyn FnMut(usize, &[u8]) -> crate::error::Result<()>) -> Result<(), Self::Error> {
        Err(Error::unsupported(NO_RECORDS).into())
    }

    // ---- The floor plan (lane P8 d3) ----------------------------------------------------------------

    /// The floor plan of the world now (`Geo::of`), for a rule that read it, changed what it is
    /// built from (the overlay's facility points) and reads it again: each toilet an EffectProgram
    /// adds is placed on the plan the toilets before it left. A host without the configs' sections
    /// refuses.
    fn floor_plan(&mut self) -> Result<Arc<Geo>, Self::Error> {
        Err(Error::unsupported(NO_PLAN).into())
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

    // ---- What the world holds and its configs (lane P9-0) ------------------------------------------

    /// Whether the world holds each namespace and owned field (H1). None: no Python view holds the
    /// world, so its slots answer (`Holding::of_world`, which `kit::holding` reads). The native host
    /// reads `model_state` and the fields against their mirrors, and keeps what it read until a
    /// call of its own runs Python.
    fn holding(&mut self) -> Result<Option<Arc<Holding>>, Self::Error> {
        Ok(None)
    }

    /// Whether `model_state[name]` is a dict, for a key the holding says holds another value than
    /// its slot's tree (lane P9-1b): a rule that reads a state with `isinstance(state, dict)` takes
    /// one that is not as no state (`kit::staff_held`). A host without Python views has no such key.
    fn other_is_dict(&mut self, _name: &str) -> Result<bool, Self::Error> {
        Ok(true)
    }

    /// Fill the section `which` of `configs`, the configs of the world now, which a rule found
    /// empty (H2). The native host runs today's parse at the same first read: the Python contract,
    /// or the core's parser of the configs read from Python where no row holds them. A parse never
    /// raises here; what it raised is the section's fault, which the read site raises. A host
    /// without the configs' parsers refuses.
    fn fill_section(&mut self, _configs: &Arc<Configs>, _which: SectionId) -> Result<Filled, Self::Error> {
        Err(Error::unsupported(NO_SECTIONS).into())
    }

    // ---- The dispatcher's handlers (lane P9-0, H4) ---------------------------------------------------

    /// `handler.apply(world, event)` of a handler the core does not run: one a test registered, or
    /// a class whose `apply` was patched (a test's hook, design-p9.md H4). The core has run the
    /// guard; it settles the event with what this gives back.
    fn apply_foreign(&mut self, _event: &EventRow) -> Result<Foreign, Self::Error> {
        Err(Error::unsupported(NO_FOREIGN).into())
    }

    // ---- A decision a Python sink records (lane P9-0, H5) -------------------------------------------

    /// `artifacts.record_decision(proposal.with_outcome(target_id, next_decision_at),
    /// observed_state=observed)`, for `DecisionSink::Python`. The core commits and remembers the
    /// decision itself after it.
    fn record_decision_python(
        &mut self,
        _data: &ViewData,
        _proposal: &Proposal,
        _target_id: Option<&str>,
        _next_decision_at: i64,
        _observed: &Observed<'_>,
    ) -> Result<(), Self::Error> {
        Err(Error::unsupported(NO_PYTHON_SINK).into())
    }

    // ---- An older state (policy E11) ---------------------------------------------------------------

    /// `upgrade_information_runtime_state(model_state["information_spread_v1"])` of a state whose
    /// schema is not 2, and the namespace's row read again from the tree: what an information event
    /// does first, after the model's checks, to a state its row does not hold as schema 2. Only a
    /// test writes such a state in memory; a save of it is view-only (`LEGACY_SHAPES_TICKABLE`).
    /// Its `ValueError` is the event's failure. A host without the Python rule refuses.
    fn upgrade_information_state(&mut self) -> Result<(), Self::Error> {
        Err(Error::unsupported(crate::rules::information_events::OLDER_STATE).into())
    }

    // ---- A tick's phases 2-6 in the core (lane P9-2a) ------------------------------------------------

    /// The rows of the pending events Python scheduled, read before a rule reads the whole queue
    /// (H6 of design-p9.md). A host without Python has nothing to read.
    fn read_queue_rows(&mut self) -> Result<(), Self::Error> {
        Ok(())
    }

    /// `world.model_configs` as a value while it is a dict the world's slot does not hold (assigned
    /// whole), for a rule that reads the configs past their sections; None while the slot holds it.
    fn configs_given(&mut self) -> Result<Option<V>, Self::Error> {
        Ok(None)
    }

    /// Make a read of Python again whose fault a rule raises (policy E14): its error, or Ok where
    /// it raises nothing now. A host without Python has nothing to read again.
    fn read_again(&mut self, _again: Again<'_>) -> Result<(), Self::Error> {
        Ok(())
    }

    /// The core wrote `remaining` for `RESOURCE_IDS[index]` in `world.resources` (an operation of
    /// the ledger, `World::set_resource`; Issue #244, decision 2). A notice: a host without Python
    /// views has nothing to do. The native host gives the Python field a new dict, as the Python rule
    /// did.
    fn resources_changed(&mut self, _index: usize, _remaining: i64) -> Result<(), Self::Error> {
        Ok(())
    }

    /// The core started the namespace `name` of `model_state` with `value` (`World::start_namespace`:
    /// the social model's first tick, the population's dynamics), before the rule writes its row
    /// (Issue #244, decision 2). A notice: a host without Python views has nothing to do. The native
    /// host stores the value in the Python `model_state`. What the world holds changes, so a rule
    /// asks for the holding again after it.
    fn start_namespace(&mut self, _name: &str, _value: &V) -> Result<(), Self::Error> {
        Ok(())
    }

    /// Where this minute's metrics go, read at each tick (a test may put its own `capture_metric`
    /// on the sink).
    fn metric_sink(&mut self) -> Result<MetricSink, Self::Error> {
        Ok(MetricSink::Recorder)
    }

    /// `artifacts.capture_metric(world, minute=)` of a sink that samples on its own.
    fn capture_by_sink(&mut self, _minute: i64) -> Result<(), Self::Error> {
        Err(Error::unsupported(NO_SINK).into())
    }

    // ---- A test's hook ---------------------------------------------------------------------------

    /// `rng.random(*parts)` of a random source that is not a `CounterRNG`: only a test's.
    fn draw(&mut self, _parts: &[Part]) -> Result<f64, Self::Error> {
        Err(Error::unsupported(NO_DRAW).into())
    }
}
