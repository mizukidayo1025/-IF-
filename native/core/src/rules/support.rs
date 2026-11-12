//! The support requests of `domain/staff.py` (`request`, `timeout_support_request`), the end of a
//! staff member's support (`StaffSupportCompletionHandler`), and the care receipts of the steps
//! around them (`experiments/phases/dispatch.py`, `_record_assigned_support`), as functions over the
//! `World` (lane P8 b4 of Issue #152). Each plans with nothing changed and then writes; a refusal
//! (`Unsupported`) before the writes leaves the event to the Python rule.
//!
//! What only Python can say is handed in by the PyO3 crate's `rules/support.rs`: whether
//! `model_state` holds `shared_operations_v1` and `body_state_v1` and whether `metric_state` is its
//! slot's tree (`Inputs`), the event's IDs as the dispatcher read them (`SupportEvent`), the
//! handler's and the service's values, and the selected care formula (`Formula`, a section of the
//! configs the PyO3 crate fills from `create_care_outcome_formula`; `CareOutcome::parse` is the
//! built-in one of a build without Python). A value of the service whose read raised is refused
//! where the rule reads it (`AFTER_UNREAD` and the others), and the PyO3 crate raises what reading
//! it again raises.
//!
//! The writes go to the rows (entities, `metric_state`, `body_state_v1`, `shared_operations_v1`,
//! `id_counters`, the queue and the log); the Python objects and the attached trees follow them when
//! the world is given back (6-1, P8-0c).

use std::sync::Arc;

use indexmap::IndexMap;
use sha2::{Digest, Sha256};

use crate::config::body::CareOutcome;
use crate::config::SectionId;
use crate::host::Host;
use crate::entities::{Activity, RequestRow, RequestStatus};
use crate::error::{Error, Outcome, Result};
use crate::events::writer::{WriteError, WriteResult};
use crate::events::{EventKind, EventRow};
use crate::mathx::{py_max, py_min};
use crate::rules::care_receipts::{self, person_context, source_ids_of, PlannedTransition, Transition};
use crate::rules::kit::{self, add_metric, check_metric_state, set_agent, Events, Held};
use crate::state::body_state::{BodyRecord, BodyRow, NAME as BODY};
use crate::state::shared_operations::{SharedOperationsRow, NAME as SHARED};
use crate::text::py_repr;
use crate::value::V;
use crate::world::{World, WorldCell, MODEL_CONFIGS};

const STAFF_SUPPORT_SOURCE: &str = "ASM-STAFF-SUPPORT-LIFECYCLE-V1";
const UNRESOLVED_METRIC: &str = "support_unresolved_count";
const RESOLVED_METRIC: &str = "support_resolved_count";
const BODY_SOURCE: &str = "ASM-LIFE-STATE-FEEDBACK-V1";
const EMOTIONAL_SUPPORT: &str = "emotional_support";
const OPERATIONAL: &str = "operational_events_v1";
const AGENT: &str = "agent";
const MODEL_STATE: &str = "model_state";
/// The label of the care receipts' refusal past a step's first change.
const APPEND_LABEL: &str = "care_receipts.append_transition";
/// What the handler says of a request that has left `pending`.
pub const NO_LONGER_PENDING: &str = "support request is no longer pending";
/// `Held::Other` of `shared_operations_v1` whose key holds None.
pub const NAMESPACE_IS_NONE: &str = "the namespace is None";
/// What the care receipts say of a `shared_operations_v1` that is not the slot's.
pub const OTHER_DOCUMENT: &str = "the world holds another document than the row's";
/// A value of the staff service whose read raised, refused where the rule reads it: the PyO3 crate
/// reads it again and raises what that raises.
pub const AFTER_UNREAD: &str = "the service's unresolved_after_minutes raised when it was read";
pub const COOLDOWN_UNREAD: &str = "the service's cooldown_minutes raised when it was read";
pub const POLICY_UNREAD: &str = "the service's policy_version raised when it was read";

/// What the support rules take of the Python world beside the rows.
#[derive(Clone, Debug)]
pub struct Inputs {
    /// `shared_operations_v1` in `model_state`: `Held::Other(NAMESPACE_IS_NONE)` where the key
    /// holds None.
    pub shared: Held,
    /// `model_state.get("body_state_v1") is not None`.
    pub body_present: bool,
    /// Whether `world.metric_state` is its slot's tree.
    pub metric_state_held: bool,
}

impl Inputs {
    /// What the receipts and the end of support read, through the host (H1, lane P9-1a), in the
    /// order the PyO3 crate read them: `shared_operations_v1`, `body_state_v1`, `metric_state`. A
    /// `model_state` that is not a plain dict is refused.
    pub fn read<C: WorldCell, H: Host>(cell: &C, host: &mut H) -> std::result::Result<Self, H::Error> {
        let holding = kit::holding(cell, host)?;
        let absent = |name: &str| holding.is_absent(name).map_err(Error::unsupported);
        let shared = if absent(SHARED)? {
            if holding.contains(SHARED) { Held::Other(NAMESPACE_IS_NONE) } else { Held::Absent }
        } else {
            holding.held(SHARED, OTHER_DOCUMENT)
        };
        let body_present = !absent(BODY)?;
        Ok(Self { shared, body_present, metric_state_held: holding.field(crate::world::METRIC_STATE) })
    }

    /// What a timeout reads: whether `metric_state` is its slot's tree.
    pub fn read_metrics<C: WorldCell, H: Host>(cell: &C, host: &mut H) -> std::result::Result<Self, H::Error> {
        let holding = kit::holding(cell, host)?;
        Ok(Self { shared: Held::Absent, body_present: false, metric_state_held: holding.field(crate::world::METRIC_STATE) })
    }
}

/// The refusal of a model selection that is not one, where the care reads it.
pub const NOT_A_SELECTION: &str = "not a selection";
/// The refusal of a model selection whose read raised, where the care reads it.
pub const SELECTION_REFUSES: &str = "the model selection refuses";

