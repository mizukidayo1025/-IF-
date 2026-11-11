//! Each person's psychology in the update phase (lane P8 a3): the new values of a minute (the five
//! built-in models of `policies/psychology.py` and `BoundedStressPolicy` of
//! `policies/alternatives.py`), the distress monitor (`domain/distress_monitor.py`), the long-term
//! distress (`domain/psychological_state.py`) and writing the new values. The entity fields come
//! from the rows, and the zone's conditions, the body record, the group and what a person heard
//! from the core's view (`rules/world_view.rs`).
//!
//! The model and its rates are what the Run read of `controller.psychology` (`run::psychology`,
//! read once per controller): `model_of`, `params`, `keeps_long_term` and `long_term_of` turn them
//! into the rules' numbers with the refusals of the kernel before. A read that raised in Python is
//! a `PolicyFault` with the path it was read by, which the PyO3 crate raises again by reading that
//! path (policy E14).
//!
//! These steps have no Python rule left (lane L10): the Python of `explain_update`,
//! `deltas_without_traces`, `update_psychological_state`, `record_distress_monitor_tick` and
//! `apply_state_deltas` that the comments below name was deleted, and the rules keep its results bit
//! for bit. A step changes nothing until it knows its checks pass: everything is computed from the
//! rows first, and any value the deleted rules turned down, or that the rules do not handle, is
//! Unsupported. Then the plan is written to the rows; their attached trees follow when the world is
//! given back (lane P8-0c). Every float operation keeps the order of the Python expression.
//!
//! What a minute gives (`PsychologyResult`: a `Delta` per person and, with a trace, the `Traces`)
//! is a value of the tick, which a Run keeps in its `Scratch`. The PyO3 crate makes the Python
//! `AgentStateDelta`s of it at the edge, for the Python steps that still take them.

use std::sync::Arc;

use indexmap::IndexMap;
use rustc_hash::{FxHashMap, FxHashSet};

use crate::config::{Fault, FaultClass};
use crate::entities::{Activity, RequestStatus};
use crate::error::{Error, Result};
use crate::mathx::{self, py_max, py_min, py_sum};
use crate::rules::trace::{PersonTrace, Traces};
use crate::rules::world_view::{ViewAgent, ViewData, ViewZone};
use crate::run::held::{Held, Obj, NOT_AN_OBJECT, NOT_READ};
use crate::run::psychology::{Model, Psychology};
use crate::state::body_state::{BodyRow, NAME as BODY_STATE};
use crate::state::metric_state::MetricRow;
use crate::state::psychological_distress::{AgentRecord, DistressRow, NAME};
use crate::value::V;
use crate::world::{World, METRIC_STATE};

const LONG_TERM_POLICIES: [&str; 2] = ["mean-reverting-distress-v1", "mean-reverting-life-state-v1"];
const MONITOR_PREFIX: &str = "acute_distress_monitor:";
const MONITOR_SUFFIXES: [&str; 5] =
    [":current_run", ":longest_run", ":episodes", ":recovered_run_total", ":recovered_run_count"];
const MINUTES_PER_DAY: i64 = 1440;

/// `PsychologyUpdateResult.policy_version` of a result that does not name its model.
pub const DEFAULT_POLICY_VERSION: &str = "causal-stress-v1";

/// The names of the body values of `AgentStateDelta`, in the order of `Delta::body`.
pub const BODY_NAMES: [&str; 7] =
    ["sleep_debt_minutes", "uncertainty", "reassurance", "discontent", "positive_affect", "negative_affect", "hygiene_need"];

#[track_caller]
fn unsupported<T>(reason: &'static str) -> Result<T> {
    Err(Error::unsupported(reason))
}

// ---- What the Run read of the policy -------------------------------------------------------------

/// A read of `controller.psychology` that raised in Python, or that the Run did not make: the PyO3
/// crate reads `path` from the controller again and raises what that raises (policy E14).
#[derive(Clone, Debug)]
pub struct PolicyFault {
    pub path: Vec<&'static str>,
    pub fault: Fault,
}

impl PolicyFault {
    /// The text of the refusal to raise, for a fault the Run's reading refused. None: read again.
    pub fn refusal(&self) -> Option<&str> {
        (self.fault.is_refusal() && &*self.fault.text != NOT_READ).then_some(&*self.fault.text)
    }
}

/// Why a model's numbers could not be made: the rules' refusal, or a read to raise again.
#[derive(Debug)]
pub enum Bad {
    Refused(Error),
    Policy(PolicyFault),
}

impl From<Error> for Bad {
    fn from(error: Error) -> Self {
        Bad::Refused(error)
    }
}

type Got<T> = std::result::Result<T, Bad>;

#[track_caller]
fn refused<T>(reason: &'static str) -> Got<T> {
    Err(Bad::Refused(Error::unsupported(reason)))
}

