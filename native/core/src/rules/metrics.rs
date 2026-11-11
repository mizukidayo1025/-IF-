//! The KPI columns of a minute (`MetricSnapshot` of `experiments/metrics.py`), read from the rows of
//! a `World`. The pilot of the kernels that move to the core (Issue #152, lane 5-4): a pure function
//! over `&World`, with what is still Python's handed in by the PyO3 crate (`rules/metrics.rs` of
//! `native/src`), which also makes the Python values of the result.
//!
//! Handed in, because Python holds it or computes it:
//! - the open support requests and their wait reasons (`OpenWait`), which `rules/staff.rs` reads
//!   from the Python objects of the unsettled requests and the routes;
//! - the event log (`EventLog`), held by the Python `EventStore` until lane 6-5;
//! - `str.casefold` of a facility kind that isn't ASCII (`Folds`): Python's fold, which Rust has no
//!   table of. An ASCII kind is folded here, as ASCII lower case is what `casefold` gives it;
//! - whether `world.metric_state` holds its slot's tree (a field assigned whole doesn't).
//!
//! The sums go over `world.agents` in insertion order with `py_sum`, so every float has the bits of
//! the Python rule. The ID lists and the JSON are written by byte order, which is the code point
//! order of Python's `sorted` and of `canonical_json`. The errors are refusals (`Unsupported`) with
//! the texts the PyO3 kernel had: a tainted row, a staff queue naming no task (a KeyError of the
//! Python rule).

use std::sync::Arc;

use crate::error::{Error, Result};
use crate::events::log::EventLog;
use crate::json::write_json_string;
use crate::mathx::py_sum;
use crate::world::World;

/// `agent.stress >= 0.7` is high stress.
pub const HIGH_STRESS_AT_LEAST: f64 = 0.7;
/// `support_wait_exposure_semantics` of the source IDs.
pub const EXPOSURE_SEMANTICS: &str = "per-minute observed request exposure by blocker; reason totals may overlap \
                                      and are not exclusive causal attribution";
/// The owned world field the counters are read from.
pub use crate::world::METRIC_STATE;

/// `"toilet" in kind.casefold()` of the facility kinds that aren't ASCII, which the caller folded
/// with Python's `str.casefold`.
#[derive(Default, Debug)]
pub struct Folds {
    kinds: Vec<(Arc<str>, bool)>,
}

impl Folds {
    /// Say whether `"toilet"` is in the casefolded kind.
    pub fn insert(&mut self, kind: Arc<str>, toilet: bool) {
        if self.get(&kind).is_none() {
            self.kinds.push((kind, toilet));
        }
    }

    fn get(&self, kind: &str) -> Option<bool> {
        self.kinds.iter().find(|(known, _)| &**known == kind).map(|(_, toilet)| *toilet)
    }
}

/// `"toilet" in kind.casefold()`: here for an ASCII kind, from `folds` for any other.
#[track_caller]
pub fn is_toilet(kind: &str, folds: &Folds) -> Result<bool> {
    if kind.is_ascii() {
        return Ok(kind.as_bytes().windows(6).any(|part| part.eq_ignore_ascii_case(b"toilet")));
    }
    folds.get(kind).ok_or_else(|| Error::unsupported("a facility kind was not folded"))
}

/// The facility kinds that aren't ASCII and aren't in `folds`, in the facilities' order without
/// repeats: what the caller folds before `sample`. None for a tainted store, which `sample` refuses.
pub fn unfolded_kinds(world: &World, folds: &Folds) -> Result<Vec<Arc<str>>> {
    let entities = world.entities.try_borrow()?;
    if entities.tainted().is_some() {
        return Ok(Vec::new());
    }
    let mut kinds: Vec<Arc<str>> = Vec::new();
    for (_, facility) in entities.facilities.iter() {
        let kind = &facility.kind;
        if !kind.is_ascii() && folds.get(kind).is_none() && !kinds.iter().any(|known| known == kind) {
            kinds.push(kind.clone());
        }
    }
    Ok(kinds)
}

/// What the metrics read of one open (pending or assigned) support request, in the order of
/// `world.unsettled_support_requests()`.
#[derive(Clone, Debug)]
pub struct OpenWait<'a> {
    pub agent: &'a str,
    pub request_id: &'a str,
    /// `support_waited_minutes` at the sample minute.
    pub waited: i64,
    /// `support_wait_reason_codes`, in their order.
    pub reasons: Vec<&'a str>,
    /// Pending, and no staff member has the skills its kind needs.
    pub without_qualified_staff: bool,
}