/// The selected care formula (`create_care_outcome_formula`), read through the host (H2, lane
/// P9-1a): None where the selection names no care model, else the section `care_outcome` as kept,
/// whose fault the caller raises (the PyO3 crate parses it again, policy E14).
pub fn care_formula<C: WorldCell, H: Host>(cell: &C, host: &mut H) -> std::result::Result<Option<kit::Kept<CareOutcome>>, H::Error> {
    let holding = kit::holding(cell, host)?;
    let (selection, kept) = kit::selection(cell, host, &holding)?;
        let selection = match selection {
        Ok(selection) => selection,
        Err(fault) if fault.is_refusal() => return Err(Error::unsupported(NOT_A_SELECTION).into()),
        Err(_) => return Err(Error::unsupported(SELECTION_REFUSES).into()),
    };
    if selection.get("care_outcome").is_none() {
        return Ok(None);
    }
    Ok(Some(kit::kept_of(kit::section_of_selection(cell, host, &holding, kept, SectionId::CareOutcome, |configs| &configs.care_outcome)?)))
}

/// What a support rule reads of its event: the payload's values that are str, or None.
#[derive(Clone, Debug, Default)]
pub struct SupportEvent {
    pub event_id: Arc<str>,
    pub due_minute: i64,
    pub request_id: Option<Arc<str>>,
    pub agent_id: Option<Arc<str>>,
    pub staff_id: Option<Arc<str>>,
    pub task_id: Option<Arc<str>>,
}

fn failed<T>(text: impl Into<String>) -> Result<T> {
    Err(Error::Outcome(Outcome::Failed, text.into()))
}

fn suppressed<T>(text: impl Into<String>) -> Result<T> {
    Err(Error::Outcome(Outcome::Suppressed, text.into()))
}

/// `SupportRequestState(id, agent_id, requested_minute, timeout_minute)`.
fn new_request(id: &Arc<str>, agent_id: &str, requested_minute: i64, timeout_minute: i64) -> RequestRow {
    RequestRow {
        id: id.clone(),
        agent_id: Arc::from(agent_id),
        requested_minute,
        timeout_minute,
        status: RequestStatus::Pending,
        assigned_staff_id: None,
        task_id: None,
        resolved_minute: None,
        cooldown_until: 0,
        last_assignment_attempt_minute: None,
        last_assignment_wait_reason_code: None,
    }
}

// ---- A request times out ------------------------------------------------------------------------------

/// `SupportRequestTimeoutHandler.apply` up to the error text: `Some(text)` when Python would suppress
/// the event. `cooldown_minutes` is the handler's.
pub fn timeout(world: &World, inputs: &Inputs, event: &SupportEvent, cooldown_minutes: i64) -> Result<Option<&'static str>> {
    let (Some(request_id), Some(agent_id)) = (&event.request_id, &event.agent_id) else {
        return failed("support timeout requires request_id and agent_id");
    };
    let minute = world.minute_index()?;
    let status = world.rows()?.support_requests.get(request_id).filter(|request| request.agent_id == *agent_id).map(|request| request.status);
    let Some(status) = status else {
        return failed(format!("support request {} does not match its Agent", py_repr(request_id)));
    };
    if status != RequestStatus::Pending {
        return Ok(Some(NO_LONGER_PENDING));
    }
    let Some(until) = minute.checked_add(cooldown_minutes) else {
        return Err(Error::unsupported("a minute out of range"));
    };
    check_metric_state(world, inputs.metric_state_held)?;
    if !world.rows()?.agents.contains_key(agent_id) {
        return Err(Error::unsupported("no such request or person"));
    }

    // Write.
    {
        let mut rows = world.rows_mut()?;
        if let Some(request) = rows.support_requests.get_mut(request_id) {
            request.status = RequestStatus::Unresolved;
            request.resolved_minute = Some(minute);
            request.cooldown_until = until;
        }
        let Some(row) = rows.agents.get_mut(agent_id) else {
            return Err(Error::unsupported("no such agent"));
        };
        row.support_request_id = None;
        row.support_cooldown_until = until;
        set_agent(&mut rows, agent_id, Activity::Idle, None)?;
    }
    add_metric(world, UNRESOLVED_METRIC)?;
    Ok(None)
}

// ---- The care receipts around a request ------------------------------------------------------------

/// `care_receipt_for_request`'s `household_id` and `support_kind`. None without a receipt.
fn prior_of(world: &World, inputs: &Inputs, request_id: &str) -> Result<Option<(Option<Arc<str>>, Arc<str>)>> {
    match &inputs.shared {
        Held::Absent => return Ok(None),
        Held::Other(reason) if *reason == NAMESPACE_IS_NONE => return Err(Error::unsupported(NAMESPACE_IS_NONE)),
        _ => {}
    }
    let row = world.row::<SharedOperationsRow>(SHARED)?;
    Ok(row.support_requests.map.get(request_id).map(|head| (head.household_id.clone(), head.support_kind.clone())))
}

/// The care receipts' check that the world's `model_state` holds the slot's tree (or its detached
/// placeholder) of `shared_operations_v1`.
fn shared_held(inputs: &Inputs) -> Result<()> {
    match &inputs.shared {
        Held::Held => Ok(()),
        Held::Other(reason) if *reason != NAMESPACE_IS_NONE => Err(Error::unsupported(*reason)),
        _ => Err(Error::unsupported(OTHER_DOCUMENT)),
    }
}

/// `append_applied_care_transition` once the namespace is known to be there.
fn append(world: &mut World, inputs: &Inputs, transition: Transition) -> WriteResult<()> {
    shared_held(inputs)?;
    care_receipts::append(world, transition)
}

/// `make_care_transition_record` and the checks of the namespace, at this sequence.
fn plan_transition(world: &World, inputs: &Inputs, transition: Transition, sequence: i64) -> Result<PlannedTransition> {
    shared_held(inputs)?;
    care_receipts::plan_transition(world, transition, sequence)
}

