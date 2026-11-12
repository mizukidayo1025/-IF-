//! The dispatch phase in the core (Issue #152, lane P9-1c; design-p9.md section 1.1 and the P9-1c
//! row of section 5): what is due at the tick's minute, in queue order, and then what follows from
//! the events that applied, as `EventDispatcher.dispatch_one` and `DispatchPhase` did.
//!
//! For each event of the due snapshot:
//! 1. where it stands now: gone from the queue (skipped), the same row, or another row of its ID,
//!    which runs only if it is the same event by value (`current is event or current.to_dict() ==
//!    event.to_dict()`: numbers by value, `1 == 1.0`, `-0.0 == 0.0`, maps in any order);
//! 2. its handler (`Policies.dispatch`, read by the PyO3 crate and checked by identity at each
//!    phase): none is `FAILED` with Python's text; a known class runs here (`HandlerKind`), and any
//!    other is the host's (`Host::apply_foreign`, H4), as is a known class whose inputs the PyO3
//!    crate still reads (the population's handler, a relocation's walk, an intake's completion);
//! 3. the EffectProgram's guard first, for an event with its marker;
//! 4. the record of a contact or an interaction (`record_social_event`, `observe_event`);
//! 5. the settle: the event leaves the queue and its record is appended, or, where the handler
//!    wrote its own, the two are compared through `Host::find_record` (Python's `ValueError`);
//! 6. the handler's follow-up records, compared or appended, and the shared receipts among them.
//!
//! Then, for the events that applied: the actions they end, a population exit, what a walk taught
//! and a staff trip's start, a timeout's interruption; the space refreshed once
//! (`Host::space_refreshed`); the trips a closed route stops; the returns admitted; the journeys of
//! people who stopped finished; the arrivals admitted; and the social policy's contact gate
//! (`consider_cooperation`, `consider_information_reaction`, decision D5).
//!
//! An event's expected outcome is its record's status and `str(exc)`. A refusal of a handler's
//! kernel is the `KernelRefusal` of the handler's `kernel_only` entry (`Error::Refused`), as the
//! Python dispatcher raised it; any other error ends the phase as it is. No borrow of the world,
//! and no lock of the Run, is held while the host runs (rule R6).

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use crate::config::movement::Movement;
use crate::config::population::AccessibilityNeeds;
use crate::config::SectionId;
use crate::error::{Error, Outcome};
use crate::events::writer::WriteError;
use crate::events::{EventRow, EventStatus};
use crate::host::{ErrorClass, Foreign, Host, HostFailure};
use crate::rules::care_receipts::{Made, PlannedTransition};
use crate::rules::kit::{self, Counts, Held};
use crate::rules::tick::read::is_refusal;
use crate::rules::tick::{RunCell, Tick};
use crate::rules::{
    action_lifecycle, agent_dynamics, body_state, common_operations, effects, handlers, information_events, movement, population,
    population_lifecycle, service, social_interaction, social_network, staff_operation_events as operations, support, toilet,
    travel_completion,
};
use crate::run::dispatch::{AfterComplete, Handler, HandlerKind, Native};
use crate::text::py_repr;
use crate::value::V;
use crate::world::WorldCell;

// ---- What ran ---------------------------------------------------------------------------------------

macro_rules! counters {
    ($($name:ident),* $(,)?) => {
        /// What the phase did in this process, for the gate and the tests (`stats` of the PyO3
        /// crate's `dispatch`).
        pub struct Counters { $(pub $name: AtomicU64),* }

        /// The counts of the phase.
        pub static COUNTS: Counters = Counters { $($name: AtomicU64::new(0)),* };

        impl Counters {
            /// `(name, times)` of every counter, in their order.
            pub fn all(&self) -> Vec<(&'static str, u64)> {
                vec![$((stringify!($name), self.$name.load(Ordering::Relaxed))),*]
            }
        }
    };
}

counters!(
    phases_ran,
    events_core,
    events_foreign,
    events_unregistered,
    events_skipped,
    events_replaced,
    replaced_dispatched,
    replaced_skipped,
    guard_passed,
    guard_stopped,
    outcomes_applied,
    outcomes_suppressed,
    outcomes_failed,
    settled_rows,
    settled_own,
    follow_ups,
    record_social_ran,
    observe_ran,
    completion_ran,
    population_exit_ran,
    learn_ran,
    started_ran,
    interrupted_ran,
    spaces_refreshed,
    trips_interrupted,
    returns_asked,
    sync_ran,
    journeys_finished,
    beds_placed,
    arrivals_asked,
    contacts_considered,
    cooperation_asked,
    reaction_asked,
);

/// The handlers' kernels that ran and what they refused, by the label of their `kernel_only` entry.
pub static KINDS: Counts = Counts::new();

/// A core error as the host's.
fn core_err<E: From<Error>>(error: Error) -> E {
    error.into()
}

fn count(counter: &AtomicU64) {
    counter.fetch_add(1, Ordering::Relaxed);
}

// ---- Texts ------------------------------------------------------------------------------------------

/// The refusal of a due event its row cannot hold (H6 of design-p9.md: the PyO3 crate reads the rows
/// of the events Python scheduled before the phase).
pub const NO_ROW: &str = "an event a row cannot hold";
/// `ValueError` of `EventDispatcher.settle`.
pub const RECORD_DISAGREES: &str = "transactional event record disagrees with its dispatch result";
/// `ValueError` of `dispatch_one` for a follow-up record.
pub const FOLLOW_UP_DISAGREES: &str = "transactional follow-up record disagrees with dispatch result";
/// The label of the phase's own refusals (`errors::kernel_refusal(py, "dispatch.run_phase", ...)`).
pub const RUN_PHASE: &str = "dispatch.run_phase";
/// `_sync_movement_after_events`, a body-less `kernel_only` step.
pub const SYNC: &str = "dispatch.sync_movement";
/// The label of the guard's refusal (`should_apply_effect_guard`).
pub const GUARD: &str = "effects.should_apply_guard";
/// The contact gate, a body-less `kernel_only` step (`_consider_social_contact_event`).
pub const CONTACT: &str = "dispatch.consider_contact";

const MOVEMENT: &str = "movement_v1";
const SOCIAL_NETWORK: &str = "social_network_v1";
const OVERLAY: &str = "geometry_overlay_v1";

/// The `kernel_only` entry of a handler's `apply`, whose `KernelRefusal` a refusal is.
fn label_of(kind: HandlerKind) -> &'static str {
    match kind {
        HandlerKind::SocialInteraction => "social_interaction.apply",
        HandlerKind::AgentTravelCompletion => "travel_completion.apply",
        HandlerKind::ToiletServiceCompletion => "toilet.complete",
        HandlerKind::ResourceServiceCompletion => "service.complete",
        HandlerKind::SupportRequestTimeout => "support.timeout",
        HandlerKind::StaffSupportCompletion => "support.complete_support",
        HandlerKind::ProximityContact => "handlers.proximity",
        HandlerKind::InformationRumor => "information_events.rumor",
        HandlerKind::InformationCorrection => "information_events.correction",
        HandlerKind::InformationContact => "information_events.contact",
        HandlerKind::ResourceLedger => "handlers.resource_ledger",
        HandlerKind::FacilityOutage => "handlers.facility_outage",
        HandlerKind::FacilityRecovery => "handlers.facility_recovery",
        HandlerKind::InfrastructureEvent => "handlers.infrastructure",
        HandlerKind::ResourceServiceCancellation => "handlers.service_cancellation",
        HandlerKind::EffectProgramPopulation => "handlers.population",
        HandlerKind::DeliveryDelay => "handlers.delivery_delay",
        HandlerKind::WaterOutage => "handlers.water_outage",
        HandlerKind::WaterRestoration => "handlers.water_restoration",
        HandlerKind::StaffUnavailability => "handlers.staff_unavailable",
        HandlerKind::StaffRestoration => "handlers.staff_restoration",
        HandlerKind::StaffOperationArrival => "staff_operation_events.arrive",
        HandlerKind::StaffOperationRelief => "staff_operation_events.relief",
        HandlerKind::StaffOperationCompletion => "staff_operation_events.complete",
        HandlerKind::EffectProgramFacility => "effects.facility",
        HandlerKind::EffectProgramEnvironment => "effects.environment",
        HandlerKind::EffectProgramTemporarySpace => "effects.temporary_space",
        HandlerKind::EffectProgramGeometry => "effects.geometry",
        HandlerKind::EffectProgramStructure => "effects.structure",
    }
}

/// A refusal as the `KernelRefusal` of `label`; any other error as it is.
fn labelled<E: HostFailure + From<Error>>(label: &'static str, error: E) -> E {
    crate::rules::tick::propose::refused(label, error)
}