/// What `sample` takes besides the world.
pub struct Inputs<'a> {
    /// The minute sampled, the world's.
    pub sample: i64,
    pub waits: &'a [OpenWait<'a>],
    pub log: &'a EventLog,
    pub folds: &'a Folds,
    /// Whether `world.metric_state` is its slot's tree. Not owned: either.
    pub metric_state_held: bool,
}

/// The fields of `MetricSnapshot` after its `minute_index`, in the order of the dataclass.
#[derive(Clone, Debug, PartialEq)]
pub struct MetricSample {
    pub toilet_queue_length: usize,
    pub toilet_active_users: usize,
    pub toilet_working_units: i64,
    pub toilet_wait_count: f64,
    pub toilet_wait_total_minutes: f64,
    pub toilet_wait_mean_minutes: f64,
    pub toilet_wait_max_minutes: f64,
    pub stress_mean: f64,
    pub stress_high_count: usize,
    pub staff_assigned_task_count: usize,
    pub staff_busy_count: usize,
    pub staff_load_ratio: f64,
    pub unresolved_support_request_count: usize,
    /// The canonical JSON of the sorted IDs behind the counts.
    pub source_ids_json: String,
    pub support_no_qualified_worker_count: usize,
    pub support_no_qualified_worker_request_minutes: f64,
    pub support_fairness_promoted_assignment_count: f64,
    pub support_wait_staff_busy_request_minutes: f64,
    pub support_wait_staff_off_shift_request_minutes: f64,
}

fn write_ids(out: &mut Vec<u8>, ids: &[impl AsRef<str>]) {
    out.push(b'[');
    for (index, id) in ids.iter().enumerate() {
        if index > 0 {
            out.push(b',');
        }
        write_json_string(out, id.as_ref());
    }
    out.push(b']');
}

fn write_key(out: &mut Vec<u8>, key: &str, first: bool) {
    if !first {
        out.push(b',');
    }
    write_json_string(out, key);
    out.push(b':');
}