/// `_record_request_interrupted`, of an applied `support_request_timeout`. Without a request the
/// Python rule finds none and records nothing.
pub fn record_interrupted(world: &mut World, inputs: &Inputs, event: &SupportEvent) -> WriteResult<()> {
    let Some(request_id) = event.request_id.clone() else {
        return Ok(());
    };
    let agent_id = world
        .rows()?
        .support_requests
        .get(&request_id)
        .and_then(|request| (request.status == RequestStatus::Unresolved).then(|| request.agent_id.clone()));
    let Some(agent_id) = agent_id else {
        return Ok(());
    };
    let Some((household_id, support_kind)) = prior_of(world, inputs, &request_id)? else {
        return Ok(());
    };
    let source_ids = source_ids_of(world, &agent_id)?;
    let transition = Transition {
        request_id,
        agent_id,
        household_id,
        support_kind,
        status: "interrupted",
        staff_id: None,
        causation_id: Some(event.event_id.clone()),
        source_ids,
        outcome: None,
    };
    append(world, inputs, transition)
}

/// `_record_staff_support_started`, of an applied `agent_travel_complete` of a staff trip. Without a
/// request the Python rule finds none and records nothing.
pub fn record_started(world: &mut World, inputs: &Inputs, event: &SupportEvent) -> WriteResult<()> {
    let Some(request_id) = event.request_id.clone() else {
        return Ok(());
    };
    let staff_id = event.staff_id.clone();
    let found = {
        let rows = world.rows()?;
        match rows.support_requests.get(&request_id) {
            None => None,
            Some(request) if request.status != RequestStatus::Assigned || request.assigned_staff_id != staff_id => None,
            Some(request) => {
                let Some(agent) = rows.agents.get(&request.agent_id) else {
                    return Err(Error::unsupported("the request's person is gone").into());
                };
                (agent.activity == Activity::SeekingHelp).then(|| (request.agent_id.clone(), request.task_id.clone()))
            }
        }
    };
    let Some((agent_id, task_id)) = found else {
        return Ok(());
    };
    let Some((household_id, support_kind)) = prior_of(world, inputs, &request_id)? else {
        return Ok(());
    };
    let source_ids = source_ids_of(world, &agent_id)?;
    let transition =
        Transition { request_id, agent_id, household_id, support_kind, status: "started", staff_id, causation_id: task_id, source_ids, outcome: None };
    append(world, inputs, transition)
}

/// `_record_assigned_support`: the receipts of the requests just assigned, in the order given. What
/// it writes is not undone, so a refusal past the first change is the care receipts' `KernelRefusal`
/// (`care_receipts.append_transition`), and one of the person's sources is the caller's.
pub fn record_assigned(world: &mut World, inputs: &Inputs, assigned: &[Arc<str>]) -> WriteResult<()> {
    for id in assigned {
        let prior = match &inputs.shared {
            Held::Absent => None,
            Held::Other(reason) if *reason == NAMESPACE_IS_NONE => None,
            _ => world.row::<SharedOperationsRow>(SHARED)?.support_requests.map.get(&**id).map(|head| (head.household_id.clone(), head.support_kind.clone())),
        };
        let Some((household_id, support_kind)) = prior else {
            continue;
        };
        let found = world.rows()?.support_requests.get(id).and_then(|request| {
            request.assigned_staff_id.clone().map(|staff| (request.agent_id.clone(), staff, request.task_id.clone()))
        });
        let Some((agent_id, staff_id, task_id)) = found else {
            continue;
        };
        let transition = |world: &World, status: &'static str| -> Result<Transition> {
            Ok(Transition {
                request_id: id.clone(),
                agent_id: agent_id.clone(),
                household_id: household_id.clone(),
                support_kind: support_kind.clone(),
                status,
                staff_id: Some(staff_id.clone()),
                causation_id: task_id.clone(),
                source_ids: source_ids_of(world, &agent_id)?,
                outcome: None,
            })
        };
        let assigned = transition(world, "assigned")?;
        append_applied(world, inputs, assigned)?;
        let seeking = world.rows()?.agents.get(&agent_id).map(|agent| agent.activity == Activity::SeekingHelp);
        let Some(seeking) = seeking else {
            return Err(Error::Key(agent_id.to_string()).into());
        };
        if seeking {
            let started = transition(world, "started")?;
            append_applied(world, inputs, started)?;
        }
    }
    Ok(())
}

/// `append_applied_care_transition` of a step past its first change, as its Python entry does it:
/// nothing without the namespace, and a refusal is that entry's `KernelRefusal`.
fn append_applied(world: &mut World, inputs: &Inputs, transition: Transition) -> WriteResult<()> {
    let done = match &inputs.shared {
        Held::Absent => Ok(()),
        Held::Other(reason) if *reason == NAMESPACE_IS_NONE => Err(WriteError::Error(Error::unsupported(NAMESPACE_IS_NONE))),
        _ => append(world, inputs, transition),
    };
    done.map_err(|error| match error {
        WriteError::Error(Error::Unsupported(reason)) => WriteError::Error(Error::Refused { label: APPEND_LABEL.to_owned(), reason }),
        other => other,
    })
}

// ---- A staff member's support ends ------------------------------------------------------------------

fn config_float(config: &V, by_agent: &str, name: &str, agent_id: &str) -> Result<f64> {
    // `float(config.get(by_agent, {}).get(agent, config.get(name, 0.0)))`, of floats.
    let V::Map(config) = config else {
        return Err(Error::unsupported("the config is not a document"));
    };
    let value = match config.get(by_agent) {
        None => None,
        Some(V::Map(values)) => values.get(agent_id),
        Some(_) => return Err(Error::unsupported("the config's table is not a document")),
    };
    match value.or_else(|| config.get(name)) {
        None => Ok(0.0),
        Some(V::Float(value)) => Ok(*value),
        Some(_) => Err(Error::unsupported("a relief that is not a float")),
    }
}

/// A `NumericStateChange` document.
fn change(kind: &str, entity: &Arc<str>, path: &str, before: f64, after: f64) -> V {
    let mut document: IndexMap<Arc<str>, V> = IndexMap::with_capacity(6);
    document.insert(Arc::from("entity_id"), V::Str(entity.clone()));
    document.insert(Arc::from("entity_kind"), V::Str(Arc::from(kind)));
    document.insert(Arc::from("field_path"), V::Str(Arc::from(path)));
    document.insert(Arc::from("before"), V::Float(before));
    document.insert(Arc::from("after"), V::Float(after));
    document.insert(Arc::from("unit"), V::Str(Arc::from("normalized_0_1")));
    V::Map(document)
}