fn fault_at(path: &[&'static str], fault: Fault) -> Bad {
    Bad::Policy(PolicyFault { path: path.to_vec(), fault })
}

/// The attribute of `object` at the end of `path` (the path from the controller).
fn held<'a>(object: &'a Obj, path: &[&'static str]) -> Got<&'a Held> {
    object.get(path[path.len() - 1]).map_err(|fault| fault_at(path, fault))
}

/// An attribute that is an object the Run read: None and other values are refused.
fn object<'a>(object: &'a Obj, path: &[&'static str]) -> Got<&'a Arc<Obj>> {
    let found = held(object, path)?.object().map_err(|fault| fault_at(path, fault))?;
    found.ok_or_else(|| fault_at(path, Fault::refused(NOT_AN_OBJECT)))
}

/// `controller.psychology`.
fn policy_of(psychology: &Psychology) -> Got<&Arc<Obj>> {
    psychology.policy.as_ref().map_err(|fault| fault_at(&["psychology"], fault.clone()))
}

/// `controller.psychology.parameters`.
fn parameters_of(psychology: &Psychology) -> Got<&Arc<Obj>> {
    object(policy_of(psychology)?, &["psychology", "parameters"])
}

/// The JSON value of an attribute, or None for a value that is not one (a str subclass, an object).
fn value_of<'a>(object: &'a Obj, path: &[&'static str]) -> Got<Option<&'a V>> {
    Ok(match held(object, path)? {
        Held::Value(value) => Some(value),
        _ => None,
    })
}

/// An exact float the Python check `validate_unit_interval` passes.
fn unit_float(value: &V) -> Result<f64> {
    unit_value(exact_float(value)?)
}

/// A value of a config that `validate_unit_interval` passes, a float or an int: a Scenario's
/// `baseline_distress_by_agent` may hold the int `0` or `1`, which the rules turn into a float.
fn unit_number(value: &V) -> Result<f64> {
    unit_value(number_of(value)?)
}

fn unit_value(number: f64) -> Result<f64> {
    if !(0.0..=1.0).contains(&number) {
        return unsupported("a value is outside 0..1");
    }
    Ok(number)
}

fn exact_float(value: &V) -> Result<f64> {
    match value {
        V::Float(number) => Ok(*number),
        _ => unsupported("a value is not a float"),
    }
}

/// An exact float, or an exact int of a config that allows one (a rate of `0`, a body rate of
/// `1`): each meets a float in the rules' arithmetic, where it is this f64. Bigger ints than the
/// configs allow are left to Unsupported.
fn number_of(value: &V) -> Result<f64> {
    match value {
        V::Float(number) => Ok(*number),
        V::Int(whole) if whole.unsigned_abs() <= 1 << 53 => Ok(*whole as f64),
        V::Int(_) | V::Big(_) => unsupported("an int value is too large"),
        _ => unsupported("a value is not a float"),
    }
}

/// An attribute that must be an exact float.
fn float_attr(object: &Obj, path: &[&'static str]) -> Got<f64> {
    match value_of(object, path)? {
        Some(value) => Ok(exact_float(value)?),
        None => refused("a value is not a float"),
    }
}

/// An attribute that must be an exact float or a small int.
fn number_attr(object: &Obj, path: &[&'static str]) -> Got<f64> {
    match value_of(object, path)? {
        Some(value) => Ok(number_of(value)?),
        None => refused("a value is not a float"),
    }
}

/// An attribute that must be an exact bool.
fn bool_attr(object: &Obj, path: &[&'static str]) -> Got<bool> {
    match value_of(object, path)? {
        Some(V::Bool(flag)) => Ok(*flag),
        _ => refused("a flag is not a bool"),
    }
}

/// A plain dict of the policy: a table by person, or the body config.
type Table = IndexMap<Arc<str>, V>;

/// An attribute that must be a plain dict.
fn table_attr<'a>(object: &'a Obj, path: &[&'static str]) -> Got<&'a Table> {
    match value_of(object, path)? {
        Some(V::Map(table)) => Ok(table),
        _ => refused("a table of the policy is not a plain dict"),
    }
}

/// An item of a dict that must be a number, and whether it is an int.
fn number_item(dict: &Table, key: &str) -> Result<(f64, bool)> {
    match dict.get(key) {
        Some(value) => Ok((number_of(value)?, matches!(value, V::Int(_)))),
        None => unsupported("a field is missing"),
    }
}

/// An item of a dict that must be a number.
fn float_item(dict: &Table, key: &str) -> Result<f64> {
    number_item(dict, key).map(|(value, _)| value)
}

/// `by_agent.get(id, default)` of a dict of numbers.
#[inline]
fn rate_of(by_agent: &Table, id: &str, default: f64) -> Result<f64> {
    match by_agent.get(id) {
        Some(value) => number_of(value),
        None => Ok(default),
    }
}

/// The model of `controller.psychology`, by its exact class: a subclass may change any rule.
pub fn model_of(psychology: &Psychology) -> Got<Model> {
    psychology.model.clone().map_err(|fault| fault_at(&["psychology"], fault))
}

// ---- The distress monitor ----------------------------------------------------------------------

/// run, longest, episodes, recovered total, recovered count, in the order of `MONITOR_SUFFIXES`.
#[inline]
fn next_monitor_values(values: [f64; 5], previous: f64, current: f64, threshold: f64) -> [f64; 5] {
    let [mut run, mut longest, mut episodes, mut recovered_total, mut recovered_count] = values;
    let was_high = previous >= threshold;
    let is_high = current >= threshold;
    if run > 0.0 && !(was_high && is_high) {
        recovered_total += run;
        recovered_count += 1.0;
        run = 0.0;
    }
    if is_high {
        if run == 0.0 {
            run = 1.0;
            episodes += 1.0;
        } else {
            run += 1.0;
        }
        longest = py_max(longest, run);
    }
    [run, longest, episodes, recovered_total, recovered_count]
}

fn metric_key<'a>(buffer: &'a mut String, id: &str, suffix: &str) -> &'a str {
    buffer.clear();
    buffer.push_str(MONITOR_PREFIX);
    buffer.push_str(id);
    buffer.push_str(suffix);
    buffer
}

/// One person's stress before (the row's) and after (the final delta's).
struct Person<'a> {
    id: &'a str,
    previous: f64,
    current: f64,
}

fn plan_monitor(metrics: &MetricRow, people: &[Person<'_>], threshold: f64) -> Result<Vec<[f64; 5]>> {
    let mut buffer = String::with_capacity(64);
    let mut steps = Vec::with_capacity(people.len());
    for person in people {
        let mut values = [0.0; 5];
        for (place, suffix) in MONITOR_SUFFIXES.iter().enumerate() {
            if let Some(value) = metrics.get(metric_key(&mut buffer, person.id, suffix)) {
                // Not finite or negative, and NaN fails the comparison: the Python rule raises.
                if !(0.0 <= *value && *value < f64::INFINITY) {
                    return unsupported("a monitor value is negative or not finite");
                }
                values[place] = *value;
            }
        }
        steps.push(next_monitor_values(values, person.previous, person.current, threshold));
    }
    Ok(steps)
}

/// The monitor's values, each key kept where it is and a new one added at the end.
fn write_monitor(metrics: &mut MetricRow, people: &[Person<'_>], steps: &[[f64; 5]]) {
    let mut buffer = String::with_capacity(64);
    for (person, next) in people.iter().zip(steps) {
        for (suffix, value) in MONITOR_SUFFIXES.iter().zip(next) {
            let key = metric_key(&mut buffer, person.id, suffix);
            match metrics.get_mut(key) {
                Some(held) => *held = *value,
                None => {
                    metrics.insert(Arc::from(key), *value);
                }
            }
        }
    }
}

// ---- Long-term distress ------------------------------------------------------------------------

/// The parameters of the day-scale distress.
#[derive(Clone, Copy, Debug)]
pub struct LongTermParameters {
    pub retention: f64,
    pub acute_weight: f64,
    pub high_distress_weight: f64,
    pub high_distress_threshold: f64,
}

/// What the long-term step reads of a model with long-term state.
pub struct LongTerm<'a> {
    pub parameters: LongTermParameters,
    /// `baseline_distress_by_agent`, or None where it is not a plain dict: refused where a new
    /// record needs it.
    pub baselines: Option<&'a Table>,
}

/// Whether the model keeps long-term state. False for a model without it, whose step does nothing
/// (`getattr(self.psychology, "update_long_term_state", None)`); refused for another model with an
/// `update_long_term_state` of its own.
pub fn keeps_long_term(psychology: &Psychology) -> Got<bool> {
    let policy = policy_of(psychology)?;
    if matches!(policy.get("policy_version"), Ok(Held::Value(V::Str(version))) if LONG_TERM_POLICIES.contains(&&**version)) {
        return Ok(true);
    }
    match policy.get("update_long_term_state") {
        Ok(Held::Opaque { callable: true }) => refused("a model's long-term state this kernel does not know"),
        Ok(_) => Ok(false),
        Err(fault) if matches!(&fault.class, FaultClass::Other(name) if &**name == "builtins.AttributeError") => Ok(false),
        Err(fault) => Err(fault_at(&["psychology", "update_long_term_state"], fault)),
    }
}

/// The parameters of a model with long-term state, in the order the kernel before read them.
pub fn long_term_of(psychology: &Psychology) -> Got<LongTerm<'_>> {
    let settings = parameters_of(psychology)?;
    let unit = |name: &'static str| -> Got<f64> {
        match value_of(settings, &["psychology", "parameters", name])? {
            Some(value) => Ok(unit_float(value)?),
            None => refused("a value is not a float"),
        }
    };
    let parameters = LongTermParameters {
        retention: unit("chronic_retention")?,
        acute_weight: unit("chronic_acute_weight")?,
        high_distress_weight: unit("chronic_high_distress_weight")?,
        high_distress_threshold: unit("high_distress_threshold")?,
    };
    let baselines = match value_of(settings, &["psychology", "parameters", "baseline_distress_by_agent"])? {
        Some(V::Map(table)) => Some(table),
        _ => None,
    };
    Ok(LongTerm { parameters, baselines })
}

/// The day's end value of D: `next_long_term_distress` in `domain/psychological_state.py`.
#[inline]
pub fn next_long_term_distress(record: &AgentRecord, parameters: &LongTermParameters) -> f64 {
    let observed = record.daily_observed_minutes as f64;
    let mean_acute = record.daily_acute_sum / observed;
    let high_fraction = record.daily_high_distress_minutes as f64 / observed;
    let baseline = record.baseline_distress;
    let next_value = baseline
        + parameters.retention * (record.long_term_distress - baseline)
        + parameters.acute_weight * (mean_acute - baseline)
        + parameters.high_distress_weight * high_fraction;
    py_min(1.0, py_max(0.0, next_value))
}

#[inline]
fn accumulate(record: &mut AgentRecord, acute: f64, threshold: f64) {
    record.daily_acute_sum += acute;
    record.daily_observed_minutes += 1;
    if acute >= threshold {
        record.daily_high_distress_minutes += 1;
    }
}

#[inline]
fn close_day(record: &mut AgentRecord, parameters: &LongTermParameters) -> Result<()> {
    record.long_term_distress = next_long_term_distress(record, parameters);
    record.daily_acute_sum = 0.0;
    record.daily_high_distress_minutes = 0;
    record.daily_observed_minutes = 0;
    record.completed_days = match record.completed_days.checked_add(1) {
        Some(days) => days,
        None => return unsupported("completed days do not fit"),
    };
    Ok(())
}

struct LongTermPlan {
    /// By position in the row: the record's next values, if it changes.
    changed: Vec<Option<AgentRecord>>,
    /// New records, in the order they are added.
    added: Vec<(usize, AgentRecord)>,
}

fn plan_long_term(row: &DistressRow, people: &[Person<'_>], minute: i64, long_term: &LongTerm<'_>) -> Result<LongTermPlan> {
    if !row.is_valid() {
        return unsupported("the namespace does not pass validation");
    }
    if row.last_updated_minute != minute - 1 {
        return unsupported("the namespace is not at the previous minute");
    }
    let parameters = &long_term.parameters;
    let threshold = parameters.high_distress_threshold;
    let mut changed: Vec<Option<AgentRecord>> = vec![None; row.agents.len()];
    let mut added = Vec::new();
    for (place, person) in people.iter().enumerate() {
        match row.agents.get_index_of(person.id) {
            Some(index) => {
                let mut record = row.agents[index];
                accumulate(&mut record, person.current, threshold);
                changed[index] = Some(record);
            }
            None => {
                let baseline = match long_term.baselines {
                    Some(baselines) => match baselines.get(person.id) {
                        Some(baseline) => unit_number(baseline)?,
                        None => 0.0,
                    },
                    None => return unsupported("the baselines are not a plain dict"),
                };
                let mut record = AgentRecord {
                    baseline_distress: baseline,
                    long_term_distress: baseline,
                    daily_acute_sum: 0.0,
                    daily_high_distress_minutes: 0,
                    daily_observed_minutes: 0,
                    completed_days: 0,
                };
                accumulate(&mut record, person.current, threshold);
                added.push((place, record));
            }
        }
    }
    if (minute + 1) % MINUTES_PER_DAY == 0 {
        for (index, (_, before)) in row.agents.iter().enumerate() {
            let mut record = changed[index].unwrap_or(*before);
            if record.daily_observed_minutes != 0 {
                close_day(&mut record, parameters)?;
                changed[index] = Some(record);
            }
        }
        for (_, record) in &mut added {
            close_day(record, parameters)?;
        }
    }
    let all_valid = changed.iter().flatten().all(AgentRecord::is_valid)
        && added.iter().all(|(place, record)| record.is_valid() && !people[*place].id.is_empty())
        && row.agents.len() + added.len() <= 100_000;
    if !all_valid {
        return unsupported("the new values do not pass validation");
    }
    Ok(LongTermPlan { changed, added })
}

fn write_long_term(row: &mut DistressRow, people: &[Person<'_>], minute: i64, plan: LongTermPlan) -> Result<()> {
    for (index, next) in plan.changed.iter().enumerate() {
        let Some(next) = next else {
            continue;
        };
        let Some((_, record)) = row.agents.get_index_mut(index) else {
            return unsupported("the row changed under the plan");
        };
        record.long_term_distress = next.long_term_distress;
        record.daily_acute_sum = next.daily_acute_sum;
        record.daily_high_distress_minutes = next.daily_high_distress_minutes;
        record.daily_observed_minutes = next.daily_observed_minutes;
        record.completed_days = next.completed_days;
    }
    row.last_updated_minute = minute;
    for (place, record) in plan.added {
        row.agents.insert(Arc::from(people[place].id), record);
    }
    Ok(())
}

/// `_update_long_term_psychology` of the update phase, for a model with long-term state: the
/// distress monitor's five values of each person in `world.metric_state`, and each person's day in
/// `psychological_distress_v1`. `finals` are each person's ID and final stress (`final_deltas`),
/// which the caller checked to be str and floats within 0..1; `metric_state_held` is whether
/// `world.metric_state` holds its slot's tree (a field assigned whole does not). Nothing is written
/// unless every check passes.
pub fn update_long_term(
    world: &World,
    long_term: &LongTerm<'_>,
    finals: &[(&str, f64)],
    minute: i64,
    metric_state_held: bool,
) -> Result<()> {
    // `previous_by_agent` and `current_by_agent` of the Python step, or Unsupported if the Python
    // rules would do anything but read them: their keys differ, or a value is out of bounds. The
    // previous values are the rows': nothing has written a person since the view was made.
    let mut people: Vec<Person<'_>> = finals.iter().map(|&(id, current)| Person { id, previous: 0.0, current }).collect();
    {
        let rows = world.rows()?;
        if people.len() != rows.agents.len() {
            return unsupported("the world and the deltas have different people");
        }
        for person in &mut people {
            let Some(row) = rows.agents.get(person.id) else {
                return unsupported("the world and the deltas have different people");
            };
            if !(0.0..=1.0).contains(&row.stress) {
                return unsupported("a value is outside 0..1");
            }
            person.previous = row.stress;
        }
    }
    people.sort_unstable_by(|a, b| a.id.as_bytes().cmp(b.id.as_bytes()));
    if people.windows(2).any(|pair| pair[0].id == pair[1].id) {
        return unsupported("two deltas for one person");
    }

    if world.slots.get(NAME).is_none() || world.slots.get(METRIC_STATE).is_none() {
        return unsupported("the namespace is not owned");
    }
    if !metric_state_held {
        return unsupported("the field holds another dict");
    }
    let threshold = long_term.parameters.high_distress_threshold;
    let monitor = plan_monitor(&*world.row::<MetricRow>(METRIC_STATE)?, &people, threshold)?;
    let plan = plan_long_term(&*world.row::<DistressRow>(NAME)?, &people, minute, long_term)?;

    write_monitor(&mut *world.row_mut::<MetricRow>(METRIC_STATE)?, &people, &monitor);
    write_long_term(&mut *world.row_mut::<DistressRow>(NAME)?, &people, minute, plan)
}

// ---- A minute of each person's values ----------------------------------------------------------

/// The rates of `PsychologyParameters`.
#[derive(Clone, Copy, Debug)]
struct Rates {
    toilet_growth: f64,
    hunger_growth: f64,
    awake_fatigue_growth: f64,
    resting_fatigue_recovery: f64,
    sleeping_fatigue_recovery: f64,
    stress_pressure: f64,
    queued_stress: f64,
    resting_stress_recovery: f64,
    sleeping_stress_recovery: f64,
    need_pressure_threshold: f64,
    hydration_growth: f64,
    support_wait_stress: f64,
    natural_recovery_rate: f64,
    direct_rest_recovery: bool,
    resource_service_enabled: bool,
}

/// What `canonical_body_config` keeps for the life-state models.
#[derive(Clone, Copy, Debug)]
struct BodyRates {
    awake_sleep_debt: f64,
    rest_sleep_debt_recovery: f64,
    sleep_sleep_debt_recovery: f64,
    sleep_debt_stress: f64,
    reassurance_decay: f64,
    uncertainty_drift: f64,
    uncertainty_decay: f64,
    discontent_queue: f64,
    discontent_relief: f64,
    discontent_stress: f64,
    positive_affect_decay: f64,
    hygiene_growth: f64,
    sanitation_hygiene_growth: f64,
    negative_affect_stress: f64,
    sanitation_stress: f64,
    /// Whether `discontent_stress_rate` is an int, which a separated family's pressure is
    /// (`life_state_before` of the trace keeps it).
    discontent_stress_int: bool,
    /// `body-feedback-sleep-debt-v1`: only the debt feeds back.
    sleep_debt_only: bool,
    tick_start_reference: bool,
}

/// The tables by person, for lookups that a scenario may fill with one value per person.
struct RateTables<'a> {
    toilet: &'a Table,
    hunger: &'a Table,
    awake_fatigue: &'a Table,
    natural_recovery: &'a Table,
    baseline: &'a Table,
    // Only with a formula; the built-in feedback of a life-state policy without one has no tables,
    // and a formula's config without one has an empty one: both give the shared rate.
    sleep_debt: Option<&'a Table>,
    negative: Option<&'a Table>,
    discontent: Option<&'a Table>,
}

/// The numbers of a model, from what the Run read of `controller.psychology`.
pub struct Params<'a> {
    model: Model,
    rates: Rates,
    body: Option<BodyRates>,
    tables: RateTables<'a>,
    /// `policy_version` of the result: the model's with a trace; `SimplePsychologyPolicy`'s result
    /// and a result without a trace keep the field's default.
    policy_version: Option<Arc<str>>,
}

impl Params<'_> {
    pub fn model(&self) -> Model {
        self.model
    }
}

/// The numbers of `model`, read in the order of the kernel before: `base` of a life-state model,
/// the parameters, the flag of direct rest recovery, `view.resource_service_enabled` (None for a
/// value that is not an exact bool), the body formula and config of a life-state model, the tables
/// by person, and with a trace the model's `policy_version`.
pub fn params(psychology: &Psychology, model: Model, resource_service_enabled: Option<bool>, traced: bool) -> Got<Params<'_>> {
    let policy = policy_of(psychology)?;
    let p = parameters_of(psychology)?;
    let life_model = matches!(model, Model::LifeState | Model::Integrated);
    let base = if life_model { Some(object(policy, &["psychology", "base"])?) } else { None };
    let f = |name: &'static str| float_attr(p, &["psychology", "parameters", name]);
    let rates = Rates {
        toilet_growth: f("toilet_growth")?,
        hunger_growth: f("hunger_growth")?,
        awake_fatigue_growth: f("awake_fatigue_growth")?,
        resting_fatigue_recovery: f("resting_fatigue_recovery")?,
        sleeping_fatigue_recovery: f("sleeping_fatigue_recovery")?,
        stress_pressure: f("stress_pressure")?,
        queued_stress: f("queued_stress")?,
        resting_stress_recovery: f("resting_stress_recovery")?,
        sleeping_stress_recovery: f("sleeping_stress_recovery")?,
        need_pressure_threshold: f("need_pressure_threshold")?,
        // `resource_service.hydration_growth_per_minute` of the Scenario's config, as written.
        hydration_growth: number_attr(p, &["psychology", "parameters", "hydration_growth"])?,
        support_wait_stress: f("support_wait_stress")?,
        natural_recovery_rate: f("natural_recovery_rate_per_minute")?,
        direct_rest_recovery: match base {
            Some(base) => bool_attr(base, &["psychology", "base", "include_direct_rest_stress_recovery"])?,
            None => bool_attr(policy, &["psychology", "include_direct_rest_stress_recovery"])?,
        },
        resource_service_enabled: match resource_service_enabled {
            Some(flag) => flag,
            None => return refused("a flag is not a bool"),
        },
    };
    let (body, config, with_formula) = if life_model {
        let formula = held(policy, &["psychology", "body_formula"])?;
        let Some(V::Map(config)) = value_of(policy, &["psychology", "body_config"])? else {
            return refused("the body config is not a plain dict");
        };
        (Some(body_rates(policy, config, formula)?), Some(config), !formula.is_none())
    } else {
        (None, None, false)
    };
    // `body_config.get(name, {})` with a formula, which must be a plain dict.
    let by_agent = |name: &str| -> Got<Option<&Table>> {
        match config {
            Some(config) if with_formula => match config.get(name) {
                Some(V::Map(table)) => Ok(Some(table)),
                Some(_) => refused("a table of the policy is not a plain dict"),
                None => Ok(None),
            },
            _ => Ok(None),
        }
    };
    let t = |name: &'static str| table_attr(p, &["psychology", "parameters", name]);
    let tables = RateTables {
        toilet: t("toilet_growth_by_agent")?,
        hunger: t("hunger_growth_by_agent")?,
        awake_fatigue: t("awake_fatigue_growth_by_agent")?,
        natural_recovery: t("natural_recovery_rate_by_agent")?,
        baseline: t("baseline_distress_by_agent")?,
        sleep_debt: by_agent("sleep_debt_stress_rate_by_agent")?,
        negative: by_agent("negative_affect_stress_rate_by_agent")?,
        discontent: by_agent("discontent_stress_rate_by_agent")?,
    };
    let policy_version = if traced && model != Model::Simple {
        match held(policy, &["psychology", "policy_version"])?.text() {
            Some(version) => Some(Arc::from(version)),
            None => return refused("the policy version is not a str"),
        }
    } else {
        None
    };
    Ok(Params { model, rates, body, tables, policy_version })
}

fn body_rates(policy: &Obj, config: &Table, formula: &Held) -> Got<BodyRates> {
    let sleep_debt_only = match formula {
        Held::Value(V::Null) => false,
        Held::Object(formula) if formula.class.has_name("sheltersim.policies.registry", "BodyDynamicsFormula") => {
            match value_of(formula, &["psychology", "body_formula", "model_id"])? {
                Some(V::Str(model_id)) => &**model_id == "body-feedback-sleep-debt-v1",
                _ => return refused("the body formula has no plain id"),
            }
        }
        _ => return refused("the body formula is not one this kernel knows"),
    };
    let item = |key: &str| float_item(config, key);
    let (discontent_stress, discontent_stress_int) = number_item(config, "discontent_stress_rate")?;
    let rates = BodyRates {
        awake_sleep_debt: item("awake_sleep_debt_minutes_per_minute")?,
        rest_sleep_debt_recovery: item("rest_sleep_debt_recovery_per_minute")?,
        sleep_sleep_debt_recovery: item("sleep_sleep_debt_recovery_per_minute")?,
        sleep_debt_stress: item("sleep_debt_stress_rate")?,
        reassurance_decay: item("reassurance_decay_per_minute")?,
        uncertainty_drift: item("uncertainty_drift_per_minute")?,
        uncertainty_decay: item("uncertainty_decay_per_minute")?,
        discontent_queue: item("discontent_queue_rate")?,
        discontent_relief: item("discontent_relief_per_minute")?,
        discontent_stress,
        discontent_stress_int,
        positive_affect_decay: item("positive_affect_decay_per_minute")?,
        hygiene_growth: item("hygiene_growth_per_minute")?,
        sanitation_hygiene_growth: item("sanitation_hygiene_growth_per_minute")?,
        negative_affect_stress: item("negative_affect_stress_rate")?,
        sanitation_stress: item("sanitation_stress_rate")?,
        sleep_debt_only,
        tick_start_reference: bool_attr(policy, &["psychology", "use_tick_start_stress_for_feedback"])?,
    };
    // The checks of sleep_debt_step on the rates; the others only meet arithmetic.
    let usable = |rate: f64| rate.is_finite() && rate >= 0.0;
    if !(usable(rates.awake_sleep_debt) && usable(rates.rest_sleep_debt_recovery) && usable(rates.sleep_sleep_debt_recovery)) {
        return refused("a sleep debt rate is out of bounds");
    }
    Ok(rates)
}

/// A person's values from the view that the life-state models read.
#[derive(Clone, Copy, Default)]
struct LifeInput {
    debt: f64,
    /// `required_sleep_minutes`, which is an int within 0..1440, or None.
    required_sleep: Option<i64>,
    reassurance: f64,
    uncertainty: f64,
    query_pressure: f64,
    misinformation_pressure: f64,
    discontent: f64,
    privacy_discontent: f64,
    positive: f64,
    hygiene: f64,
    ventilation_growth: f64,
    sanitation: f64,
    negative: f64,
    family_separated: bool,
    sleep_rate: f64,
    negative_rate: f64,
    discontent_rate: f64,
}

/// What the rules read of one person.
#[derive(Clone, Copy)]
struct Input {
    activity: Activity,
    rest_efficiency: f64,
    toilet: f64,
    hunger: f64,
    fatigue: f64,
    hydration: f64,
    stress: f64,
    sensitivity: f64,
    support_open: bool,
    environmental_per_minute: f64,
    toilet_growth: f64,
    hunger_growth: f64,
    awake_fatigue: f64,
    kappa: f64,
    baseline: f64,
    life: LifeInput,
}

/// The needs and stress of the next minute, and the body values if the model has them.
#[derive(Clone, Copy)]
struct Next {
    toilet: f64,
    hunger: f64,
    fatigue: f64,
    hydration: f64,
    stress: f64,
    /// In the order of `BODY_NAMES`.
    body: Option<[f64; 7]>,
}

#[inline]
fn clamp(value: f64) -> f64 {
    py_min(1.0, py_max(0.0, value))
}

/// `_mean_reverting_terms(value, baseline, kappa, 1.0)`, whose checks the caller has done.
#[inline]
fn mean_reverting_terms(value: f64, baseline: f64, kappa: f64) -> Result<(f64, f64)> {
    let decay = mathx::exp(-kappa * 1.0)?;
    let natural = (baseline - value) * (1.0 - decay);
    if kappa == 0.0 {
        return Ok((natural, 1.0));
    }
    Ok((natural, -mathx::expm1(-kappa * 1.0)? / kappa))
}

/// One person's next minute. A math function raises as Python did. Every float operation
/// keeps the order of `policies/psychology.py`, and `py_max`/`py_min` return the first on a tie.
fn step(model: Model, rates: &Rates, body: Option<&BodyRates>, a: &Input, mut stages: Option<&mut Stages>) -> Result<Next> {
    // SimplePsychologyPolicy.explain_update
    let (fatigue_delta, stress_recovery) = match a.activity {
        Activity::Sleeping => (
            -rates.sleeping_fatigue_recovery * a.rest_efficiency,
            if rates.direct_rest_recovery { rates.sleeping_stress_recovery } else { 0.0 },
        ),
        Activity::Resting => (
            -rates.resting_fatigue_recovery * a.rest_efficiency,
            if rates.direct_rest_recovery { rates.resting_stress_recovery } else { 0.0 },
        ),
        _ => (a.awake_fatigue, 0.0),
    };
    let mut highest = py_max(py_max(a.toilet, a.hunger), a.fatigue);
    if rates.resource_service_enabled {
        highest = py_max(highest, a.hydration);
    }
    let unmet = py_max(0.0, highest - rates.need_pressure_threshold);
    let need_pressure_stress = rates.stress_pressure * unmet * a.sensitivity;
    let queued = a.activity == Activity::Queued;
    let queue_wait_stress = if queued { rates.queued_stress } else { 0.0 };
    let support_wait_stress = if a.support_open { rates.support_wait_stress } else { 0.0 };
    let environmental_stress = a.environmental_per_minute * a.sensitivity;
    let stress_delta =
        need_pressure_stress + queue_wait_stress + support_wait_stress + environmental_stress - stress_recovery;
    let simple_stress = clamp(a.stress + stress_delta);
    if let Some(stages) = stages.as_deref_mut() {
        stages.highest = highest;
        stages.unmet = unmet;
        stages.need_pressure_stress = need_pressure_stress;
        stages.queue_wait_stress = queue_wait_stress;
        stages.support_wait_stress = support_wait_stress;
        stages.environmental_stress = environmental_stress;
        stages.stress_recovery = stress_recovery;
        stages.stress_delta = stress_delta;
        stages.simple_stress = simple_stress;
    }
    let needs = |stress: f64, body: Option<[f64; 7]>| Next {
        toilet: clamp(a.toilet + a.toilet_growth),
        hunger: clamp(a.hunger + a.hunger_growth),
        fatigue: clamp(a.fatigue + fatigue_delta),
        hydration: if rates.resource_service_enabled { clamp(a.hydration + rates.hydration_growth) } else { 0.0 },
        stress,
        body,
    };
    match model {
        Model::Simple => return Ok(needs(simple_stress, None)),
        // BoundedStressPolicy.explain_update
        Model::Bounded => {
            let pressure = py_sum(&[need_pressure_stress, queue_wait_stress, support_wait_stress, environmental_stress]);
            let added = (1.0 - a.stress) * -mathx::expm1(-pressure)?;
            let recovery = a.stress * -mathx::expm1(-stress_recovery)?;
            if let Some(stages) = stages {
                stages.added = added;
                stages.recovery = recovery;
            }
            return Ok(needs(py_min(1.0, py_max(0.0, a.stress + added - recovery)), None));
        }
        // MeanRevertingDistressPolicy.explain_update
        Model::MeanReverting => {
            let (natural, multiplier) = mean_reverting_terms(a.stress, a.baseline, a.kappa)?;
            if let Some(stages) = stages {
                stages.natural = natural;
                stages.multiplier = multiplier;
            }
            let forced = py_sum(&[
                need_pressure_stress * multiplier,
                queue_wait_stress * multiplier,
                support_wait_stress * multiplier,
                environmental_stress * multiplier,
                -stress_recovery * multiplier,
                0.0 * multiplier,
            ]);
            return Ok(needs(clamp(a.stress + (natural + forced)), None));
        }
        Model::LifeState | Model::Integrated => {}
    }

    // LifeStatePsychologyPolicy.explain_update and sleep_debt_step
    let Some(g) = body else {
        return unsupported("the model has no body rates");
    };
    let life = &a.life;
    let family_pressure = if life.family_separated { g.discontent_stress } else { 0.0 };
    let effective_awake = match life.required_sleep {
        None => g.awake_sleep_debt,
        Some(0) => 0.0,
        // Both are small ints: their true division is this f64 division.
        Some(required) => py_min(10.0, required as f64 / (1440 - required).max(1) as f64),
    };
    let next_debt = match a.activity {
        Activity::Sleeping => life.debt - g.sleep_sleep_debt_recovery * a.rest_efficiency,
        Activity::Resting => life.debt - g.rest_sleep_debt_recovery * a.rest_efficiency,
        _ => life.debt + effective_awake,
    };
    let debt = py_min(1440.0, py_max(0.0, next_debt));
    let reassurance = py_max(0.0, life.reassurance - g.reassurance_decay);
    let uncertainty_without = py_min(
        1.0,
        py_max(
            0.0,
            life.uncertainty + life.query_pressure * g.uncertainty_drift
                - g.uncertainty_decay
                - reassurance * g.uncertainty_decay,
        ),
    );
    let uncertainty = py_min(
        1.0,
        py_max(
            0.0,
            life.uncertainty
                + (life.query_pressure + life.misinformation_pressure) * g.uncertainty_drift
                - g.uncertainty_decay
                - reassurance * g.uncertainty_decay,
        ),
    );
    let queue_pressure = if queued { g.discontent_queue } else { 0.0 };
    let need_pressure = py_max(py_max(py_max(a.toilet, a.hunger), a.fatigue), a.hydration);
    let resting = matches!(a.activity, Activity::Resting | Activity::Sleeping);
    let support_relief = if resting && !a.support_open { g.discontent_relief } else { 0.0 };
    let reference = if g.tick_start_reference { a.stress } else { simple_stress };
    let discontent = py_min(
        1.0,
        py_max(
            0.0,
            life.discontent + reference * g.discontent_stress + queue_pressure
                + life.privacy_discontent
                + py_max(0.0, need_pressure - 0.5) * g.discontent_stress
                - support_relief
                - reassurance * g.discontent_relief,
        ),
    );
    let positive = 0.5 + (life.positive - 0.5) * py_max(0.0, 1.0 - g.positive_affect_decay);
    let hygiene = py_min(
        1.0,
        py_max(
            0.0,
            life.hygiene + g.hygiene_growth + life.ventilation_growth
                + (1.0 - life.sanitation) * g.sanitation_hygiene_growth,
        ),
    );
    let negative = py_min(
        1.0,
        py_max(
            0.0,
            life.negative + reference * g.negative_affect_stress
                - reassurance * g.negative_affect_stress,
        ),
    );
    // BodyDynamicsFormula.stress_feedback, and the same with the shared rates without a formula.
    let feedback_with = |uncertainty: f64| -> f64 {
        let debt_component = debt / 1440.0 * life.sleep_rate;
        if g.sleep_debt_only {
            return debt_component;
        }
        debt_component + uncertainty * life.negative_rate + discontent * life.discontent_rate
            + hygiene * g.sanitation_stress
            + (1.0 - life.sanitation) * g.sanitation_stress
            + family_pressure
            - reassurance * life.negative_rate
            - positive * g.discontent_relief
    };
    let feedback = feedback_with(uncertainty);
    let body_values = Some([debt, uncertainty, reassurance, discontent, positive, negative, hygiene]);
    if model == Model::LifeState {
        if let Some(stages) = stages {
            stages.family_pressure = family_pressure;
            stages.family_pressure_int = life.family_separated && g.discontent_stress_int;
            stages.feedback = feedback;
            stages.information_feedback = feedback - feedback_with(uncertainty_without);
        }
        return Ok(needs(py_min(1.0, py_max(0.0, simple_stress + feedback)), body_values));
    }

    // IntegratedDistressLifeStatePolicy.explain_update
    let information_feedback = feedback - feedback_with(uncertainty_without);
    let life_feedback = feedback - information_feedback;
    let (natural, multiplier) = mean_reverting_terms(a.stress, a.baseline, a.kappa)?;
    if let Some(stages) = stages {
        stages.family_pressure = family_pressure;
        stages.family_pressure_int = life.family_separated && g.discontent_stress_int;
        stages.feedback = feedback;
        stages.information_feedback = information_feedback;
        stages.natural = natural;
        stages.multiplier = multiplier;
    }
    let forced = py_sum(&[
        need_pressure_stress * multiplier,
        queue_wait_stress * multiplier,
        support_wait_stress * multiplier,
        environmental_stress * multiplier,
        life_feedback * multiplier,
        information_feedback * multiplier,
    ]);
    Ok(needs(clamp(a.stress + (natural + forced)), body_values))
}

/// What `step` knows on the way, which only a trace reads.
#[derive(Clone, Copy, Default)]
struct Stages {
    highest: f64,
    unmet: f64,
    need_pressure_stress: f64,
    queue_wait_stress: f64,
    support_wait_stress: f64,
    environmental_stress: f64,
    stress_recovery: f64,
    stress_delta: f64,
    simple_stress: f64,
    /// `BoundedStressPolicy`.
    added: f64,
    recovery: f64,
    /// The mean reversion.
    natural: f64,
    multiplier: f64,
    /// The life-state models.
    family_pressure: f64,
    /// The int `discontent_stress_rate` of a separated family, as Python keeps it.
    family_pressure_int: bool,
    feedback: f64,
    information_feedback: f64,
}

/// `AgentPsychologyTrace` of `explain_update`, on what `step` found. Every float operation keeps the
/// order of the Python expression, and each field stays what the stage that last replaces it says.
fn person_trace(model: Model, rates: &Rates, a: &Input, next: &Next, stages: &Stages, index: usize) -> PersonTrace {
    let s = stages;
    let before = a.stress;
    let mut needs = Vec::with_capacity(4);
    needs.push(("toilet_urgency", a.toilet, next.toilet));
    needs.push(("hunger", a.hunger, next.hunger));
    needs.push(("fatigue", a.fatigue, next.fatigue));
    if rates.resource_service_enabled {
        needs.push(("hydration_need", a.hydration, next.hydration));
    }
    // `max(needs, key=...)`: the first of the highest.
    let mut highest_need = needs[0].0;
    let mut highest_value = needs[0].1;
    for &(name, value, _) in &needs[1..] {
        if value > highest_value {
            highest_need = name;
            highest_value = value;
        }
    }
    let recovery = -s.stress_recovery;
    let simple_components = vec![
        ("unmet_need_pressure", s.need_pressure_stress),
        ("facility_queue_wait", s.queue_wait_stress),
        ("staff_support_wait", s.support_wait_stress),
        ("experimental_environmental_stress", s.environmental_stress),
        ("recovery", recovery),
    ];
    let mut trace = PersonTrace {
        index,
        needs,
        stress_before: before,
        stress_after: next.stress,
        highest_need,
        highest_need_value: highest_value,
        need_pressure: s.unmet,
        need_pressure_stress: s.need_pressure_stress,
        queue_wait_stress: s.queue_wait_stress,
        support_wait_stress: s.support_wait_stress,
        recovery_stress: recovery,
        environmental_stress: s.environmental_stress,
        natural_recovery_stress: 0.0,
        life_state_feedback_stress: 0.0,
        information_uncertainty_stress: 0.0,
        life_state: None,
        family_pressure_int: false,
        components: simple_components,
        raw_stress_delta: s.stress_delta,
        candidate_before_clip: before + s.stress_delta,
        clipped_delta: s.simple_stress - before,
    };
    match model {
        Model::Simple => {}
        // It scales four fields and leaves the first stage's components and deltas.
        Model::Bounded => {
            let pressure =
                py_sum(&[s.need_pressure_stress, s.queue_wait_stress, s.support_wait_stress, s.environmental_stress]);
            let scale = if pressure != 0.0 { s.added / pressure } else { 0.0 };
            trace.need_pressure_stress = s.need_pressure_stress * scale;
            trace.queue_wait_stress = s.queue_wait_stress * scale;
            trace.support_wait_stress = s.support_wait_stress * scale;
            trace.environmental_stress = s.environmental_stress * scale;
            trace.recovery_stress = -s.recovery;
        }
        Model::MeanReverting => {
            let forced = [
                ("unmet_need_pressure", s.need_pressure_stress * s.multiplier),
                ("facility_queue_wait", s.queue_wait_stress * s.multiplier),
                ("staff_support_wait", s.support_wait_stress * s.multiplier),
                ("experimental_environmental_stress", s.environmental_stress * s.multiplier),
                ("recovery", recovery * s.multiplier),
                ("life_state_feedback", 0.0 * s.multiplier),
            ];
            revert_to_mean(&mut trace, s, &forced, before, next.stress);
        }
        Model::LifeState | Model::Integrated => {
            let life = &a.life;
            let misinformation = life.misinformation_pressure;
            let values = next.body.unwrap_or_default();
            trace.family_pressure_int = s.family_pressure_int;
            trace.life_state = Some((
                [
                    life.debt,
                    life.uncertainty,
                    life.reassurance,
                    life.discontent,
                    life.positive,
                    life.negative,
                    life.hygiene,
                    s.family_pressure,
                    misinformation,
                ],
                [
                    values[0], values[1], values[2], values[3], values[4], values[5], values[6], s.family_pressure,
                    misinformation,
                ],
            ));
            let life_feedback = s.feedback - s.information_feedback;
            trace.life_state_feedback_stress = life_feedback;
            trace.information_uncertainty_stress = s.information_feedback;
            if model == Model::LifeState {
                let candidate = s.simple_stress + s.feedback;
                trace.raw_stress_delta = candidate - before;
                trace.candidate_before_clip = candidate;
                trace.clipped_delta = next.stress - before;
                trace.components = vec![
                    ("unmet_need_pressure", s.need_pressure_stress),
                    ("facility_queue_wait", s.queue_wait_stress),
                    ("staff_support_wait", s.support_wait_stress),
                    ("experimental_environmental_stress", s.environmental_stress),
                    ("recovery", recovery),
                    (
                        "base_stage_clip",
                        s.simple_stress
                            - (before
                                + s.need_pressure_stress
                                + s.queue_wait_stress
                                + s.support_wait_stress
                                + s.environmental_stress
                                + recovery),
                    ),
                    ("life_state_feedback", life_feedback),
                    ("information_belief_via_uncertainty", s.information_feedback),
                ];
            } else {
                let forced = [
                    ("unmet_need_pressure", s.need_pressure_stress * s.multiplier),
                    ("facility_queue_wait", s.queue_wait_stress * s.multiplier),
                    ("staff_support_wait", s.support_wait_stress * s.multiplier),
                    ("experimental_environmental_stress", s.environmental_stress * s.multiplier),
                    ("life_state_feedback", life_feedback * s.multiplier),
                    ("information_belief_via_uncertainty", s.information_feedback * s.multiplier),
                ];
                revert_to_mean(&mut trace, s, &forced, before, next.stress);
                trace.recovery_stress = 0.0;
            }
        }
    }
    trace
}

/// What the two mean-reverting models replace.
fn revert_to_mean(trace: &mut PersonTrace, s: &Stages, forced: &[(&'static str, f64); 6], before: f64, stress: f64) {
    let raw = s.natural + py_sum(&forced.map(|(_, amount)| amount));
    trace.natural_recovery_stress = s.natural;
    trace.raw_stress_delta = raw;
    trace.candidate_before_clip = before + raw;
    trace.clipped_delta = stress - before;
    trace.components = Vec::with_capacity(7);
    trace.components.push(("natural_recovery", s.natural));
    trace.components.extend_from_slice(forced);
}

/// Whether `AgentStateDelta`'s checks pass these values. NaN fails the comparison.
#[inline]
fn within_bounds(next: &Next) -> bool {
    let unit = |value: f64| (0.0..=1.0).contains(&value);
    unit(next.toilet)
        && unit(next.hunger)
        && unit(next.fatigue)
        && unit(next.hydration)
        && unit(next.stress)
        && next.body.is_none_or(|body| (0.0..=1440.0).contains(&body[0]) && body[1..].iter().copied().all(unit))
}

/// A person's fields that only the view has.
struct ViewPerson {
    /// `group_id`, for the life-state models; an empty one is left to Python.
    group: Option<Arc<str>>,
    rest_efficiency: f64,
    environmental_per_minute: f64,
    toilet_growth: f64,
    hunger_growth: f64,
    awake_fatigue: f64,
    kappa: f64,
    baseline: f64,
    life: LifeInput,
}

fn read_life(agent: &ViewAgent, zone: &ViewZone, tables: &RateTables<'_>, g: &BodyRates, rest_efficiency: f64) -> Result<LifeInput> {
    let required_sleep = match agent.person.required_sleep_minutes {
        None => None,
        Some(minutes) if (0..=1440).contains(&minutes) => Some(minutes),
        Some(_) => return unsupported("the required sleep is out of bounds"),
    };
    let id = &*agent.id;
    let rate = |table: Option<&Table>, default: f64| match table {
        Some(by_agent) => rate_of(by_agent, id, default),
        None => Ok(default),
    };
    let life = LifeInput {
        debt: agent.body.sleep_debt_minutes,
        required_sleep,
        reassurance: agent.body.reassurance,
        uncertainty: agent.body.uncertainty,
        query_pressure: agent.information_query_pressure,
        misinformation_pressure: agent.misinformation_belief_pressure,
        discontent: agent.body.discontent,
        privacy_discontent: zone.privacy_discontent_per_minute,
        positive: agent.body.positive_affect,
        hygiene: agent.body.hygiene_need,
        ventilation_growth: zone.ventilation_hygiene_growth_per_minute,
        sanitation: zone.sanitation_level,
        negative: agent.body.negative_affect,
        family_separated: false,
        sleep_rate: rate(tables.sleep_debt, g.sleep_debt_stress)?,
        negative_rate: rate(tables.negative, g.negative_affect_stress)?,
        discontent_rate: rate(tables.discontent, g.discontent_stress)?,
    };
    // The checks of sleep_debt_step on what the view holds.
    if !(life.debt.is_finite() && (0.0..=1440.0).contains(&life.debt)) || !(0.0..=1.0).contains(&rest_efficiency) {
        return unsupported("the sleep debt or the sleep efficiency is out of bounds");
    }
    Ok(life)
}

fn read_view_people(data: &ViewData, params: &Params<'_>) -> Result<Vec<ViewPerson>> {
    let (tables, rates) = (&params.tables, &params.rates);
    let reversion = matches!(params.model, Model::MeanReverting | Model::Integrated);
    let mut found = Vec::with_capacity(data.agents.len());
    for agent in &data.agents {
        // An empty id fails the checks of AgentStateDelta.
        if agent.id.is_empty() {
            return unsupported("an id is empty");
        }
        let id = &*agent.id;
        let zone = data.zone_of(agent);
        let rest_efficiency = zone.rest_efficiency_multiplier;
        let (life, group) = match params.body.as_ref() {
            Some(g) => {
                // `group_id or ""` meets the people of an empty group.
                if agent.group_id.as_ref().is_some_and(|group| group.is_empty()) {
                    return unsupported("a group id is empty");
                }
                (read_life(agent, zone, tables, g, rest_efficiency)?, agent.group_id.clone())
            }
            None => (LifeInput::default(), None),
        };
        found.push(ViewPerson {
            group,
            rest_efficiency,
            environmental_per_minute: zone.environmental_stress_per_minute,
            toilet_growth: rate_of(tables.toilet, id, rates.toilet_growth)?,
            hunger_growth: rate_of(tables.hunger, id, rates.hunger_growth)?,
            awake_fatigue: rate_of(tables.awake_fatigue, id, rates.awake_fatigue_growth)?,
            kappa: if reversion { rate_of(tables.natural_recovery, id, rates.natural_recovery_rate)? } else { 0.0 },
            baseline: if reversion { rate_of(tables.baseline, id, 0.0)? } else { 0.0 },
            life,
        });
    }
    Ok(found)
}

/// The groups with people in more than one zone. Everyone in such a group has a peer elsewhere,
/// and nobody in any other group does: the `any(...)` of `LifeStatePsychologyPolicy`.
fn separated_groups<'a>(people: &'a [ViewPerson], zones: &[Arc<str>]) -> FxHashMap<&'a str, bool> {
    let mut first_zone: FxHashMap<&str, &Arc<str>> = FxHashMap::default();
    let mut groups: FxHashMap<&'a str, bool> = FxHashMap::default();
    for (person, zone) in people.iter().zip(zones) {
        if let Some(group) = &person.group {
            let group: &str = group;
            let spread = groups.entry(group).or_insert(false);
            match first_zone.get(group) {
                Some(first) => *spread |= **first != *zone,
                None => {
                    first_zone.insert(group, zone);
                }
            }
        }
    }
    groups
}

/// A person's values of the next minute: what `AgentStateDelta` holds.
#[derive(Clone, Debug, PartialEq)]
pub struct Delta {
    pub agent_id: Arc<str>,
    pub minute_index: i64,
    pub toilet_urgency: f64,
    pub hunger: f64,
    pub fatigue: f64,
    pub hydration_need: f64,
    pub stress: f64,
    pub trust: f64,
    /// In the order of `BODY_NAMES`: all Some for a life-state model, all None otherwise.
    pub body: Option<[f64; 7]>,
}

/// A minute's psychology: one delta per person of the view, in its order (ID order), and with a
/// trace the rows of each person's contributions. A value of the tick (`Run.scratch`).
#[derive(Clone, Debug)]
pub struct PsychologyResult {
    pub deltas: Arc<[Delta]>,
    pub traces: Option<Arc<Traces>>,
    /// `PsychologyUpdateResult.policy_version`.
    pub policy_version: Arc<str>,
}

/// `_psychology_update` of the update phase: each person's values of the next minute, from the
/// view and the rows. `minute` is the view's, which the caller checked. Nothing changes.
pub fn update(world: &World, view: &ViewData, params: &Params<'_>, minute: i64, traced: bool) -> Result<PsychologyResult> {
    if traced && view.partial {
        return unsupported("a trace reads what a view Python made doesn't fill here");
    }
    let model = params.model;
    let life_model = matches!(model, Model::LifeState | Model::Integrated);
    let reversion = matches!(model, Model::MeanReverting | Model::Integrated);
    let viewed = read_view_people(view, params)?;

    // What the view copied from the entities, from the rows.
    let (mut inputs, trusts, zones) = {
        let rows = world.rows()?;
        if rows.agents.len() != viewed.len() {
            return unsupported("the view and the world have different people");
        }
        let mut inputs = Vec::with_capacity(viewed.len());
        let mut trusts = Vec::with_capacity(viewed.len());
        let mut zones = Vec::with_capacity(if life_model { viewed.len() } else { 0 });
        for (person, agent) in viewed.iter().zip(&view.agents) {
            let id = &*agent.id;
            let Some(row) = rows.agents.get(id) else {
                return unsupported("the view and the world have different people");
            };
            if &*row.profile.id != id {
                return unsupported("a person's id differs from its key");
            }
            if !(0.0..=1.0).contains(&row.trust) {
                return unsupported("a trust is outside 0..1");
            }
            // The checks of _mean_reverting_terms.
            if reversion
                && !((0.0..=1.0).contains(&row.stress)
                    && (0.0..=1.0).contains(&person.baseline)
                    && person.kappa.is_finite()
                    && person.kappa >= 0.0)
            {
                return unsupported("a value of the mean reversion is out of bounds");
            }
            // `request.status` of the view, which is None for an id with no request.
            let support_open = row.support_request_id.as_ref().is_some_and(|request| {
                rows.support_requests
                    .get(request)
                    .is_some_and(|request| matches!(request.status, RequestStatus::Pending | RequestStatus::Assigned))
            });
            inputs.push(Input {
                activity: row.activity,
                rest_efficiency: person.rest_efficiency,
                toilet: row.needs.toilet_urgency,
                hunger: row.needs.hunger,
                fatigue: row.needs.fatigue,
                hydration: row.needs.hydration_need,
                stress: row.stress,
                sensitivity: row.profile.stress_sensitivity,
                support_open,
                environmental_per_minute: person.environmental_per_minute,
                toilet_growth: person.toilet_growth,
                hunger_growth: person.hunger_growth,
                awake_fatigue: person.awake_fatigue,
                kappa: person.kappa,
                baseline: person.baseline,
                life: person.life,
            });
            trusts.push(row.trust);
            if life_model {
                zones.push(row.zone_id.clone());
            }
        }
        (inputs, trusts, zones)
    };
    if life_model {
        let groups = separated_groups(&viewed, &zones);
        for (input, person) in inputs.iter_mut().zip(&viewed) {
            if let Some(group) = &person.group {
                input.life.family_separated = groups.get(&**group).copied().unwrap_or(false);
            }
        }
    }

    let mut deltas = Vec::with_capacity(inputs.len());
    let mut traces = Vec::with_capacity(if traced { inputs.len() } else { 0 });
    let mut stages = Stages::default();
    for (index, ((input, trust), agent)) in inputs.iter().zip(&trusts).zip(&view.agents).enumerate() {
        let next = step(model, &params.rates, params.body.as_ref(), input, traced.then_some(&mut stages))?;
        // Values outside AgentStateDelta's bounds raise in its Python checks.
        if !within_bounds(&next) {
            return unsupported("a new value is out of bounds");
        }
        deltas.push(Delta {
            agent_id: agent.id.clone(),
            minute_index: minute,
            toilet_urgency: next.toilet,
            hunger: next.hunger,
            fatigue: next.fatigue,
            hydration_need: next.hydration,
            stress: next.stress,
            trust: *trust,
            body: next.body,
        });
        if traced {
            traces.push(person_trace(model, &params.rates, input, &next, &stages, index));
        }
    }
    let traces = traced.then(|| Arc::new(Traces { threshold: params.rates.need_pressure_threshold, rows: traces }));
    let policy_version = params.policy_version.clone().unwrap_or_else(|| Arc::from(DEFAULT_POLICY_VERSION));
    Ok(PsychologyResult { deltas: deltas.into(), traces, policy_version })
}

// ---- Writing the new values ---------------------------------------------------------------------

/// A final delta to write, as the caller read it: the six values (`NEEDS_AND_TRUST`) known to be
/// floats within 0..1, and each body value an exact float or None.
#[derive(Clone, Copy, Debug)]
pub struct Applied<'a> {
    pub agent_id: &'a str,
    /// In the order of `NEEDS_AND_TRUST`.
    pub values: [f64; 6],
    /// In the order of `BODY_NAMES`.
    pub body: [Option<f64>; 7],
}

/// The names of `Applied::values`.
pub const NEEDS_AND_TRUST: [&str; 6] = ["toilet_urgency", "hunger", "fatigue", "hydration_need", "stress", "trust"];

impl Applied<'_> {
    fn has_body(&self) -> bool {
        self.body.iter().any(Option::is_some)
    }
}

