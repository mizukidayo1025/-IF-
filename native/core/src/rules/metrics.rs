//! The KPI columns of a minute (`MetricSnapshot` of `experiments/metrics.py`), read from the rows of
//! a `World`. The pilot of the kernels that move to the core (Issue #152, lane 5-4): a pure function
//! over `&World`, with what is still Python's handed in by the PyO3 crate (`rules/metrics.rs` of
//! `native/src`), which also makes the Python values of the result.
//!
//! Handed in, because Python holds it or computes it:
//! - what the staff rules read of the Python world (`staff::Inputs`), and the routes' inputs where
//!   a wait's routes are read (`staff::WaitRoutes`): the open requests' waits and reasons are the
//!   staff rules' `open_waits` over the rows (lane P8 b4), and `sample` answers None where they
//!   need the routes, which the PyO3 crate then reads before it calls again;
//! - which requests: the ones a caller listed, or those `world.unsettled_support_requests()` gives,
//!   which the memo of the last end-of-tick check knows (`after_tick::Memo`) or else all;
//! - whether `world.metric_state` holds its slot's tree (a field assigned whole doesn't).
//!
//! `capture` is the metrics phase of a tick: the sample, given to the host's sink
//! (`Host::capture_metric`), which makes the `MetricSnapshot` at the edge.
//!
//! A facility kind is folded here with the core's `str.casefold` (`text::casefold`, P8-0a), which
//! before was Python's for a kind that isn't ASCII. The realized events of the minute come from the world's log (`World::log`, lane 6-5), which is
//! Unsupported while the world's EventLog is not held there.
//!
//! The sums go over `world.agents` in insertion order with `py_sum`, so every float has the bits of
//! the Python rule. The ID lists and the JSON are written by byte order, which is the code point
//! order of Python's `sorted` and of `canonical_json`. The errors are refusals (`Unsupported`) with
//! the texts the PyO3 kernel had: a tainted row, a staff queue naming no task (a KeyError of the
//! Python rule).

use std::sync::Arc;

use crate::error::{Error, Result};
use crate::host::Host;
use crate::json::write_json_string;
use crate::mathx::py_sum;
use crate::rules::after_tick::Memo;
use crate::rules::kit::{is_toilet, Events};
use crate::rules::staff::{self, WaitRoutes};
use crate::world::{World, WorldCell};

/// `agent.stress >= 0.7` is high stress.
pub const HIGH_STRESS_AT_LEAST: f64 = 0.7;
/// `support_wait_exposure_semantics` of the source IDs.
pub const EXPOSURE_SEMANTICS: &str = "per-minute observed request exposure by blocker; reason totals may overlap \
                                      and are not exclusive causal attribution";
/// The owned world field the counters are read from.
pub use crate::world::METRIC_STATE;

/// What the metrics read of one open (pending or assigned) support request, in the order of
/// `world.unsettled_support_requests()`.
#[derive(Clone, Debug)]
struct OpenWait<'a> {
    pub agent: &'a str,
    pub request_id: &'a str,
    /// `support_waited_minutes` at the sample minute.
    pub waited: i64,
    /// `support_wait_reason_codes`, in their order.
    pub reasons: Vec<&'a str>,
    /// Pending, and no staff member has the skills its kind needs.
    pub without_qualified_staff: bool,
}

/// The support requests a sample reads.
#[derive(Clone, Copy)]
pub enum Requests<'a> {
    /// These, in this order (a caller's list of `MetricSnapshot.from_world`).
    Given(&'a [Arc<str>]),
    /// `world.unsettled_support_requests()`: what the memo of the world's last end-of-tick check
    /// says may be open, or else all of them, in the order of the table.
    Unsettled(Option<&'a Memo>),
}

/// What `sample` takes besides the world.
pub struct Inputs<'a> {
    /// The minute sampled, the world's.
    pub sample: i64,
    pub requests: Requests<'a>,
    /// What the staff rules read of the Python world; its `metric_state_held` is the sample's too.
    pub staff: &'a staff::Inputs<'a>,
    /// The routes' inputs, where a wait's routes are read: None for a first call.
    pub routes: Option<&'a WaitRoutes<'a>>,
}

/// What `capture` did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Captured {
    /// The sample went to the host's sink.
    Taken,
    /// The waits need their routes (`Inputs::routes`): nothing was sampled.
    NeedsRoutes,
}