/// What the care does to the person and their body: their new values.
enum Care {
    None,
    /// What a selected care formula does: each person's new stress and body record.
    Formula { people: Vec<CaredPerson> },
    Emotional { stress: f64, trust: f64, record: BodyRecord },
    Hygiene { record: BodyRecord },
}

/// One person whom a formula's care changes.
struct CaredPerson {
    agent_id: Arc<str>,
    stress: Option<f64>,
    record: BodyRecord,
}

/// What `StaffSupportCompletionHandler` reads of itself.
#[derive(Clone, Copy, Debug)]
pub struct Handler {
    pub cooldown_minutes: i64,
    pub require_colocated: bool,
}

/// The selected care formula (`create_care_outcome_formula`): not read yet, or read.
#[derive(Clone, Copy)]
pub enum Formula<'a> {
    Unread,
    Read(Option<&'a CareOutcome>),
}

/// What `complete_support` did.
pub enum Completion {
    /// Done, with the planned transition (its sequence allocated), if the request has a receipt.
    Done(Option<PlannedTransition>),
    /// The care needs the selected formula, which the caller reads and gives in a second call.
    /// Nothing was written.
    NeedsFormula,
}

/// `StaffSupportCompletionHandler.apply` up to the receipt: the transition it returns as a record,
/// planned with its sequence allocated, or None without a receipt. `reserved` is what the caller's
/// plan took of the sequences before it.
pub fn complete_support(world: &World, inputs: &Inputs, event: &SupportEvent, handler: Handler, formula: Formula<'_>, reserved: usize) -> Result<Completion> {
    let ids = (&event.request_id, &event.agent_id, &event.staff_id, &event.task_id);
    let (Some(request_id), Some(agent_id), Some(staff_id), Some(task_id)) = ids else {
        return failed("staff completion requires request, Agent, staff, and task IDs");
    };
    if request_id.is_empty() || agent_id.is_empty() || staff_id.is_empty() || task_id.is_empty() {
        return failed("staff completion requires request, Agent, staff, and task IDs");
    }
    let minute = world.minute_index()?;
    let due = event.due_minute;
    // The checks of the handler, then of `complete_support_task`, in their order: which one failed.
    enum Failed {
        NoTask,
        NotDue,
        NotAssigned,
        NotMatching,
    }
    let checked = {
        let rows = world.rows()?;
        (|| {
            let Some(task) = rows.active_tasks.get(task_id) else {
                return Err(Failed::NoTask);
            };
            if task.due_minute != due {
                return Err(Failed::NotDue);
            }
            let Some(request) = rows.support_requests.get(request_id) else {
                return Err(Failed::NotAssigned);
            };
            if request.status != RequestStatus::Assigned {
                return Err(Failed::NotAssigned);
            }
            let (Some(agent), Some(person)) = (rows.agents.get(agent_id), rows.staff.get(staff_id)) else {
                return Err(Failed::NotMatching);
            };
            if &*task.kind != "staff_support"
                || request.agent_id != *agent_id
                || request.assigned_staff_id.as_deref() != Some(&**staff_id)
                || request.task_id.as_deref() != Some(&**task_id)
                || task.agent_id.as_deref() != Some(&**agent_id)
                || task.staff_id.as_deref() != Some(&**staff_id)
                || task.due_minute != minute
                || person.active_task_id.as_deref() != Some(&**task_id)
                || (handler.require_colocated && agent.activity != Activity::SeekingHelp)
                || (handler.require_colocated && agent.zone_id != person.zone_id)
            {
                return Err(Failed::NotMatching);
            }
            Ok((agent.stress, agent.trust))
        })()
    };
    let (stress, trust) = match checked {
        Ok(values) => values,
        Err(Failed::NoTask) => {
            if cancelled(world, task_id)? {
                return suppressed(format!("staff support task {} was cancelled", py_repr(task_id)));
            }
            return failed("staff completion does not match an active due task");
        }
        Err(Failed::NotDue) => return failed("staff completion does not match an active due task"),
        Err(Failed::NotAssigned) => return failed(format!("support request {} is not assigned", py_repr(request_id))),
        Err(Failed::NotMatching) => return failed("support completion does not match its assigned task"),
    };
    let Some(until) = minute.checked_add(handler.cooldown_minutes) else {
        return Err(Error::unsupported("a minute out of range"));
    };
    check_metric_state(world, inputs.metric_state_held)?;
    let events = Events::of(world)?;
    events.reserve(reserved);
    let (care, transition) = match prior_of(world, inputs, request_id)? {
        None => (Care::None, None),
        Some((household_id, support_kind)) => {
            // Of the kinds, only emotional support is the same under every care formula.
            let selected = match (&*support_kind == EMOTIONAL_SUPPORT, formula) {
                (true, _) => None,
                (false, Formula::Unread) => return Ok(Completion::NeedsFormula),
                (false, Formula::Read(selected)) => selected,
            };
            let (care, outcome, source_ids) = match selected {
                Some(formula) => formula_care(world, inputs, formula, agent_id, &support_kind, household_id.as_deref(), stress)?,
                None => fixed_care(world, inputs, agent_id, &support_kind, stress, trust)?,
            };
            let transition = Transition {
                request_id: request_id.clone(),
                agent_id: agent_id.clone(),
                household_id,
                support_kind,
                status: "completed",
                staff_id: Some(staff_id.clone()),
                causation_id: Some(event.event_id.clone()),
                source_ids,
                outcome: Some(outcome),
            };
            (care, Some(plan_transition(world, inputs, transition, events.next_sequence()?)?))
        }
    };
    {
        let rows = world.rows()?;
        if !rows.agents.contains_key(agent_id) || !rows.staff.contains_key(staff_id) {
            return Err(Error::unsupported("a row has no object"));
        }
    }

    // Write: `complete_support_task`, then what the care does.
    {
        let mut rows = world.rows_mut()?;
        rows.active_tasks.remove_row(task_id);
        let Some(row) = rows.staff.get_mut(staff_id) else {
            return Err(Error::unsupported("no such staff"));
        };
        row.work_queue.clear();
        row.active_task_id = None;
        row.busy_until = 0;
        if let Some(request) = rows.support_requests.get_mut(request_id) {
            request.status = RequestStatus::Resolved;
            request.resolved_minute = Some(minute);
            request.cooldown_until = until;
        }
        set_agent(&mut rows, agent_id, Activity::Idle, None)?;
        let Some(row) = rows.agents.get_mut(agent_id) else {
            return Err(Error::unsupported("no such agent"));
        };
        row.support_request_id = None;
        row.support_cooldown_until = until;
        if let Care::Emotional { stress, trust, .. } = care {
            row.stress = stress;
            row.trust = trust;
        }
        if let Care::Formula { people } = &care {
            for person in people {
                if let (Some(stress), Some(row)) = (person.stress, rows.agents.get_mut(&person.agent_id)) {
                    row.stress = stress;
                }
            }
        }
    }
    add_metric(world, RESOLVED_METRIC)?;
    let records: Vec<(&Arc<str>, BodyRecord)> = match &care {
        Care::None => Vec::new(),
        Care::Formula { people } => people.iter().map(|person| (&person.agent_id, person.record)).collect(),
        Care::Emotional { record, .. } | Care::Hygiene { record } => vec![(agent_id, *record)],
    };
    if !records.is_empty() {
        let mut row = world.row_mut::<BodyRow>(BODY)?;
        for (id, next) in records {
            let Some(record) = row.agents.get_mut(&**id) else {
                return Err(Error::Runtime("a planned record left the row".to_owned()));
            };
            *record = next;
        }
    }
    if let Some(planned) = &transition {
        events.allocate_sequence().and_then(|sequence| match sequence == planned.row().sequence {
            true => Ok(()),
            false => Err(Error::Runtime("the event counter moved after the plan".to_owned())),
        })?;
    }
    Ok(Completion::Done(transition))
}