/// A host error as an event's outcome, if it is one: its status and `str(exc)`.
fn outcome_of<E: HostFailure>(error: &E) -> Option<(EventStatus, String)> {
    match error.outcome()? {
        Outcome::Suppressed => Some((EventStatus::Suppressed, error.text())),
        Outcome::Failed => Some((EventStatus::Failed, error.text())),
    }
}

/// Count a kernel's result under its label in `counts`: a refusal by its reason.
fn counted<T, E: HostFailure>(counts: &Counts, label: &'static str, result: std::result::Result<T, E>) -> std::result::Result<T, E> {
    match &result {
        Ok(_) => counts.ran(label),
        Err(error) => {
            if let Some(reason) = is_refusal(error) {
                counts.refused(label, &reason);
            }
        }
    }
    result
}

// ---- Equality by value ------------------------------------------------------------------------------

/// Python's `==` of two values, as `to_dict()` gives them: `common_operations::py_eq`, with an int
/// past 64 bits equal to the float of its value.
pub fn py_eq(first: &V, second: &V) -> bool {
    match (first, second) {
        (V::Big(big), V::Float(float)) | (V::Float(float), V::Big(big)) => {
            float.is_finite() && float.fract() == 0.0 && format!("{float:.0}") == **big
        }
        (V::List(one), V::List(two)) => one.len() == two.len() && one.iter().zip(two).all(|(a, b)| py_eq(a, b)),
        (V::Map(one), V::Map(two)) => one.len() == two.len() && one.iter().all(|(key, value)| two.get(key).is_some_and(|other| py_eq(value, other))),
        _ => common_operations::py_eq(first, second),
    }
}

/// `first.to_dict() == second.to_dict()` of two rows of events.
pub fn same_event(first: &EventRow, second: &EventRow) -> bool {
    first.event_id == second.event_id
        && first.kind == second.kind
        && first.due_minute == second.due_minute
        && first.priority == second.priority
        && first.sequence == second.sequence
        && first.event_type == second.event_type
        && first.source_id == second.source_id
        && first.causation_id == second.causation_id
        && py_eq(&first.payload, &second.payload)
        && py_eq(&first.provenance, &second.provenance)
}

// ---- The records of an event --------------------------------------------------------------------------

/// A record a handler gave the dispatcher to append (`EventLogRecord`).
#[derive(Clone, Debug, PartialEq)]
pub struct Record {
    pub event: EventRow,
    pub realized_minute: i64,
    pub status: EventStatus,
    pub detail: Option<String>,
}

impl From<Made> for Record {
    fn from(made: Made) -> Self {
        Record { event: made.event, realized_minute: made.minute, status: EventStatus::Applied, detail: Some(made.detail) }
    }
}

impl From<handlers::Followed> for Record {
    fn from(followed: handlers::Followed) -> Self {
        Record { event: followed.event, realized_minute: followed.minute, status: EventStatus::Applied, detail: followed.detail }
    }
}

impl From<crate::host::ForeignRecord> for Record {
    fn from(record: crate::host::ForeignRecord) -> Self {
        Record { event: record.event, realized_minute: record.realized_minute, status: record.status, detail: record.detail }
    }
}

/// What a handler did: applied, with its records and its care receipt, or its outcome.
enum Handled {
    Applied { records: Vec<Record>, receipt: Option<PlannedTransition> },
    Outcome(EventStatus, String),
}

impl Handled {
    fn applied() -> Self {
        Handled::Applied { records: Vec::new(), receipt: None }
    }

    fn records(records: Vec<Record>) -> Self {
        Handled::Applied { records, receipt: None }
    }
}

/// A handler's result: an outcome error is the event's outcome; a refusal is the handler's
/// `KernelRefusal`; any other error goes on.
fn handled<E: HostFailure + From<Error>>(label: &'static str, result: std::result::Result<Handled, E>) -> std::result::Result<Handled, E> {
    match result {
        Ok(handled) => Ok(handled),
        Err(error) => match outcome_of(&error) {
            Some((status, text)) => Ok(Handled::Outcome(status, text)),
            None => Err(labelled(label, error)),
        },
    }
}

/// One record of the phase, as the steps after the loop read it.
pub struct Dispatched {
    pub row: Arc<EventRow>,
    pub applied: bool,
}

// ---- The phase -----------------------------------------------------------------------------------------