/// The metrics phase of a tick: the sample of the world's minute, given to the host's sink. The
/// world is not borrowed while the host runs (R6).
pub fn capture<C: WorldCell, H: Host>(cell: &C, host: &mut H, inputs: &Inputs<'_>) -> std::result::Result<Captured, H::Error> {
    let taken = cell.with(|world| sample(world, inputs))??;
    let Some(taken) = taken else {
        return Ok(Captured::NeedsRoutes);
    };
    host.capture_metric(&taken, inputs.sample)?;
    Ok(Captured::Taken)
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

/// `MetricSnapshot.from_world` after its `minute_index`, for a sample minute that is the world's:
/// None where the waits need their routes and `inputs.routes` is None. It only reads.
pub fn sample(world: &World, inputs: &Inputs<'_>) -> Result<Option<MetricSample>> {
    let sample = inputs.sample;
    let all: Vec<Arc<str>>;
    let ids: &[Arc<str>] = match inputs.requests {
        Requests::Given(ids) => ids,
        Requests::Unsettled(memo) => {
            all = match memo.and_then(|memo| memo.unsettled_requests(world).ok().flatten()) {
                Some(keys) => keys,
                None => world.rows()?.support_requests.slots().map(|(key, _)| key.clone()).collect(),
            };
            &all
        }
    };
    let Some(open) = staff::open_waits(world, inputs.staff, inputs.routes, ids, sample, true)? else {
        return Ok(None);
    };
    let waits: Vec<OpenWait<'_>> = open
        .requests
        .iter()
        .map(|request| OpenWait {
            agent: &request.agent,
            request_id: &ids[request.index],
            waited: request.waited,
            reasons: open.reasons_of(request),
            without_qualified_staff: open.has_no_qualified_staff(request),
        })
        .collect();
    // The refusals of a world whose events a rule cannot write, as before.
    Events::of(world)?;
    let metric_state_held = inputs.staff.metric_state_held;
    let mut queued: Vec<Arc<str>> = Vec::new();
    let mut active: Vec<Arc<str>> = Vec::new();
    let mut high_stress: Vec<Arc<str>> = Vec::new();
    let mut staff_tasks: Vec<Arc<str>> = Vec::new();
    let mut busy_staff: Vec<Arc<str>> = Vec::new();
    let (working_units, stress_mean, staff_count) = {
        let rows = world.rows()?;
        let mut working: i64 = 0;
        for (_, facility) in rows.facilities.iter() {
            if is_toilet(&facility.kind) {
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
    let mut unresolved: Vec<&str> = waits.iter().map(|wait| wait.agent).collect();
    unresolved.sort_unstable();
    let mut no_qualified: Vec<&str> = waits.iter().filter(|wait| wait.without_qualified_staff).map(|wait| wait.agent).collect();
    no_qualified.sort_unstable();
    let log = world.log()?;
    let mut events: Vec<&str> = log.event_ids_at(sample).collect();
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
    write_waits(&mut out, &waits);
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

    if world.slots.get(METRIC_STATE).is_some() && !metric_state_held {
        return Err(Error::unsupported("the field holds another dict"));
    }
    let row = world.metric_state()?;
    let read = |name: &str| row.get(name).copied().unwrap_or(0.0);
    let count = read("toilet_wait_count");
    let total = read("toilet_wait_total_minutes");
    Ok(Some(MetricSample {
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
    }))
}

// ---- `operational_kpi_values` (lane P10-1c) ---------------------------------------------------------

/// The names of a service's waits in `metric_state`: the count, the total, the mean and the
/// longest.
pub const KPI_WAITS: [(&str, &str, &str, &str); 2] = [
    ("water_service_wait_count", "water_service_wait_total_minutes", "water_service_wait_mean_minutes", "water_service_wait_max_minutes"),
    ("meal_service_wait_count", "meal_service_wait_total_minutes", "meal_service_wait_mean_minutes", "meal_service_wait_max_minutes"),
];

/// What `operational_kpi_values` reads of the rows, each part with what its read refused, in the
/// order the rule reads them: the people's needs and the facilities' queues, then the ledger's
/// reserved amounts, then the waits and stalls of `metric_state`.
pub struct KpiRows {
    /// The means of `toilet_urgency`, `hunger` and `hydration_need` over `world.agents` in their
    /// order (`py_sum`), or None for no one (the Python rule's mean divides by zero).
    pub means: Option<[f64; 3]>,
    /// The water and meal queues' lengths, then their active services.
    pub queues: [usize; 4],
    /// `reserved` of the water and the meal accounts.
    pub reserved: Result<[i64; 2]>,
    /// Each service's `(count, total, longest)` wait, the outage stalls and the stock stalls.
    pub metrics: Result<([(f64, f64, f64); 2], f64, f64)>,
}

/// `operational_kpi_values`' reads of the rows. `ledger` is whether the world holds the ledger's
/// namespace, and `metric_state_held` whether `world.metric_state` is its slot's tree.
pub fn kpi_rows(world: &World, ledger: &crate::rules::kit::Held, metric_state_held: bool) -> Result<KpiRows> {
    use crate::rules::kit::Held;
    let (means, queues) = {
        let rows = world.rows()?;
        let count = rows.agents.len();
        let means = (count != 0).then(|| {
            let mean = |need: fn(&crate::entities::NeedRow) -> f64| {
                let values: Vec<f64> = rows.agents.iter().map(|(_, agent)| need(&agent.needs)).collect();
                py_sum(&values) / count as f64
            };
            [mean(|needs| needs.toilet_urgency), mean(|needs| needs.hunger), mean(|needs| needs.hydration_need)]
        });
        let mut queues = [0; 4];
        for (_, facility) in rows.facilities.iter() {
            let (queue, active) = match &*facility.kind {
                "water_station" => (0, 2),
                "meal_distribution" => (1, 3),
                _ => continue,
            };
            queues[queue] += facility.resource_queue_agent_ids.len();
            queues[active] += facility.resource_active_agent_ids.len();
        }
        (means, queues)
    };
    let reserved = match ledger {
        Held::Held => world.row::<crate::state::resource_ledger::LedgerRow>(crate::state::resource_ledger::NAME).map(|row| [row.accounts.of(0).reserved, row.accounts.of(1).reserved]),
        Held::Absent => Err(Error::unsupported("the world holds another ledger")),
        Held::Other(reason) => Err(Error::unsupported(*reason)),
    };
    let metrics = if !metric_state_held {
        Err(Error::unsupported("the field holds another dict"))
    } else {
        world.metric_state().map(|row| {
            let read = |name: &str| row.get(name).copied().unwrap_or(0.0);
            let wait = |names: &(&str, &str, &str, &str)| (read(names.0), read(names.1), read(names.3));
            ([wait(&KPI_WAITS[0]), wait(&KPI_WAITS[1])], read("resource_service_wait_water_outage_count"), read("resource_service_wait_stock_count"))
        })
    };
    Ok(KpiRows { means, queues, reserved, metrics })
}