/// `task_id in world.model_state["operational_events_v1"]["cancelled_staff_task_ids"]`, where the
/// namespace is there and not None.
fn cancelled(world: &World, task_id: &str) -> Result<bool> {
    let state = world.slots.try_value(OPERATIONAL)?;
    let state = match state.as_deref() {
        None | Some(V::Null) => return Ok(false),
        Some(V::Map(state)) => state,
        Some(_) => return Err(Error::unsupported("a value is not a plain dict")),
    };
    match state.get("cancelled_staff_task_ids") {
        None => Err(Error::Key("cancelled_staff_task_ids".to_owned())),
        Some(V::List(ids)) => Ok(ids.iter().any(|id| matches!(id, V::Str(id) if &**id == task_id))),
        Some(_) => Err(Error::unsupported("the cancelled tasks are not a list")),
    }
}

/// The care without a selected formula: emotional support and hygiene support change the person and
/// their body where the body has their record; anything else is not measured.
fn fixed_care(world: &World, inputs: &Inputs, agent_id: &Arc<str>, kind: &str, stress: f64, trust: f64) -> Result<(Care, (&'static str, V), Vec<Arc<str>>)> {
    let emotional = kind == EMOTIONAL_SUPPORT;
    let hygiene = kind == "hygiene_support";
    let mut source_ids = vec![Arc::from(STAFF_SUPPORT_SOURCE)];
    // Without the body state, or without the person's record in it, nothing is measured and nothing
    // changes: the rule reads and writes the body only when it is a dict.
    let found = if (emotional || hygiene) && inputs.body_present {
        let row = world.row::<BodyRow>(BODY)?;
        match row.agents.get(&**agent_id).copied() {
            None => None,
            Some(record) => {
                let name = if emotional { "complaint_reassurance_gain" } else { "hygiene_support_relief" };
                let by_agent = if emotional { "complaint_reassurance_gain_by_agent" } else { "hygiene_support_relief_by_agent" };
                Some((record, config_float(&row.config, by_agent, name, agent_id)?))
            }
        }
    } else {
        None
    };
    let Some((record, relief)) = found else {
        return Ok((Care::None, ("not_measured", V::List(Vec::new())), source_ids));
    };
    let mut next = record;
    let mut changes = Vec::with_capacity(5);
    let mut changed = false;
    let mut note = |kind: &str, path: &str, before: f64, after: f64| {
        changed |= before != after;
        changes.push(change(kind, agent_id, path, before, after));
    };
    let care = if emotional {
        let (stress_after, trust_after) = (py_max(0.0, stress - relief), py_min(1.0, trust + relief * 0.25));
        next.reassurance = py_min(1.0, record.reassurance + relief);
        next.positive_affect = py_min(1.0, record.positive_affect + relief * 0.5);
        next.negative_affect = py_max(0.0, record.negative_affect - relief * 0.5);
        note(AGENT, "stress", stress, stress_after);
        note(AGENT, "trust", trust, trust_after);
        note(MODEL_STATE, "body_state_v1.agents.reassurance", record.reassurance, next.reassurance);
        note(MODEL_STATE, "body_state_v1.agents.positive_affect", record.positive_affect, next.positive_affect);
        note(MODEL_STATE, "body_state_v1.agents.negative_affect", record.negative_affect, next.negative_affect);
        Care::Emotional { stress: stress_after, trust: trust_after, record: next }
    } else {
        next.hygiene_need = py_max(0.0, record.hygiene_need - relief);
        note(MODEL_STATE, "body_state_v1.agents.hygiene_need", record.hygiene_need, next.hygiene_need);
        Care::Hygiene { record: next }
    };
    if ![stress, trust, relief, record.reassurance, record.positive_affect, record.negative_affect, record.hygiene_need].iter().all(|value| value.is_finite()) {
        return Err(Error::unsupported("a value is not finite"));
    }
    let outcome = (if changed { "observed_change" } else { "observed_no_change" }, V::List(changes));
    source_ids.push(Arc::from(BODY_SOURCE));
    Ok((care, outcome, source_ids))
}

// ---- What a selected care formula does ---------------------------------------------------------------

/// `type(value).__name__` of a value Python's `in` or `iter` refuses.
fn type_name(value: &V) -> &'static str {
    match value {
        V::Null => "NoneType",
        V::Bool(_) => "bool",
        V::Int(_) | V::Big(_) => "int",
        V::Float(_) => "float",
        V::Str(_) => "str",
        V::List(_) => "list",
        V::Map(_) => "dict",
    }
}