/// `_run_dispatch_phase` at the tick's minute. The caller (the PyO3 crate's `run_phase` and the tick
/// driver) checked what Python holds before the first event (policy E10): the dispatcher's class,
/// the world's stores, the minute.
pub fn run<C: WorldCell, H: Host, R: RunCell>(tick: &mut Tick<'_, C, H, R>) -> std::result::Result<(), H::Error>
where
    H::Error: From<WriteError>,
{
    count(&COUNTS.phases_ran);
    tick.host.read_queue_rows()?;
    let minute = tick.minute;
    let due = tick.world(|world| {
        let queue = world.queue()?;
        let end = queue.due_end(minute);
        queue.ordered()[..end]
            .iter()
            .map(|(_, serial)| {
                let slot = queue.slot(*serial);
                slot.row.clone().map(|row| (slot.event_id.clone(), row)).ok_or_else(|| Error::unsupported(NO_ROW))
            })
            .collect::<crate::error::Result<Vec<_>>>()
    })?;
    let mut outcomes = Vec::with_capacity(due.len());
    for (event_id, row) in due {
        one_event(tick, &event_id, row, &mut outcomes)?;
    }
    after_events(tick, &outcomes)
}

/// What the queue holds of this ID now: None when it left, else its row (None for an object no
/// row holds).
fn current<C: WorldCell, H: Host, R: RunCell>(tick: &Tick<'_, C, H, R>, event_id: &str) -> std::result::Result<Option<Option<Arc<EventRow>>>, H::Error> {
    tick.world(|world| {
        let queue = world.queue()?;
        Ok(queue.serial_of(event_id).map(|serial| queue.slot(serial).row.clone()))
    })
}

/// One event of the snapshot.
fn one_event<C: WorldCell, H: Host, R: RunCell>(
    tick: &mut Tick<'_, C, H, R>,
    event_id: &str,
    row: Arc<EventRow>,
    outcomes: &mut Vec<Dispatched>,
) -> std::result::Result<(), H::Error>
where
    H::Error: From<WriteError>,
{
    match current(tick, event_id)? {
        None => {
            count(&COUNTS.events_skipped);
            return Ok(());
        }
        Some(Some(now)) if Arc::ptr_eq(&now, &row) => {}
        Some(now) => {
            count(&COUNTS.events_replaced);
            let now = match now {
                Some(now) => now,
                // A Python handler or hook scheduled it: its row is read now.
                None => {
                    tick.host.read_queue_rows()?;
                    current(tick, event_id)?.flatten().ok_or_else(|| Error::unsupported(NO_ROW))?
                }
            };
            if !same_event(&now, &row) {
                count(&COUNTS.replaced_skipped);
                return Ok(());
            }
            count(&COUNTS.replaced_dispatched);
        }
    }
    let handler = tick.policies.dispatch.handler(&row.event_type).map_err(|fault| Error::unsupported(fault.text.to_string()))?.cloned();
    let handled = match handler {
        None => {
            count(&COUNTS.events_unregistered);
            Handled::Outcome(EventStatus::Failed, format!("no handler registered for event type {}", py_repr(&row.event_type)))
        }
        Some(handler) => match guard(tick, &row)? {
            Some(stopped) => stopped,
            None => apply(tick, &handler, &row)?,
        },
    };
    let (status, detail, records, receipt) = match handled {
        Handled::Applied { records, receipt } => (EventStatus::Applied, None, records, receipt),
        Handled::Outcome(status, text) => (status, Some(text), Vec::new(), None),
    };
    count(match status {
        EventStatus::Applied => &COUNTS.outcomes_applied,
        EventStatus::Suppressed => &COUNTS.outcomes_suppressed,
        EventStatus::Failed => &COUNTS.outcomes_failed,
    });
    if status == EventStatus::Applied && body_state::records_event(&row.event_type) {
        social_records(tick, &row)?;
    }
    settle(tick, &row, status, detail.as_deref())?;
    if let Some(receipt) = receipt {
        // `dispatch_one` appends the handler's record after the event's own, and the receipt follows.
        let written = tick.cell.with(|world| crate::rules::care_receipts::record_transition(world, receipt)).map_err(core_err::<H::Error>)?;
        tick.written(written)?;
    }
    outcomes.push(Dispatched { row: row.clone(), applied: status == EventStatus::Applied });
    follow_ups(tick, records, outcomes)
}

/// `effect_event_guard_or_suppress` of an event with an EffectProgram's marker: None when the
/// handler goes on, else the event's outcome.
fn guard<C: WorldCell, H: Host, R: RunCell>(tick: &mut Tick<'_, C, H, R>, row: &EventRow) -> std::result::Result<Option<Handled>, H::Error> {
    if kit::provenance(row, effects::MARKER).is_none() {
        return Ok(None);
    }
    let holding = tick.holding()?;
    let program = holding.held(effects::MARKER, "the world holds another effect_program_v1");
    let ledger = holding.held("resource_ledger_v1", "the world holds another ledger");
    let ins = effects::GuardIn { program: &program, ledger: &ledger };
    let result = counted(&effects::COUNTS, "dispatch_guard", effects::guard_or_suppress(tick.cell, tick.host, &ins, row));
    match result {
        Ok(()) => {
            count(&COUNTS.guard_passed);
            Ok(None)
        }
        Err(error) => match outcome_of(&error) {
            Some((status, text)) => {
                count(&COUNTS.guard_stopped);
                Ok(Some(Handled::Outcome(status, text)))
            }
            None => Err(labelled(GUARD, error)),
        },
    }
}

/// The handler's `apply`: the core's kernel of a known class, or the host's.
fn apply<C: WorldCell, H: Host, R: RunCell>(tick: &mut Tick<'_, C, H, R>, handler: &Handler, row: &EventRow) -> std::result::Result<Handled, H::Error>
where
    H::Error: From<WriteError>,
{
    let native = match handler {
        Handler::Native(native) if runs_here(native, row) => native,
        _ => return foreign(tick, row),
    };
    count(&COUNTS.events_core);
    let label = label_of(native.kind);
    let result = handled(label, apply_native(tick, native, row));
    match &result {
        Ok(_) => KINDS.ran(label),
        Err(error) => {
            if let Some(reason) = is_refusal(error) {
                KINDS.refused(label, &reason);
            }
        }
    }
    result
}

/// Whether a known handler runs here: its managers are the controller's, its `_after_arrival` and
/// `after_complete` are as defined, its walk's manager has no `request` of its own (a test's), and
/// its inputs are the core's (not the population's handler, a relocation's walk).
fn runs_here(native: &Native, row: &EventRow) -> bool {
    if !native.own_managers || !native.after_arrival_as_defined {
        return false;
    }
    match native.kind {
        HandlerKind::EffectProgramPopulation => false,
        HandlerKind::ToiletServiceCompletion => !matches!(native.after_complete(), Ok(AfterComplete::Other) | Err(_)),
        HandlerKind::AgentTravelCompletion => match kit::payload(row, "purpose") {
            Some(V::Str(purpose)) if &**purpose == "presence_relocation" => false,
            Some(V::Str(purpose)) if &**purpose == "toilet_request" => native.own_request("toilet_service") == Ok(false),
            Some(V::Str(purpose)) if &**purpose == "resource_request" => native.own_request("resource_service") == Ok(false),
            _ => true,
        },
        _ => true,
    }
}

/// `handler.apply(world, event)` of a handler the core does not run (H4): its outcome, and the records
/// it gave back.
fn foreign<C: WorldCell, H: Host, R: RunCell>(tick: &mut Tick<'_, C, H, R>, row: &EventRow) -> std::result::Result<Handled, H::Error> {
    count(&COUNTS.events_foreign);
    let Foreign { status, detail, records } = tick.host.apply_foreign(row)?;
    Ok(match status {
        EventStatus::Applied => Handled::records(records.into_iter().map(Record::from).collect()),
        status => Handled::Outcome(status, detail.unwrap_or_default()),
    })
}

/// `record_social_event` and, with `social_network_v1` in `model_state`, `observe_event` of an
/// applied contact or interaction.
fn social_records<C: WorldCell, H: Host, R: RunCell>(tick: &mut Tick<'_, C, H, R>, row: &EventRow) -> std::result::Result<(), H::Error> {
    record_social_event(tick, row).map_err(|error| labelled("body_state.record_social_event", error))?;
    count(&COUNTS.record_social_ran);
    let holding = tick.holding()?;
    if !holding.contains(SOCIAL_NETWORK) {
        return Ok(());
    }
    if social_network::observes_event(&row.event_type) {
        let keys: &[&str] = match &*row.event_type {
            "information_contact_update" => &["recipient_agent_id", "sender_agent_id", "message_id"],
            "proximity_contact_update" => &["first_agent_id", "second_agent_id", "observed_minute"],
            _ => &["interaction_type", "agent_id", "participant_agent_id"],
        };
        let payload = payload_fields(row, keys);
        let minute = tick.minute;
        tick.cell
            .with(|world| social_network::observe_event(world, &row.event_type, &payload, minute))
            .and_then(|done| done)
            .map_err(|error| labelled("social_network.observe_event", core_err::<H::Error>(error)))?;
    }
    count(&COUNTS.observe_ran);
    Ok(())
}

/// The payload's values of these keys, as `payload_fields` of the PyO3 crate reads them.
fn payload_fields(row: &EventRow, keys: &[&str]) -> V {
    let mut fields = indexmap::IndexMap::with_capacity(keys.len());
    if let V::Map(payload) = &row.payload {
        for key in keys {
            if let Some((name, value)) = payload.get_key_value(*key) {
                fields.insert(name.clone(), value.clone());
            }
        }
    }
    V::Map(fields)
}

/// `record_social_event`: the namespace and the configs' models read through the host, at the
/// points the rule read them.
fn record_social_event<C: WorldCell, H: Host, R: RunCell>(tick: &mut Tick<'_, C, H, R>, row: &EventRow) -> std::result::Result<(), H::Error> {
    if body_state::namespace_absent(tick.cell, tick.host)? {
        return Ok(());
    }
    let inputs = body_state::Inputs::read(tick.cell, tick.host)?;
    let models = match &inputs.models {
        Ok(models) => models.clone(),
        Err(fault) => return Err(tick.raise(SectionId::BodyModels, fault)),
    };
    let keys: &[&str] = match &*row.event_type {
        "proximity_contact_update" => &[
            "contact_minutes", "message_id", "topic_id", "interaction_duration_minutes", "first_agent_id", "second_agent_id", "observed_minute",
            "distance_m",
        ],
        "information_contact_update" => {
            &["contact_minutes", "message_id", "topic_id", "interaction_duration_minutes", "sender_agent_id", "recipient_agent_id"]
        }
        "social_interaction" => {
            &["contact_minutes", "message_id", "topic_id", "interaction_duration_minutes", "interaction_type", "agent_id", "participant_agent_id"]
        }
        _ => &["contact_minutes", "message_id", "topic_id", "interaction_duration_minutes", "zone_id"],
    };
    let payload = payload_fields(row, keys);
    let event = body_state::SocialEvent { event_id: &row.event_id, event_type: &row.event_type, causation_id: row.causation_id.as_deref(), payload: &payload };
    let minute = tick.minute;
    tick.world(|world| body_state::record_social_event(world, &event, minute, &inputs.selection, &models))
}

/// `EventDispatcher.settle`: the event leaves the queue if it is there, and its record is appended,
/// unless the log holds one of its ID, which the handler wrote: that one must be of the same event
/// and status.
fn settle<C: WorldCell, H: Host, R: RunCell>(tick: &mut Tick<'_, C, H, R>, row: &EventRow, status: EventStatus, detail: Option<&str>) -> std::result::Result<(), H::Error> {
    let minute = tick.minute;
    let settled = tick.cell.with(|world| -> crate::events::writer::WriteResult<(Option<u64>, bool)> {
        let taken = world.queue_mut()?.take_slot(&row.event_id).map(|(serial, _)| serial);
        if world.log()?.has_event_id(&row.event_id) {
            return Ok((taken, false));
        }
        kit::log_record(world, row, minute, status, detail)?;
        Ok((taken, true))
    });
    let (taken, recorded) = tick.written(settled.map_err(core_err::<H::Error>)?)?;
    if let Some(serial) = taken {
        tick.host.slots_changed(&[serial])?;
    }
    if recorded {
        count(&COUNTS.settled_rows);
        return Ok(());
    }
    count(&COUNTS.settled_own);
    match tick.host.find_record(&row.event_id)? {
        Some(logged) if logged.status == status && same_event(&logged.event, row) => Ok(()),
        _ => Err(Error::Value(RECORD_DISAGREES.to_owned()).into()),
    }
}

/// The handler's records: each one the log holds already must be the same event with the same status,
/// and any other is appended. The shared receipts among them are taken in
/// (`ingest_applied_shared_operation_records`).
fn follow_ups<C: WorldCell, H: Host, R: RunCell>(tick: &mut Tick<'_, C, H, R>, records: Vec<Record>, outcomes: &mut Vec<Dispatched>) -> std::result::Result<(), H::Error> {
    if records.is_empty() {
        return Ok(());
    }
    for record in &records {
        count(&COUNTS.follow_ups);
        let id = record.event.event_id.clone();
        let written = tick.cell.with(|world| -> crate::events::writer::WriteResult<bool> {
            if world.log()?.has_event_id(&id) {
                return Ok(false);
            }
            kit::log_record(world, &record.event, record.realized_minute, record.status, record.detail.as_deref())?;
            Ok(true)
        });
        let appended = tick.written(written.map_err(core_err::<H::Error>)?)?;
        if !appended {
            match tick.host.find_record(&id)? {
                Some(logged) if logged.status == record.status && same_event(&logged.event, &record.event) => {}
                _ => return Err(Error::Value(FOLLOW_UP_DISAGREES.to_owned()).into()),
            }
        }
        outcomes.push(Dispatched { row: Arc::new(record.event.clone()), applied: record.status == EventStatus::Applied });
    }
    ingest(tick, &records)
}

/// `ingest_applied_shared_operation_records` of the handler's records: nothing without the
/// namespace; each APPLIED receipt not recorded yet goes to `record_shared_operation`. Only the
/// first can be refused: nothing has changed before it.
fn ingest<C: WorldCell, H: Host, R: RunCell>(tick: &mut Tick<'_, C, H, R>, records: &[Record]) -> std::result::Result<(), H::Error> {
    let holding = tick.holding()?;
    if !crate::rules::care_receipts::has_namespace(&holding)? {
        return Ok(());
    }
    let mut first = true;
    for record in records {
        if record.status != EventStatus::Applied || !common_operations::RECEIPT_EVENTS.contains(&&*record.event.event_type) {
            continue;
        }
        let id = record.event.event_id.clone();
        let realized = record.realized_minute;
        let recorded = tick.cell.with(|world| -> crate::error::Result<bool> {
            let minute = world.minute_index()?;
            common_operations::is_recorded(world, &id, realized == minute, minute)
        });
        let recorded = match recorded.and_then(|recorded| recorded) {
            Err(Error::Unsupported(reason)) if !first => return Err(Error::Runtime(format!("the shared operations left their row: {reason}")).into()),
            other => other.map_err(core_err::<H::Error>)?,
        };
        if recorded {
            continue;
        }
        let receipt = common_operations::Receipt::of(&record.event.payload)?;
        common_operations::record_through(tick.cell, tick.host, &receipt)?;
        first = false;
    }
    Ok(())
}

// ---- The handlers -----------------------------------------------------------------------------------

/// The payload's value of `key` when it is a str (`EventRef::text` of a row).
fn text_of(row: &EventRow, key: &str) -> Option<Arc<str>> {
    match kit::payload(row, key) {
        Some(V::Str(text)) => Some(text.clone()),
        _ => None,
    }
}

/// The event's IDs as the support rules read them (`event_of` of the support glue): the payload's
/// str of each key asked.
fn support_event(row: &EventRow, keys: [bool; 4]) -> support::SupportEvent {
    let text = |wanted: bool, key: &str| if wanted { text_of(row, key) } else { None };
    support::SupportEvent {
        request_id: text(keys[0], "request_id"),
        agent_id: text(keys[1], "agent_id"),
        staff_id: text(keys[2], "staff_id"),
        task_id: text(keys[3], "task_id"),
        event_id: row.event_id.clone(),
        due_minute: row.due_minute,
    }
}

/// A value the Run read of a handler, its fault the handler's refusal.
fn read<T>(value: crate::config::read::Read<T>) -> crate::error::Result<T> {
    value.map_err(|fault| Error::unsupported(fault.text.to_string()))
}

fn apply_native<C: WorldCell, H: Host, R: RunCell>(tick: &mut Tick<'_, C, H, R>, native: &Native, row: &EventRow) -> std::result::Result<Handled, H::Error>
where
    H::Error: From<WriteError>,
{
    let cell = tick.cell;
    let label = label_of(native.kind);
    match native.kind {
        HandlerKind::SocialInteraction => Ok(match social_interaction::apply(cell, tick.host, row)? {
            None => Handled::applied(),
            Some((outcome, text)) => Handled::Outcome(status_of(outcome), text.to_owned()),
        }),
        HandlerKind::AgentTravelCompletion => travel(tick, native, row),
        HandlerKind::ToiletServiceCompletion => toilet_completion(tick, native, row),
        HandlerKind::ResourceServiceCompletion => {
            if tick.world(|world| world.schema_version())? != 6 {
                return Err(Error::failed("resource service completion requires WorldState v6").into());
            }
            let manager = service::manager_in(tick)?;
            let done = tick.cell.with(|world| service::complete(world, &manager.manager(), row)).map_err(core_err::<H::Error>)?;
            match done {
                Ok(balance) => tick.host.resources_changed(balance.index, balance.remaining)?,
                Err(failure) => return Err(service::failure_of(tick.host, failure)),
            }
            Ok(Handled::applied())
        }
        HandlerKind::ResourceServiceCancellation => {
            tick.world(|world| handlers::service_cancellation(world))?;
            let manager = service::manager_in(tick)?;
            let done = tick.cell.with(|world| service::cancel(world, &manager.manager(), row)).map_err(core_err::<H::Error>)?;
            let done = counted(&handlers::COUNTS, "resource_service_cancel", done.map_err(|failure| service::failure_of(tick.host, failure)));
            if let Some(balance) = done? {
                tick.host.resources_changed(balance.index, balance.remaining)?;
            }
            Ok(Handled::applied())
        }
        HandlerKind::SupportRequestTimeout => {
            let cooldown = read(native.cooldown_minutes())?;
            let event = support_event(row, [true, true, false, false]);
            let inputs = support::Inputs::read_metrics(cell, tick.host)?;
            Ok(match tick.world(|world| support::timeout(world, &inputs, &event, cooldown))? {
                None => Handled::applied(),
                Some(text) => Handled::Outcome(EventStatus::Suppressed, text.to_owned()),
            })
        }
        HandlerKind::StaffSupportCompletion => {
            let event = support_event(row, [true, true, true, true]);
            let handler = support::Handler { cooldown_minutes: read(native.cooldown_minutes())?, require_colocated: read(native.require_colocated())? };
            let inputs = support::Inputs::read(cell, tick.host)?;
            let complete = |tick: &mut Tick<'_, C, H, R>, formula: support::Formula<'_>| tick.world(|world| support::complete_support(world, &inputs, &event, handler, formula, 0));
            let receipt = match complete(tick, support::Formula::Unread)? {
                support::Completion::Done(planned) => planned,
                support::Completion::NeedsFormula => {
                    let formula = match support::care_formula(cell, tick.host)? {
                        None => None,
                        Some(Ok(formula)) => Some(formula),
                        Some(Err(fault)) => return Err(tick.raise(SectionId::CareOutcome, &fault)),
                    };
                    match complete(tick, support::Formula::Read(formula.as_deref()))? {
                        support::Completion::Done(planned) => planned,
                        support::Completion::NeedsFormula => return Err(Error::Runtime("the care asked for its formula again".to_owned()).into()),
                    }
                }
            };
            Ok(Handled::Applied { records: Vec::new(), receipt })
        }
        HandlerKind::ProximityContact => {
            let result = (|| {
                let holding = tick.holding()?;
                let config = match tick.host_section(&holding, SectionId::Proximity, |configs| &configs.proximity)? {
                    Ok(config) => config,
                    Err(fault) => return Err(tick.raise(SectionId::Proximity, &fault)),
                };
                let movement = holding.held(MOVEMENT, "the world holds another movement state");
                tick.world(|world| handlers::proximity(world, config.as_ref().as_ref(), &movement, row))
            })();
            counted(&handlers::COUNTS, "proximity", result).map(|()| Handled::applied())
        }
        HandlerKind::InformationRumor | HandlerKind::InformationCorrection | HandlerKind::InformationContact => {
            let name = match native.kind {
                HandlerKind::InformationRumor => "rumor",
                HandlerKind::InformationCorrection => "correction",
                _ => "contact",
            };
            counted(&handlers::COUNTS, name, information(tick, native.kind, row))
        }
        HandlerKind::ResourceLedger => {
            let result = (|| {
                let operation = read(native.operation())?;
                let holding = tick.holding()?;
                let operational = holding.held(handlers::OPERATIONAL, "the world holds another operational state");
                let service_held = holding.held("resource_service_v1", "the world holds another service state");
                let ledger = holding.held("resource_ledger_v1", "the world holds another ledger");
                let inputs = handlers::LedgerIn { operational: &operational, service: &service_held, ledger: &ledger };
                let done = tick.cell.with(|world| handlers::resource_ledger(world, &inputs, row, &operation)).map_err(core_err::<H::Error>)?;
                match done {
                    Ok(applied) => tick.host.resources_changed(applied.index, applied.remaining),
                    // The handler's `except ResourceLedgerError as exc: raise EventApplicationError(str(exc))`.
                    Err(crate::rules::resources::Fail::Ledger(fault)) => Err(Error::failed(fault.text(service::repr_of)).into()),
                    Err(crate::rules::resources::Fail::Error(error)) => Err(error.into()),
                }
            })();
            counted(&handlers::COUNTS, "resource_ledger", result).map(|()| Handled::applied())
        }
        HandlerKind::FacilityOutage | HandlerKind::FacilityRecovery => {
            let restores = native.kind == HandlerKind::FacilityRecovery;
            let result = (|| {
                let holding = tick.holding()?;
                let program = holding.held(effects::MARKER, "the world holds another effect_program_v1");
                let infrastructure = holding.held(handlers::INFRASTRUCTURE, "the world holds another infrastructure state");
                let inputs = handlers::FacilityIn { program: &program, infrastructure: &infrastructure, shared_absent: !holding.contains("shared_operations_v1") };
                tick.world(|world| handlers::facility_units(world, &inputs, row, restores))
            })();
            let made = counted(&handlers::COUNTS, if restores { "facility_recovery" } else { "facility_outage" }, result)?;
            Ok(Handled::records(made.into_iter().map(Record::from).collect()))
        }
        HandlerKind::InfrastructureEvent => {
            let (kind, name) = counted(&handlers::COUNTS, "infrastructure", handlers::Infra::of(&row.event_type).map_err(core_err::<H::Error>))?;
            tick.host.read_queue_rows()?;
            let (previous, followed) = counted(&handlers::COUNTS, name, handlers::infrastructure_event(cell, tick.host, row, kind))?;
            let receipts = handlers::infrastructure_after(cell, tick.host, &previous, row)?;
            let mut records: Vec<Record> = followed.into_iter().map(Record::from).collect();
            records.extend(receipts.into_iter().map(Record::from));
            Ok(Handled::records(records))
        }
        HandlerKind::DeliveryDelay | HandlerKind::WaterOutage | HandlerKind::WaterRestoration | HandlerKind::StaffRestoration => {
            tick.host.read_queue_rows()?;
            let (name, done) = match native.kind {
                HandlerKind::DeliveryDelay => ("delivery_delay", handlers::delivery_delay_event(cell, tick.host, row)),
                HandlerKind::WaterOutage => ("water_outage", handlers::water_outage_event(cell, tick.host, row)),
                HandlerKind::WaterRestoration => ("water_restoration", handlers::water_restoration_event(cell, tick.host, row)),
                _ => ("staff_restoration", handlers::staff_restoration_event(cell, tick.host, row)),
            };
            counted(&handlers::COUNTS, name, done).map(|()| Handled::applied())
        }
        HandlerKind::StaffUnavailability => {
            tick.host.read_queue_rows()?;
            let result = handlers::staff_unavailable_event(cell, tick.host, row);
            let result = read_again(tick, result, handlers::COOLDOWN_UNREAD, SectionId::SupportCooldownMinutes);
            let result = read_again(tick, result, operations::FORMULA_UNREAD, SectionId::StaffOperations);
            let made = counted(&handlers::COUNTS, "staff_unavailable", result)?;
            Ok(Handled::records(made.into_iter().map(Record::from).collect()))
        }
        HandlerKind::StaffOperationArrival | HandlerKind::StaffOperationRelief | HandlerKind::StaffOperationCompletion => staff_operation(tick, native.kind, row),
        HandlerKind::EffectProgramFacility | HandlerKind::EffectProgramEnvironment | HandlerKind::EffectProgramTemporarySpace | HandlerKind::EffectProgramGeometry => {
            let expire = read(native.expire())?;
            let (name, result) = match native.kind {
                HandlerKind::EffectProgramFacility => ("facility", effects::facility_event(cell, tick.host, row, expire)),
                HandlerKind::EffectProgramEnvironment => {
                    // The expiry events are read as rows of the queue.
                    tick.host.read_queue_rows()?;
                    let result = effects::environment_event(cell, tick.host, row, expire);
                    ("environment", read_again(tick, result, effects::CONFIG_UNREAD, SectionId::ZoneEnvironment))
                }
                HandlerKind::EffectProgramTemporarySpace => ("temporary_space", effects::temporary_space_event(cell, tick.host, row, expire)),
                _ => ("geometry", effects::geometry_event(cell, tick.host, row, expire)),
            };
            counted(&effects::COUNTS, name, result).map(|()| Handled::applied())
        }
        HandlerKind::EffectProgramStructure => {
            let structure = tick.world(|world| effects::structure_of(row).map(|structure| { let _ = world; structure }))?;
            let (name, result): (&'static str, std::result::Result<Option<Made>, H::Error>) = match structure {
                effects::Structure::TaskOrder => {
                    tick.host.read_queue_rows()?;
                    let result = effects::task_order_event(cell, tick.host, row).map(|()| None);
                    ("structure.staff_task_order", read_again(tick, result, operations::FORMULA_UNREAD, SectionId::StaffOperations))
                }
                effects::Structure::Handoff => {
                    tick.host.read_queue_rows()?;
                    ("structure.staff_handoff", effects::handoff_event(cell, tick.host, row))
                }
                effects::Structure::Repair => ("structure.facility_repair", effects::repair_event(cell, tick.host, row)),
                effects::Structure::Role => ("structure.set_staff_role", effects::role_event(cell, tick.host, row).map(|()| None)),
                effects::Structure::Add => ("structure.add", effects::add_event(cell, tick.host, row).map(|_| None)),
            };
            let made = counted(&effects::COUNTS, name, result)?;
            Ok(Handled::records(made.into_iter().map(Record::from).collect()))
        }
        HandlerKind::EffectProgramPopulation => Err(Error::Runtime(format!("{label} is the host's")).into()),
    }
}

fn status_of(outcome: Outcome) -> EventStatus {
    match outcome {
        Outcome::Suppressed => EventStatus::Suppressed,
        Outcome::Failed => EventStatus::Failed,
    }
}

/// A refusal the core said where the rule reads what only a Python read gives (`reason`): what that
/// read raises, read again now (policy E14); a read that raises nothing leaves the refusal.
fn read_again<T, C: WorldCell, H: Host, R: RunCell>(
    tick: &mut Tick<'_, C, H, R>,
    result: std::result::Result<T, H::Error>,
    reason: &str,
    which: SectionId,
) -> std::result::Result<T, H::Error> {
    match result {
        Err(error) if is_refusal(&error).as_deref() == Some(reason) => match tick.host.read_again(crate::host::Again::Section(which)) {
            Err(read) => Err(read),
            Ok(()) => Err(error),
        },
        result => result,
    }
}

/// The information events: their inputs, the config's fault raised before the event is read
/// (`invalid information_spread config: ...` for its `ValueError`), then the event; a corrupt
/// `model_configs` of a selection that raised is what `selected_models` raises.
fn information<C: WorldCell, H: Host, R: RunCell>(tick: &mut Tick<'_, C, H, R>, kind: HandlerKind, row: &EventRow) -> std::result::Result<Handled, H::Error> {
    let gathered = information_events::Gathered::read(tick.cell, tick.host)?;
    let config = match &gathered.config {
        Ok(config) => config.clone(),
        Err(fault) => {
            let error = tick.raise(SectionId::InformationSpread, fault);
            if error.class() == ErrorClass::Value {
                return Err(Error::failed(format!("invalid information_spread config: {}", error.text())).into());
            }
            return Err(error);
        }
    };
    let inputs = gathered.inputs(&config);
    let result = match kind {
        HandlerKind::InformationRumor => information_events::rumor(tick.cell, tick.host, &inputs, row).map(|()| Handled::applied()),
        HandlerKind::InformationCorrection => information_events::correction(tick.cell, tick.host, &inputs, row).map(|()| Handled::applied()),
        _ => information_events::contact(tick.cell, tick.host, &inputs, row).map(|applied| match applied {
            information_events::Applied::Applied => Handled::applied(),
            information_events::Applied::Suppressed(text) => Handled::Outcome(EventStatus::Suppressed, text.to_owned()),
        }),
    };
    match result {
        Err(error)
            if matches!(&gathered.selection, Err(fault) if !fault.is_refusal())
                && error.text() == Error::corrupt(crate::world::MODEL_CONFIGS, information_events::SELECTION_RAISED).to_string() =>
        {
            match tick.host.read_again(crate::host::Again::Section(SectionId::Selection)) {
                Err(read) => Err(read),
                Ok(()) => Err(error),
            }
        }
        other => other,
    }
}

/// The events of an autonomous staff operation. A completion whose work is an intake admits through
/// the population rules, whose inputs the PyO3 crate still reads: the whole event is the host's
/// (`apply`, H4), with nothing written here.
fn staff_operation<C: WorldCell, H: Host, R: RunCell>(tick: &mut Tick<'_, C, H, R>, kind: HandlerKind, row: &EventRow) -> std::result::Result<Handled, H::Error>
where
    H::Error: From<WriteError>,
{
    match kind {
        HandlerKind::StaffOperationArrival => {
            let holding = tick.holding()?;
            let gathered = tick.gathered(&holding, false, false)?;
            let planned = tick.world(|world| -> crate::error::Result<Option<Arc<str>>> {
                match operations::plan_arrive(world, &gathered.inputs(), row)? {
                    operations::Planned::Go(plan) => operations::write_arrive(world, plan).map(|()| None),
                    operations::Planned::Unavailable(id) => Ok(Some(id)),
                }
            });
            match counted(&handlers::COUNTS, "arrive", planned)? {
                None => Ok(Handled::applied()),
                Some(id) => stop_and_suppress(tick, &id, row, "staff became unavailable before reaching the assigned task"),
            }
        }
        HandlerKind::StaffOperationRelief => {
            tick.host.read_queue_rows()?;
            let holding = tick.holding()?;
            let gathered = tick.gathered(&holding, true, true)?;
            let mut taken = Vec::new();
            let result = tick.cell.with(|world| {
                let inputs = gathered.inputs();
                let plan = operations::plan_relief(world, &inputs, row)?;
                operations::write_relief(world, &inputs, plan, &mut taken)
            });
            tick.host.slots_changed(&taken)?;
            let result = result.map_err(core_err::<H::Error>).and_then(|written| tick.written(written));
            let made = counted(&handlers::COUNTS, "relief", gathered.finish(result))?;
            Ok(Handled::records(made.into_iter().map(Record::from).collect()))
        }
        _ => {
            let holding = tick.holding()?;
            let gathered = tick.gathered(&holding, false, false)?;
            enum Step {
                Done(Vec<Made>),
                Unavailable(Arc<str>),
                Intake,
            }
            let step = tick.world(|world| -> crate::error::Result<Step> {
                let inputs = gathered.inputs();
                match operations::plan_complete(world, &inputs, row)? {
                    operations::Planned::Unavailable(id) => Ok(Step::Unavailable(id)),
                    operations::Planned::Go(plan) if matches!(plan.work, operations::Work::Intake) => Ok(Step::Intake),
                    operations::Planned::Go(plan) => operations::write_complete(world, &inputs, plan).map(Step::Done),
                }
            });
            match counted(&handlers::COUNTS, "complete", step)? {
                Step::Done(made) => Ok(Handled::records(made.into_iter().map(Record::from).collect())),
                Step::Unavailable(id) => stop_and_suppress(tick, &id, row, "staff became unavailable before completing the assigned task"),
                Step::Intake => foreign(tick, row),
            }
        }
    }
}

/// `interrupt_staff_operation(world, staff_id, event, current_event_id=event.event_id)` and then
/// `raise EventSuppressed(text)`.
fn stop_and_suppress<C: WorldCell, H: Host, R: RunCell>(tick: &mut Tick<'_, C, H, R>, staff_id: &str, row: &EventRow, text: &str) -> std::result::Result<Handled, H::Error> {
    tick.host.read_queue_rows()?;
    let holding = tick.holding()?;
    let gathered = tick.gathered(&holding, true, false)?;
    let mut taken = Vec::new();
    let result = tick.cell.with(|world| operations::stop_and_suppress(world, &gathered.inputs(), staff_id, row, text, &mut taken));
    tick.host.slots_changed(&taken)?;
    let result = result.map_err(core_err::<H::Error>).and_then(|written| tick.written(written));
    gathered.finish(result)?;
    Err(Error::Runtime("a stop that did not suppress".to_owned()).into())
}

/// `ToiletServiceCompletionHandler.apply`: the release, then, where `after_complete` is the
/// controller's own step, the walk home (`_schedule_return_to_initial_zone`), whose refusal is that
/// step's `KernelRefusal`. What the walk home reads is read before the release, as the PyO3 crate
/// read it; what that read raised is the walk home's error, after the release.
fn toilet_completion<C: WorldCell, H: Host, R: RunCell>(tick: &mut Tick<'_, C, H, R>, native: &Native, row: &EventRow) -> std::result::Result<Handled, H::Error>
where
    H::Error: From<WriteError>,
{
    let goes_home = read(native.after_complete())? == AfterComplete::Own;
    let holding = tick.holding()?;
    let queues = holding.held("facility_queues_v1", "the world holds another queue state");
    let geo = tick.host.floor_plan()?;
    let body = holding.held("body_state_v1", "the world holds another body state");
    let metric_state_held = holding.field(crate::world::METRIC_STATE);
    let home = if goes_home { Some(home_of(tick, &holding)) } else { None };
    let completed = tick.cell.with(|world| toilet::complete(world, row, &queues, &geo, &body, metric_state_held)).map_err(core_err::<H::Error>)?;
    let _ = tick.written(completed)?;
    if let Some(home) = home {
        let returned = (|| {
            let home = home?;
            let routing = crate::rules::travel::Routing { graph: &home.graph, geo: home.plan.get(), movement: &home.section, staff: &home.staff };
            let walking = crate::rules::travel::Walking { movement: &home.movement, geo: home.plan.get() };
            let returned = tick.cell.with(|world| crate::rules::travel::schedule_return(world, &routing, walking, row)).map_err(core_err::<H::Error>)?;
            let returned = tick.written(returned);
            home.plan.finish(returned, is_refusal)
        })();
        returned.map_err(|error| labelled("travel.schedule_return", error))?;
    }
    Ok(Handled::applied())
}

/// What the walk home reads besides the world (`travel::Home` of the PyO3 crate).
struct Home<E> {
    graph: crate::rules::travel::Graph,
    plan: crate::rules::tick::read::Plan<E>,
    section: kit::Kept<Movement>,
    movement: Held,
    staff: Held,
}

fn home_of<C: WorldCell, H: Host, R: RunCell>(tick: &mut Tick<'_, C, H, R>, holding: &crate::host::Holding) -> std::result::Result<Home<H::Error>, H::Error> {
    let graph = crate::rules::tick::propose::graph(tick)?;
    if graph.capacities.any {
        tick.host.read_queue_rows()?;
    }
    let plan = tick.plan();
    let section = tick.value_section(holding, SectionId::Movement, |configs| &configs.movement, Movement::parse)?;
    let movement = holding.held(MOVEMENT, "the world holds another movement state");
    let staff = holding.held("staff_operations_v1", "the world holds another document than the row's");
    Ok(Home { graph, plan, section, movement, staff })
}

/// `AgentTravelCompletionHandler.apply` of a walk, then `_after_arrival` of a walk to a toilet or a
/// resource, with the controller's managers' queues (the core's `toilet::queue_up` and
/// `service::queue_up`).
fn travel<C: WorldCell, H: Host, R: RunCell>(tick: &mut Tick<'_, C, H, R>, native: &Native, row: &EventRow) -> std::result::Result<Handled, H::Error>
where
    H::Error: From<WriteError>,
{
    let read_walk = travel_completion::Gathered::read(tick.cell, tick.host, row)?;
    let resource_service = native.handler.object("resource_service").map(|manager| manager.is_some()).unwrap_or(false);
    // The pending events are read as rows (`population::with_rows` of the PyO3 crate).
    tick.host.read_queue_rows()?;
    let inputs = read_walk.inputs(resource_service, None);
    let applied = tick.cell.with(|world| travel_completion::apply(world, &inputs, row)).map_err(core_err::<H::Error>)?;
    let walk = match applied {
        Ok(Some(walk)) => walk,
        Ok(None) => return Ok(Handled::applied()),
        Err(travel_completion::Applied::Checks(error) | travel_completion::Applied::After(error)) => return Err(error.into()),
    };
    if !matches!(&*walk.purpose, "toilet_request" | "resource_request") {
        return Ok(Handled::applied());
    }
    let facility = walk.facility_id.as_deref().unwrap_or_default();
    let origin = walk.origin.zone().map(|zone| &**zone);
    let holding = tick.holding()?;
    let has_geometry = has_geometry(tick, &holding)?;
    let validates = walk.purpose.as_ref() == "toilet_request" && read(native.validate_request_state())?;
    let arrival = travel_completion::Arrival {
        agent_id: &walk.agent_id,
        purpose: &walk.purpose,
        facility_id: facility,
        destination: &walk.destination,
        origin,
        has_geometry,
        resource_service: walk.purpose.as_ref() == "resource_request" && resource_service,
        validates,
    };
    let mut managers = CoreManagers { cell: tick.cell, policies: tick.policies.clone() };
    let arrived = travel_completion::arrive(tick.cell, tick.host, &arrival, &mut managers)?;
    tick.written(arrived)?;
    Ok(Handled::applied())
}

/// `"facility_geometry" in world.model_configs`: of the slot's row while the world holds the field,
/// else of the configs Python holds, which must be a plain dict.
fn has_geometry<C: WorldCell, H: Host, R: RunCell>(tick: &mut Tick<'_, C, H, R>, holding: &crate::host::Holding) -> std::result::Result<bool, H::Error> {
    if holding.field(crate::world::MODEL_CONFIGS) {
        let found = tick.world(|world| {
            Ok(match world.slots.try_value(crate::world::MODEL_CONFIGS)?.as_deref() {
                Some(V::Map(configs)) => Some(configs.contains_key("facility_geometry")),
                _ => None,
            })
        })?;
        if let Some(found) = found {
            return Ok(found);
        }
    }
    match tick.host.configs_given()? {
        Some(V::Map(configs)) => Ok(configs.contains_key("facility_geometry")),
        _ => Err(Error::unsupported("a value is not a plain dict").into()),
    }
}

/// The controller's managers' queues, as `_after_arrival` asks them: `toilet::queue_up` and
/// `service::queue_up`, each reading what it reads when it is asked (a walk turned away never reads
/// them). A manager with its own `request` (a test's) made the whole event the host's before this.
struct CoreManagers<'c, C> {
    cell: &'c C,
    policies: Arc<crate::run::Policies>,
}

impl<C: WorldCell, H: Host> travel_completion::Managers<H> for CoreManagers<'_, C> {
    fn own(&mut self, _toilet: bool) -> std::result::Result<bool, H::Error> {
        Ok(false)
    }

    fn toilet(&mut self, host: &mut H, agent_id: &str, facility_id: &str) -> std::result::Result<(), H::Error> {
        let holding = kit::holding(self.cell, host)?;
        let queues = holding.held("facility_queues_v1", "the world holds another queue state");
        let geo = host.floor_plan()?;
        let needs = kit::kept_section_value(self.cell, host, &holding, SectionId::AccessibilityNeeds, |configs| &configs.accessibility_needs, AccessibilityNeeds::parse)?;
        let request = toilet::Request { agent_id, facility_id, queue_id: None, queues: &queues, geo: &geo, needs: &needs };
        let written = self.cell.with(|world| toilet::queue_up(world, &request).map(drop))?;
        written.map_err(|error| host.write_error(error))
    }

    fn resource(&mut self, host: &mut H, agent_id: &str, facility_id: &str) -> std::result::Result<Option<String>, H::Error> {
        let manager = service::manager_parts(self.cell, host, &self.policies)?;
        let holding = kit::holding(self.cell, host)?;
        let needs = kit::kept_section_value(self.cell, host, &holding, SectionId::AccessibilityNeeds, |configs| &configs.accessibility_needs, AccessibilityNeeds::parse)?;
        host.read_queue_rows()?;
        let pending = self.cell.with(|world| service::reserves_of(world))??;
        let request = service::Request { agent_id, facility_id, minute: service::Minute::Absent, queue_id: None, needs: &needs };
        Ok(match self.cell.with(|world| service::queue_up(world, &manager.manager(), &request, &pending))?? {
            service::Requested::Queued(_) => None,
            service::Requested::Refused(text) => Some(text),
        })
    }
}

// ---- After the events ---------------------------------------------------------------------------------

/// What follows from the events that applied, in `DispatchPhase`'s order.
fn after_events<C: WorldCell, H: Host, R: RunCell>(tick: &mut Tick<'_, C, H, R>, outcomes: &[Dispatched]) -> std::result::Result<(), H::Error> {
    let applied = || outcomes.iter().filter(|outcome| outcome.applied);
    for outcome in applied() {
        record_applied(tick, &outcome.row)?;
    }
    if applied().any(|outcome| matches!(&*outcome.row.event_type, "effect_program_geometry" | "effect_program_geometry_expire" | "effect_program_structure")) {
        count(&COUNTS.spaces_refreshed);
        tick.host.space_refreshed()?;
    }
    for outcome in applied() {
        if &*outcome.row.event_type == "effect_program_geometry" && operations::closes_a_route(&outcome.row) {
            interrupt_trips(tick, &outcome.row).map_err(|error| labelled("staff_operation_events.interrupt_trips", error))?;
        }
    }
    admit(tick, true).map_err(|error| labelled("population_lifecycle.admit_pending_returns", error))?;
    sync_movement(tick)?;
    admit(tick, false).map_err(|error| labelled("population.admit_pending_arrivals", error))?;
    for outcome in applied() {
        if &*outcome.row.event_type == "information_contact_update" {
            consider_contact(tick, &outcome.row).map_err(|error| labelled(CONTACT, error))?;
        }
    }
    Ok(())
}

/// `_record_applied_outcomes` of one applied event.
fn record_applied<C: WorldCell, H: Host, R: RunCell>(tick: &mut Tick<'_, C, H, R>, row: &EventRow) -> std::result::Result<(), H::Error> {
    let minute = tick.minute;
    let event_type = &*row.event_type;
    if action_lifecycle::ends_any_action(event_type) {
        let ending = action_lifecycle::Ending::of(row);
        let done = tick.run.with(|run| {
            // Most applied events end no open action: the world is not borrowed for them.
            if action_lifecycle::closes(&run.actions, &ending).is_none() {
                return Ok(());
            }
            tick.cell.with(|world| action_lifecycle::complete_for_event(world, &mut run.actions, &ending, minute)).and_then(|done| done)
        });
        done.and_then(|done| done).map_err(|error| labelled("action_lifecycle.record_applied_action_completion", core_err::<H::Error>(error)))?;
        count(&COUNTS.completion_ran);
    }
    if event_type == "effect_program_population" {
        let done = tick.run.with(|run| tick.cell.with(|world| action_lifecycle::population_exit(world, &mut run.actions, &row.event_id, minute)).and_then(|done| done));
        done.and_then(|done| done).map_err(|error| labelled("action_lifecycle.record_applied_population_exit", core_err::<H::Error>(error)))?;
        count(&COUNTS.population_exit_ran);
    }
    if event_type == "agent_travel_complete" {
        if let (Some(agent_id), Some(facility_id)) = (text_of(row, "agent_id"), text_of(row, "facility_id")) {
            agent_dynamics::learn_facility_through(tick.cell, tick.host, Some(&agent_id), Some(&facility_id)).map_err(|error| labelled("agent_dynamics.learn_facility", error))?;
            count(&COUNTS.learn_ran);
        }
        if text_of(row, "purpose").as_deref() == Some("staff_support_travel") {
            let event = support_event(row, [true, false, true, false]);
            if event.request_id.is_some() {
                let inputs = support::Inputs::read(tick.cell, tick.host)?;
                let written = tick.cell.with(|world| support::record_started(world, &inputs, &event)).map_err(core_err::<H::Error>)?;
                tick.written(written).map_err(|error| labelled("support.record_started", error))?;
            }
            count(&COUNTS.started_ran);
        }
    }
    if event_type == "support_request_timeout" {
        let event = support_event(row, [true, false, false, false]);
        if event.request_id.is_some() {
            let inputs = support::Inputs::read(tick.cell, tick.host)?;
            let written = tick.cell.with(|world| support::record_interrupted(world, &inputs, &event)).map_err(core_err::<H::Error>)?;
            tick.written(written).map_err(|error| labelled("support.record_interrupted", error))?;
        }
        count(&COUNTS.interrupted_ran);
    }
    Ok(())
}

/// `interrupt_staff_trips_on_closed_routes` of a geometry event that closed a route.
fn interrupt_trips<C: WorldCell, H: Host, R: RunCell>(tick: &mut Tick<'_, C, H, R>, row: &EventRow) -> std::result::Result<(), H::Error> {
    tick.host.read_queue_rows()?;
    let holding = tick.holding()?;
    let gathered = tick.gathered(&holding, true, false)?;
    let mut taken = Vec::new();
    let stopping = operations::Stopping { event_id: &row.event_id, due_minute: row.due_minute };
    let result = tick.cell.with(|world| operations::interrupt_trips(world, &gathered.inputs(), &stopping, &mut taken));
    tick.host.slots_changed(&taken)?;
    let result = result.map_err(core_err::<H::Error>).and_then(|written| tick.written(written));
    gathered.finish(result)?;
    count(&COUNTS.trips_interrupted);
    Ok(())
}

/// `admit_pending_returns` (`returns`) or `admit_pending_arrivals`: nothing past the namespace while
/// no one waits; else the population rules, through the host (P9-3).
fn admit<C: WorldCell, H: Host, R: RunCell>(tick: &mut Tick<'_, C, H, R>, returns: bool) -> std::result::Result<(), H::Error> {
    let holding = tick.holding()?;
    let nothing = if returns {
        let lifecycle = holding.held(population_lifecycle::KEY, "the world holds another lifecycle state");
        tick.cell.with(|world| population_lifecycle::nothing_to_admit(world, &lifecycle)).and_then(|done| done).map_err(planning)?
    } else {
        let dynamics = holding.held(population::DYNAMICS, "the world holds another population dynamics state");
        tick.world(|world| population::nothing_waiting(world, &dynamics))?
    };
    if nothing {
        return Ok(());
    }
    count(if returns { &COUNTS.returns_asked } else { &COUNTS.arrivals_asked });
    tick.host.admit_pending(returns, tick.minute)
}

/// A plan's read error, as the population rules' `planning` says it: a refusal stays itself, any
/// other error but an outcome is `the plan read: ...`.
fn planning<E: From<Error>>(error: Error) -> E {
    match error {
        error @ (Error::Unsupported(_) | Error::Outcome(..)) => error.into(),
        other => Error::unsupported(format!("the plan read: {other}")).into(),
    }
}

/// `_sync_movement_after_events`: the journeys of people who stopped moving are finished, in ID
/// order; a queue trip of a person queued at the service stays.
pub fn sync_movement<C: WorldCell, H: Host, R: RunCell>(tick: &mut Tick<'_, C, H, R>) -> std::result::Result<(), H::Error> {
    let holding = tick.holding()?;
    if !holding.contains(MOVEMENT) {
        return Ok(());
    }
    let movement = holding.held(MOVEMENT, "the world holds another movement state");
    let ids = tick
        .world(|world| {
            let Some(row) = kit::held_row::<crate::state::movement::MovementRow>(world, MOVEMENT, &movement)? else {
                return Err(Error::unsupported("the movement state is None"));
            };
            let mut ids: Vec<Arc<str>> = row.journeys.iter().map(|(id, _)| id.clone()).collect();
            // `sorted`: bytes of UTF-8 sort as code points do.
            ids.sort_unstable();
            Ok(ids)
        })
        .map_err(|error| labelled(SYNC, error))?;
    count(&COUNTS.sync_ran);
    for id in ids {
        let stopped = tick
            .world(|world| {
                let row = world.row::<crate::state::movement::MovementRow>(MOVEMENT)?;
                let purpose = row.journeys.get(&*id).map(|journey| journey.purpose.clone());
                let rows = world.rows()?;
                Ok(match rows.agents.get(&*id) {
                    None => Some(purpose),
                    Some(agent) => {
                        let queue_trip = agent.activity == crate::entities::Activity::Queued
                            && matches!(purpose.as_deref(), Some("toilet_queue_position" | "resource_queue_position" | "toilet_queue_service" | "resource_queue_service"));
                        (agent.activity != crate::entities::Activity::Moving && !queue_trip).then_some(purpose)
                    }
                })
            })
            .map_err(|error| labelled(SYNC, error))?;
        if let Some(purpose) = stopped {
            finish_journey(tick, &id, purpose.as_deref())?;
        }
    }
    Ok(())
}

/// `_finish_stopped_journey`: `finish_movement`, and a person who walked to a reserved bed is placed
/// at its point (the row's write; an attached tree follows it).
fn finish_journey<C: WorldCell, H: Host, R: RunCell>(tick: &mut Tick<'_, C, H, R>, id: &Arc<str>, purpose: Option<&str>) -> std::result::Result<(), H::Error> {
    let bed = if purpose == Some("sleep_bed") {
        let holding = tick.holding()?;
        let overlay = holding.held(OVERLAY, "the world holds another overlay state");
        tick.world(|world| {
            let row = kit::held_row::<crate::row::ValueRow>(world, OVERLAY, &overlay)?;
            movement::reserved_bed_of(row.as_deref().map(|row| &row.0), id)
        })
        .map_err(|error| labelled(SYNC, error))?
    } else {
        None
    };
    movement::finish_through(tick.cell, tick.host, id).map_err(|error| labelled("movement.finish_movement", error))?;
    count(&COUNTS.journeys_finished);
    let Some(bed) = bed else {
        return Ok(());
    };
    let holding = tick.holding()?;
    let placed = tick.world(|world| {
        let zone_id = match world.rows()?.agents.get(&**id) {
            Some(agent) => agent.zone_id.clone(),
            None => return Ok(false),
        };
        movement::place_at_bed_held(world, &holding, id, &zone_id, &bed)
    });
    if placed.map_err(|error| labelled(SYNC, error))? {
        count(&COUNTS.beds_placed);
    }
    Ok(())
}

/// `_consider_social_contact_event` of an applied `information_contact_update` (decision D5): a
/// contact within a household under stress asks the social policy to consider cooperation; a false
/// message the recipient has heard asks it to consider a reaction. Nothing without a social policy.
pub fn consider_contact<C: WorldCell, H: Host, R: RunCell>(tick: &mut Tick<'_, C, H, R>, row: &EventRow) -> std::result::Result<(), H::Error> {
    if !tick.host.llm()?.social_policy {
        return Ok(());
    }
    count(&COUNTS.contacts_considered);
    let (Some(sender_id), Some(recipient_id)) = (text_of(row, "sender_agent_id"), text_of(row, "recipient_agent_id")) else {
        return Ok(());
    };
    let message_id = text_of(row, "message_id");
    let holding = tick.holding()?;
    let presence = holding.held(population::PRESENCE, "the world holds another presence state");
    let together = tick.world(|world| {
        let rows = world.rows()?;
        let (Some(sender), Some(recipient)) = (rows.agents.get(&*sender_id), rows.agents.get(&*recipient_id)) else {
            return Ok(None);
        };
        if sender_id == recipient_id {
            return Ok(None);
        }
        let stress = crate::mathx::py_max(sender.stress, recipient.stress);
        drop(rows);
        let household = |id: &str| population_lifecycle::household_of(world, &presence, id).map_err(|error| planning::<Error>(error));
        let first = household(&sender_id)?;
        Ok(Some(first.is_some() && first == household(&recipient_id)?).map(|same| (same, stress)))
    })?;
    let Some((same_household, stress)) = together else {
        return Ok(());
    };
    if same_household && stress >= stress_threshold(tick)? {
        count(&COUNTS.cooperation_asked);
        tick.host.consider_cooperation(&sender_id, &recipient_id, &row.event_id)?;
    }
    let Some(message_id) = message_id else {
        return Ok(());
    };
    let holding = tick.holding()?;
    let held = holding.held_dict(crate::state::information::NAME, information_events::OTHER_DOCUMENT);
    let reacts = tick.world(|world| {
        let Some(state) = kit::held_row::<crate::state::information::InformationRow>(world, crate::state::information::NAME, &held)? else {
            return Ok(false);
        };
        let (Some(message), Some(belief)) = (state.messages.get(&*message_id), state.beliefs.get(&*recipient_id).and_then(|beliefs| beliefs.get(&*message_id))) else {
            return Ok(false);
        };
        if !message.parses() || !belief.parses() {
            return Err(Error::unsupported("a message or a belief the contract does not take"));
        }
        Ok(&*message.truth_status == "false" && &*belief.belief == "heard")
    })?;
    if reacts {
        count(&COUNTS.reaction_asked);
        tick.host.consider_information_reaction(&recipient_id, &sender_id, &message_id, &row.event_id)?;
    }
    Ok(())
}

/// `social_policy.limits.stress_threshold`, as the Run read it.
fn stress_threshold<C: WorldCell, H: Host, R: RunCell>(tick: &mut Tick<'_, C, H, R>) -> std::result::Result<f64, H::Error> {
    let policies = tick.policies.clone();
    let limits = match policies.social.limits() {
        Ok(Some(limits)) => limits.clone(),
        Ok(None) => return Err(Error::unsupported("the social policy has no limits").into()),
        Err(fault) => return Err(tick.policy_error(&["social_policy", "limits"], &fault)),
    };
    match limits.value("stress_threshold") {
        Ok(V::Float(threshold)) => Ok(*threshold),
        Ok(V::Int(threshold)) => Ok(*threshold as f64),
        Ok(_) => Err(Error::unsupported("the stress threshold is not a number").into()),
        Err(fault) => Err(tick.policy_error(&["social_policy", "limits", "stress_threshold"], &fault)),
    }
}