/// `apply_state_deltas`. Everything is checked before anything changes. `body_state` is whether
/// `model_state` holds `body_state_v1` (the caller refuses anything else it may hold). A person
/// whose six values all have the bits of the row is not written again.
pub fn apply(world: &World, deltas: &[Applied<'_>], body_state: bool) -> Result<()> {
    let mut seen = FxHashSet::with_capacity_and_hasher(deltas.len(), Default::default());
    for applied in deltas {
        if !seen.insert(applied.agent_id) {
            return unsupported("two deltas for one person");
        }
    }
    drop(seen);
    let any_body = deltas.iter().any(Applied::has_body);
    if any_body {
        if !body_state {
            return unsupported("the delta has body values and the world no body records");
        }
        let row = world.row::<BodyRow>(BODY_STATE)?;
        if deltas.iter().any(|applied| applied.has_body() && !row.agents.contains_key(applied.agent_id)) {
            return unsupported("the delta has body values and the person no body record");
        }
    }

    // Every person first: from here on nothing may be refused.
    {
        let mut rows = world.rows_mut()?;
        let mut changed = Vec::with_capacity(deltas.len());
        for applied in deltas {
            let Some(row) = rows.agents.get(applied.agent_id) else {
                return unsupported("a delta names nobody in the world");
            };
            let current =
                [row.needs.toilet_urgency, row.needs.hunger, row.needs.fatigue, row.needs.hydration_need, row.stress, row.trust];
            changed.push(current.iter().zip(&applied.values).any(|(now, next)| now.to_bits() != next.to_bits()));
        }
        for (applied, changed) in deltas.iter().zip(changed) {
            if !changed {
                continue;
            }
            let Some(row) = rows.agents.get_mut(applied.agent_id) else {
                return unsupported("a delta names nobody in the world");
            };
            let [toilet, hunger, fatigue, hydration, stress, trust] = applied.values;
            row.needs.toilet_urgency = toilet;
            row.needs.hunger = hunger;
            row.needs.fatigue = fatigue;
            row.needs.hydration_need = hydration;
            row.stress = stress;
            row.trust = trust;
        }
    }

    if any_body {
        let mut row = world.row_mut::<BodyRow>(BODY_STATE)?;
        for applied in deltas.iter().filter(|applied| applied.has_body()) {
            let Some(record) = row.agents.get_mut(applied.agent_id) else {
                return Err(Error::Runtime("a body record left the row".into()));
            };
            let fields = [
                &mut record.sleep_debt_minutes,
                &mut record.uncertainty,
                &mut record.reassurance,
                &mut record.discontent,
                &mut record.positive_affect,
                &mut record.negative_affect,
                &mut record.hygiene_need,
            ];
            for (field, value) in fields.into_iter().zip(&applied.body) {
                if let Some(value) = value {
                    *field = *value;
                }
            }
        }
    }
    Ok(())
}