/// `support_wait_by_agent`, by agent: a later request of the same agent replaces an earlier one, as
/// in the dict the Python rule fills.
fn write_waits(out: &mut Vec<u8>, waits: &[OpenWait<'_>]) {
    let mut order: Vec<usize> = (0..waits.len()).collect();
    order.sort_by(|first, second| waits[*first].agent.as_bytes().cmp(waits[*second].agent.as_bytes()));
    out.push(b'{');
    let mut first = true;
    for (place, index) in order.iter().enumerate() {
        let wait = &waits[*index];
        if order.get(place + 1).is_some_and(|next| waits[*next].agent == wait.agent) {
            continue;
        }
        write_key(out, wait.agent, first);
        first = false;
        out.extend_from_slice(b"{\"reason_codes\":");
        write_ids(out, &wait.reasons);
        out.extend_from_slice(b",\"request_id\":");
        write_json_string(out, wait.request_id);
        out.extend_from_slice(b",\"waited_minutes\":");
        out.extend_from_slice(itoa::Buffer::new().format(wait.waited).as_bytes());
        out.push(b'}');
    }
    out.push(b'}');
}

/// `MetricSnapshot.from_world` after its `minute_index`, for a sample minute that is the world's.
/// It only reads.
pub fn sample(world: &World, inputs: &Inputs<'_>) -> Result<MetricSample> {
    let sample = inputs.sample;
    let mut queued: Vec<Arc<str>> = Vec::new();
    let mut active: Vec<Arc<str>> = Vec::new();
    let mut high_stress: Vec<Arc<str>> = Vec::new();
    let mut staff_tasks: Vec<Arc<str>> = Vec::new();
    let mut busy_staff: Vec<Arc<str>> = Vec::new();
    let (working_units, stress_mean, staff_count) = {
        let rows = world.rows()?;
        let mut working: i64 = 0;
        for (_, facility) in rows.facilities.iter() {
            if is_toilet(&facility.kind, inputs.folds)? {
                queued.extend(facility.queue_agent_ids.iter().cloned());
                active.extend(facility.active_agent_ids.iter().cloned());
                let Some(total) = working.checked_add(facility.working_units) else {
                    return Err(Error::unsupported("the working units do not fit an i64"));
                };
                working = total;
            }
        }
        let mut stresses = Vec::with_capacity(rows.agents.len());
        for (_, agent) in rows.agents.iter() {
            stresses.push(agent.stress);
            if agent.stress >= HIGH_STRESS_AT_LEAST {
                high_stress.push(agent.profile.id.clone());
            }
        }
        let mean = if stresses.is_empty() { 0.0 } else { py_sum(&stresses) / stresses.len() as f64 };
        for (_, member) in rows.staff.iter() {
            let mut busy = false;
            for task_id in &member.work_queue {
                staff_tasks.push(task_id.clone());
                let Some(task) = rows.active_tasks.get(task_id) else {
                    return Err(Error::unsupported("a staff queue names no task"));
                };
                busy |= task.start_minute <= sample && sample < task.due_minute;
            }
            if busy {
                busy_staff.push(member.id.clone());
            }
        }
        (working, mean, rows.staff.len())
    };
    queued.sort_unstable();
    active.sort_unstable();
    high_stress.sort_unstable();
    staff_tasks.sort_unstable();
    busy_staff.sort_unstable();
    let mut unresolved: Vec<&str> = inputs.waits.iter().map(|wait| wait.agent).collect();
    unresolved.sort_unstable();
    let mut no_qualified: Vec<&str> =
        inputs.waits.iter().filter(|wait| wait.without_qualified_staff).map(|wait| wait.agent).collect();
    no_qualified.sort_unstable();
    let mut events: Vec<&str> = inputs.log.event_ids_at(sample).collect();
    events.sort_unstable();

    // Keys by their bytes, as canonical_json writes them.
    let mut out: Vec<u8> = Vec::with_capacity(1024);
    out.push(b'{');
    write_key(&mut out, "busy_staff", true);
    write_ids(&mut out, &busy_staff);
    write_key(&mut out, "high_stress_agents", false);
    write_ids(&mut out, &high_stress);
    write_key(&mut out, "realized_events", false);
    write_ids(&mut out, &events);
    write_key(&mut out, "staff_tasks", false);
    write_ids(&mut out, &staff_tasks);
    write_key(&mut out, "support_requests_without_qualified_staff", false);
    write_ids(&mut out, &no_qualified);
    write_key(&mut out, "support_wait_by_agent", false);
    write_waits(&mut out, inputs.waits);
    write_key(&mut out, "support_wait_exposure_semantics", false);
    write_json_string(&mut out, EXPOSURE_SEMANTICS);
    write_key(&mut out, "toilet_active_agents", false);
    write_ids(&mut out, &active);
    write_key(&mut out, "toilet_queue_agents", false);
    write_ids(&mut out, &queued);
    write_key(&mut out, "unresolved_support_agents", false);
    write_ids(&mut out, &unresolved);
    out.push(b'}');
    // Only str went in, and write_json_string writes UTF-8.
    let source_ids_json = String::from_utf8(out).map_err(|_| Error::unsupported("the source IDs are not UTF-8"))?;

    if world.slots.get(METRIC_STATE).is_some() && !inputs.metric_state_held {
        return Err(Error::unsupported("the field holds another dict"));
    }
    let row = world.metric_state()?;
    let read = |name: &str| row.get(name).copied().unwrap_or(0.0);
    let count = read("toilet_wait_count");
    let total = read("toilet_wait_total_minutes");
    Ok(MetricSample {
        toilet_queue_length: queued.len(),
        toilet_active_users: active.len(),
        toilet_working_units: working_units,
        toilet_wait_count: count,
        toilet_wait_total_minutes: total,
        toilet_wait_mean_minutes: if count != 0.0 { total / count } else { 0.0 },
        toilet_wait_max_minutes: read("toilet_wait_max_minutes"),
        stress_mean,
        stress_high_count: high_stress.len(),
        staff_assigned_task_count: staff_tasks.len(),
        staff_busy_count: busy_staff.len(),
        staff_load_ratio: if staff_count == 0 { 0.0 } else { busy_staff.len() as f64 / staff_count as f64 },
        unresolved_support_request_count: unresolved.len(),
        source_ids_json,
        support_no_qualified_worker_count: no_qualified.len(),
        support_no_qualified_worker_request_minutes: read("support_no_qualified_worker_request_minutes"),
        support_fairness_promoted_assignment_count: read("support_fairness_promoted_assignment_count"),
        support_wait_staff_busy_request_minutes: read("support_wait_staff_busy_request_minutes"),
        support_wait_staff_off_shift_request_minutes: read("support_wait_staff_off_shift_request_minutes"),
    })
}