/// `needle in value`, as Python answers it for a str needle.
fn python_contains(value: &V, needle: &str) -> Result<bool> {
    match value {
        V::List(items) => Ok(items.iter().any(|item| matches!(item, V::Str(item) if &**item == needle))),
        V::Str(text) => Ok(text.contains(needle)),
        V::Map(map) => Ok(map.contains_key(needle)),
        other => Err(Error::Type(format!("argument of type '{}' is not iterable", type_name(other)))),
    }
}

/// The str items of `iter(value)`, as Python iterates them.
fn python_str_items(value: &V) -> Result<Vec<Arc<str>>> {
    match value {
        V::List(items) => Ok(items.iter().filter_map(|item| if let V::Str(item) = item { Some(item.clone()) } else { None }).collect()),
        V::Str(text) => Ok(text.chars().map(|char| Arc::from(char.to_string())).collect()),
        V::Map(map) => Ok(map.keys().cloned().collect()),
        other => Err(Error::Type(format!("'{}' object is not iterable", type_name(other)))),
    }
}

/// `explicit_care_dependents`.
fn care_dependents(world: &World, caregiver_id: &str, household_id: Option<&str>) -> Result<Vec<Arc<str>>> {
    let Some(V::Map(caregiver)) = person_context(world, caregiver_id)? else {
        return Ok(Vec::new());
    };
    if caregiver.get("requires_review").is_some_and(V::is_truthy) {
        return Ok(Vec::new());
    }
    let Some(V::List(dependents)) = caregiver.get("care_for_ids") else {
        return Ok(Vec::new());
    };
    let Some(V::Str(household)) = caregiver.get("household_id") else {
        return Ok(Vec::new());
    };
    if dependents.is_empty() || household_id.is_some_and(|given| given != &**household) || !caregiver.get("source_ids").is_some_and(V::is_truthy) {
        return Ok(Vec::new());
    }
    let mut linked: Vec<Arc<str>> = Vec::new();
    for dependent_id in dependents {
        let id = kit::exact_str(dependent_id)?;
        let Some(dependent) = person_context(world, id)? else {
            continue;
        };
        if !world.rows()?.agents.contains_key(id) {
            continue;
        }
        let V::Map(dependent) = dependent else {
            continue;
        };
        if dependent.get("requires_review").is_some_and(V::is_truthy) || !matches!(dependent.get("household_id"), Some(V::Str(theirs)) if theirs == household) {
            continue;
        }
        let caregivers = match dependent.get("caregiver_ids") {
            None => false,
            Some(value) => python_contains(value, caregiver_id)?,
        };
        if !caregivers || !dependent.get("source_ids").is_some_and(V::is_truthy) {
            continue;
        }
        linked.push(id.clone());
    }
    linked.sort_unstable();
    linked.dedup();
    Ok(linked)
}

fn body_field(record: &BodyRecord, field: &str) -> Result<f64> {
    match field {
        "reassurance" => Ok(record.reassurance),
        "discontent" => Ok(record.discontent),
        "uncertainty" => Ok(record.uncertainty),
        _ => Err(Error::unsupported("another field of the body")),
    }
}

fn set_body_field(record: &mut BodyRecord, field: &str, value: f64) -> Result<()> {
    match field {
        "reassurance" => record.reassurance = value,
        "discontent" => record.discontent = value,
        "uncertainty" => record.uncertainty = value,
        _ => return Err(Error::unsupported("another field of the body")),
    }
    Ok(())
}

/// A value of the formula as the rule takes it: a refusal its `Unsupported`, any other fault a
/// corrupt `model_configs` (the PyO3 crate's `configs::read`).
fn formula_read<T>(value: crate::config::read::Read<T>) -> Result<T> {
    value.map_err(|fault| {
        if fault.is_refusal() {
            Error::unsupported(fault.text.to_string())
        } else {
            Error::corrupt(MODEL_CONFIGS, "a value of a section holds an error")
        }
    })
}

/// The part of `StaffSupportCompletionHandler.apply` that a selected formula decides, for a kind other
/// than emotional support: what changes, the outcome, and the receipt's sources.
fn formula_care(
    world: &World,
    inputs: &Inputs,
    formula: &CareOutcome,
    agent_id: &Arc<str>,
    kind: &str,
    household_id: Option<&str>,
    stress: f64,
) -> Result<(Care, (&'static str, V), Vec<Arc<str>>)> {
    let hygiene = kind == "hygiene_support";
    let typed = formula_read(formula.deltas(kind, agent_id))?;
    let mut relational: Vec<(Arc<str>, Vec<(&'static str, f64)>)> = Vec::new();
    let mut sources = Vec::new();
    if kind == "caregiver_assistance" {
        for dependent in care_dependents(world, agent_id, household_id)? {
            let deltas = formula_read(formula.deltas("caregiver_assistance", &dependent))?;
            if !deltas.is_empty() {
                relational.push((dependent, deltas));
            }
        }
        let mut identities = vec![agent_id.clone()];
        identities.extend(relational.iter().map(|(id, _)| id.clone()));
        for identity in identities {
            if let Some(V::Map(context)) = person_context(world, &identity)? {
                if let Some(listed) = context.get("source_ids") {
                    sources.extend(python_str_items(listed)?);
                }
            }
        }
        sources.sort_unstable();
        sources.dedup();
    }
    let mut source_ids = vec![Arc::from(STAFF_SUPPORT_SOURCE)];
    source_ids.extend(sources);
    let measured_kind = hygiene || !typed.is_empty();
    let not_measured = || ("not_measured", V::List(Vec::new()));
    if !inputs.body_present {
        if !relational.is_empty() {
            return Err(Error::unsupported("a dependent without a body record"));
        }
        return Ok((Care::None, not_measured(), source_ids));
    }
    // The people as they are, each once; sequential updates read each other's results.
    let mut order: Vec<Arc<str>> = vec![agent_id.clone()];
    for (dependent, _) in &relational {
        if !order.contains(dependent) {
            order.push(dependent.clone());
        }
    }
    let (records, own_has_body, relief) = {
        let row = world.row::<BodyRow>(BODY)?;
        let records: Vec<Option<BodyRecord>> = order.iter().map(|id| row.agents.get(&**id).copied()).collect();
        let relief = config_float(&row.config, "hygiene_support_relief_by_agent", "hygiene_support_relief", agent_id)?;
        (records, row.agents.get(&**agent_id).is_some(), relief)
    };
    if !own_has_body {
        if !relational.is_empty() {
            return Err(Error::unsupported("a dependent is measured without the person's body"));
        }
        return Ok((Care::None, not_measured(), source_ids));
    }
    let mut people: Vec<CaredPerson> = Vec::with_capacity(order.len());
    for (id, record) in order.iter().zip(records) {
        let Some(record) = record else {
            return Err(Error::unsupported("a dependent without a body record"));
        };
        people.push(CaredPerson { agent_id: id.clone(), stress: None, record });
    }
    let index_of = |id: &Arc<str>| order.iter().position(|known| known == id).expect("listed above");
    let originals: Vec<BodyRecord> = people.iter().map(|person| person.record).collect();
    let mut stresses: Vec<f64> = Vec::with_capacity(order.len());
    stresses.push(stress);
    {
        let rows = world.rows()?;
        for id in &order[1..] {
            let Some(row) = rows.agents.get(id) else {
                return Err(Error::unsupported("no such person"));
            };
            stresses.push(row.stress);
        }
    }
    let original_stresses = stresses.clone();
    // What the Python rule records before anything changes: (kind, person, field).
    let mut before: Vec<(bool, usize, &'static str)> = Vec::new();
    if measured_kind {
        if hygiene {
            before.push((false, 0, "hygiene_need"));
        }
        for (field, _) in &typed {
            before.push((*field == "stress", 0, field));
        }
    }
    for (dependent, deltas) in &relational {
        let index = index_of(dependent);
        for (field, _) in deltas {
            before.push((*field == "stress", index, field));
        }
    }
    // The changes, in the order of the Python rule.
    let mut outcome_source = false;
    if hygiene {
        people[0].record.hygiene_need = formula.hygiene_after(people[0].record.hygiene_need, relief);
        outcome_source = true;
    } else if !typed.is_empty() {
        for (field, delta) in &typed {
            if *field == "stress" {
                stresses[0] = py_min(1.0, py_max(0.0, stresses[0] + delta));
            } else {
                let next = py_min(1.0, py_max(0.0, body_field(&people[0].record, field)? + delta));
                set_body_field(&mut people[0].record, field, next)?;
            }
        }
        outcome_source = true;
    }
    for (dependent, deltas) in &relational {
        let index = index_of(dependent);
        for (field, delta) in deltas {
            if *field == "stress" {
                stresses[index] = py_min(1.0, py_max(0.0, stresses[index] + delta));
            } else {
                let next = py_min(1.0, py_max(0.0, body_field(&people[index].record, field)? + delta));
                set_body_field(&mut people[index].record, field, next)?;
            }
        }
        outcome_source = true;
    }
    if !measured_kind && relational.is_empty() {
        return Ok((Care::None, not_measured(), source_ids));
    }
    let mut changes = Vec::with_capacity(before.len());
    let mut changed = false;
    for (is_stress, index, field) in &before {
        let id = &order[*index];
        let (kind_name, path, was, now) = if *is_stress {
            (AGENT, "stress".to_owned(), original_stresses[*index], stresses[*index])
        } else if *field == "hygiene_need" {
            (MODEL_STATE, "body_state_v1.agents.hygiene_need".to_owned(), originals[*index].hygiene_need, people[*index].record.hygiene_need)
        } else {
            (MODEL_STATE, format!("body_state_v1.agents.{field}"), body_field(&originals[*index], field)?, body_field(&people[*index].record, field)?)
        };
        if !was.is_finite() || !now.is_finite() {
            return Err(Error::unsupported("a value is not finite"));
        }
        changed |= was != now;
        changes.push(change(kind_name, id, &path, was, now));
    }
    if changes.len() > 32 {
        return Err(Error::unsupported("more changes than a receipt holds"));
    }
    for (index, person) in people.iter_mut().enumerate() {
        if stresses[index] != original_stresses[index] {
            person.stress = Some(stresses[index]);
        }
    }
    source_ids.push(Arc::from(BODY_SOURCE));
    if outcome_source {
        source_ids.push(formula.source_id.clone());
    }
    Ok((Care::Formula { people }, (if changed { "observed_change" } else { "observed_no_change" }, V::List(changes)), source_ids))
}

// ---- A request, for `seek_help` and `seek_information` -----------------------------------------------

/// What `StaffService.request` reads of the service, each None where reading it raised.
#[derive(Clone, Debug, Default)]
pub struct RequestService {
    pub unresolved_after_minutes: Option<i64>,
    pub cooldown_minutes: Option<i64>,
    pub policy_version: Option<Arc<str>>,
}

fn service_value<T: Clone>(value: &Option<T>, unread: &'static str) -> Result<T> {
    value.clone().ok_or_else(|| Error::unsupported(unread))
}

/// What `StaffService.request` will do, with nothing changed.
pub enum PlannedRequest {
    /// It raises `ValueError`: `support_request_rejected_count` is the step's answer.
    Rejected,
    Opens { request_id: Arc<str>, timeout_minute: i64, event: EventRow },
    /// Asked in a Run's last minute: unresolved at once, with a cooldown until this minute.
    Closes { request_id: Arc<str>, until: i64 },
}

/// The ID of a person's request at a minute: `support-` and the first 12 bytes of the SHA-256 of
/// the person's ID, a NUL and the minute, in hex.
pub fn request_id_of(agent_id: &str, minute: i64) -> Arc<str> {
    let mut hash = Sha256::new();
    hash.update(agent_id.as_bytes());
    hash.update([0]);
    hash.update(minute.to_string().as_bytes());
    let digest = hash.finalize();
    let mut text = String::with_capacity(8 + 24);
    text.push_str("support-");
    for byte in &digest[..12] {
        text.push(char::from_digit(u32::from(byte >> 4), 16).expect("a digit"));
        text.push(char::from_digit(u32::from(byte & 15), 16).expect("a digit"));
    }
    Arc::from(text)
}

/// `StaffService.request`, without the world's whole validation, with `reserved` sequences taken
/// before it by the caller's plan.
pub fn plan_request(world: &World, service: &RequestService, reserved: usize, agent_id: &str, minute: i64) -> Result<PlannedRequest> {
    let events = Events::of(world)?;
    events.reserve(reserved);
    let request_id = request_id_of(agent_id, minute);
    let eligible = {
        let rows = world.rows()?;
        let Some(agent) = rows.agents.get(agent_id) else {
            return Err(Error::unsupported("no such agent"));
        };
        minute >= agent.support_cooldown_until
            && agent.support_request_id.is_none()
            && matches!(agent.activity, Activity::Idle | Activity::Resting | Activity::Sleeping)
            && rows.support_requests.get(&request_id).is_none()
    };
    if !eligible {
        return Ok(PlannedRequest::Rejected);
    }
    let after = service_value(&service.unresolved_after_minutes, AFTER_UNREAD)?;
    let Some(after_end) = minute.checked_add(after) else {
        return Err(Error::unsupported("a minute out of range"));
    };
    let timeout_minute = after_end.min(events.duration() - 1);
    if timeout_minute == minute {
        let cooldown = service_value(&service.cooldown_minutes, COOLDOWN_UNREAD)?;
        let Some(until) = minute.checked_add(cooldown) else {
            return Err(Error::unsupported("a minute out of range"));
        };
        return Ok(PlannedRequest::Closes { request_id, until });
    }
    let sequence = events.next_sequence()?;
    let event_id: Arc<str> = Arc::from(format!("support-timeout-{sequence}"));
    if events.is_taken(&event_id, Some(sequence))? {
        return Err(Error::unsupported("the event's identity is taken"));
    }
    let policy = service_value(&service.policy_version, POLICY_UNREAD)?;
    let mut payload = IndexMap::with_capacity(2);
    payload.insert(Arc::from("request_id"), V::Str(request_id.clone()));
    payload.insert(Arc::from("agent_id"), V::Str(Arc::from(agent_id)));
    let mut provenance = IndexMap::with_capacity(4);
    provenance.insert(Arc::from("source_type"), V::Str(Arc::from("assumption")));
    provenance.insert(Arc::from("source_id"), V::Str(Arc::from(STAFF_SUPPORT_SOURCE)));
    provenance.insert(Arc::from("policy_version"), V::Str(policy));
    provenance.insert(Arc::from("timeout_minutes"), V::Int(after));
    let event = kit::new_event(kit::NewEvent {
        event_id,
        kind: EventKind::Completion,
        due_minute: timeout_minute,
        priority: 20,
        sequence,
        event_type: Arc::from("support_request_timeout"),
        payload,
        source_id: Some(request_id.clone()),
        causation_id: None,
        provenance,
    })?;
    Ok(PlannedRequest::Opens { request_id, timeout_minute, event })
}

/// The writes of `StaffService.request` for a request that opens.
pub fn write_request(world: &World, agent_id: &str, minute: i64, request_id: &Arc<str>, timeout_minute: i64, event: EventRow) -> WriteResult<()> {
    let events = Events::of(world)?;
    {
        let mut rows = world.rows_mut()?;
        if !rows.agents.contains_key(agent_id) {
            return Err(Error::Runtime("a row has no object".to_owned()).into());
        }
        rows.support_requests.insert(request_id.clone(), new_request(request_id, agent_id, minute, timeout_minute));
        set_agent(&mut rows, agent_id, Activity::SeekingHelp, None)?;
        let Some(row) = rows.agents.get_mut(agent_id) else {
            return Err(Error::unsupported("no such agent").into());
        };
        row.support_request_id = Some(request_id.clone());
    }
    events.allocate_sequence()?;
    events.schedule(event)
}

/// The writes of `StaffService.request` for a request closed at once. The person stays as they are.
pub fn write_closed_request(world: &World, metric_state_held: bool, agent_id: &str, minute: i64, request_id: &Arc<str>, until: i64) -> Result<()> {
    check_metric_state(world, metric_state_held)?;
    {
        let mut rows = world.rows_mut()?;
        if !rows.agents.contains_key(agent_id) {
            return Err(Error::Runtime("a row has no object".to_owned()));
        }
        let request = RequestRow { status: RequestStatus::Unresolved, resolved_minute: Some(minute), cooldown_until: until, ..new_request(request_id, agent_id, minute, minute) };
        rows.support_requests.insert(request_id.clone(), request);
        let Some(row) = rows.agents.get_mut(agent_id) else {
            return Err(Error::unsupported("no such agent"));
        };
        row.support_cooldown_until = until;
    }
    add_metric(world, UNRESOLVED_METRIC)
}

/// `support_kind_for_agent`.
pub fn support_kind_of(world: &World, agent_id: &str) -> Result<Arc<str>> {
    let default = || Arc::<str>::from(EMOTIONAL_SUPPORT);
    let Some(V::Map(person)) = person_context(world, agent_id)? else {
        return Ok(default());
    };
    let log = world.log()?;
    let completed = log.completed_kinds(agent_id);
    let done = |kind: &str| completed.iter().any(|known| known == kind);
    if let Some(V::List(items)) = person.get("care_support_needs") {
        for item in items {
            if let V::Str(kind) = item {
                if !done(kind) {
                    return Ok(kind.clone());
                }
            }
        }
    }
    let is_true = |key: &str| matches!(person.get(key), Some(V::Bool(true)));
    if is_true("medication_support") && !done("medication_support") {
        return Ok(Arc::from("medication_support"));
    }
    if is_true("medical_support") && !done("medical_assessment") {
        return Ok(Arc::from("medical_assessment"));
    }
    Ok(default())
}
