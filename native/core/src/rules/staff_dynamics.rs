//! `domain/staff_dynamics.py`: staff shift, rest and fatigue, on the rows of a `World` (lane L10,
//! `kernel_only`; moved to the core in P8b-1). What they refuse is a KernelRefusal at the Python
//! entry. The checks of a Scenario's config and of an override stay in Python; the PyO3 crate's
//! `rules/staff_dynamics.rs` keeps `canonical_staff_dynamics_config` and the state of a world built
//! from a Scenario (decision 4), and hands in what Python holds: the fatigue formula of the configs
//! (docs/rust-port/lanes/s-staff-dynamics.md).
//!
//! `update` plans everything from the rows first, so an Unsupported changes nothing; it then writes
//! the row of `staff_dynamics_v1` alone, and the tree follows the row when the world is given back
//! (lane P8-0c). Every other function only reads, or builds a new value.
//!
//! `operational_events_v1` and `staff_operations_v1` are read from their rows. A namespace whose row
//! a write that did not fit dropped (tainted) is refused: its tree is all there is of it.

use std::sync::Arc;

use crate::cell::ShardRef;
use crate::config::read::Read;
use crate::config::staff::StaffFatigue;
use crate::error::{Error, Result};
use crate::mathx::{exp, py_max, py_min};
use crate::row::Row;
use crate::state::staff_dynamics::{ConfigRow, LastActivity, StaffDynamicsRow, StaffRecord, MINUTES_PER_WEEK, NAME};
use crate::state::staff_operations::{Phase, StaffOperationsRow, NAME as OPERATIONS};
use crate::value::V;
use crate::world::World;

/// `operational_events_v1`, whose `staff_status` says who is unavailable.
pub const OPERATIONAL: &str = "operational_events_v1";
/// Above this an int does not always fit a float, and Python's int / int is correctly rounded.
const FLOAT_EXACT_INT: u64 = 1 << 53;

// ---- What the rules read of a namespace -------------------------------------------------------------

/// What the world holds of a namespace with a typed row.
pub enum Held<T> {
    /// No such name, or None there: what `model_state.get(name)` reads as None.
    Absent,
    Row(T),
    /// A write that did not fit dropped the row: the tree is all there is of it.
    Tainted,
}

/// The row of a namespace of the world, or what it holds instead.
pub fn held_row<'w, T: Row>(world: &'w World, name: &str) -> Result<Held<ShardRef<'w, T>>> {
    let Some(slot) = world.slots.get(name) else {
        return Ok(Held::Absent);
    };
    {
        let slot = slot.try_borrow()?;
        if slot.row.is_none() {
            return Ok(if slot.taint.is_none() { Held::Absent } else { Held::Tainted });
        }
    }
    world.row::<T>(name).map(Held::Row)
}

/// The row of a namespace of the world, as a Python rule's `model_state.get(name)` reads it: None
/// for a name the world does not have (or holds as None), Unsupported for a namespace a write that
/// did not fit tainted, whose tree the rules can't read here.
#[track_caller]
pub fn namespace<'w, T: Row>(world: &'w World, name: &str) -> Result<Option<ShardRef<'w, T>>> {
    match held_row::<T>(world, name)? {
        Held::Absent => Ok(None),
        Held::Row(row) => Ok(Some(row)),
        Held::Tainted => Err(Error::unsupported("the namespace has no row")),
    }
}

/// A section of `model_configs` as its read site reads it: a refusal keeps its reason, any other
/// fault is a state no read site lets through (`configs::read` of the PyO3 crate).
#[track_caller]
pub fn config_read<T>(value: Read<T>) -> Result<T> {
    value.map_err(|fault| {
        if fault.is_refusal() {
            Error::unsupported(fault.text.to_string())
        } else {
            Error::corrupt("model_configs", "a value of a section holds an error")
        }
    })
}

// ---- What the rules read from the rest of the world -------------------------------------------------

/// `state["staff_status"]` of `operational_events_v1`, or None without that namespace (or with None
/// there): the input of `staff_is_available` in `domain/operational.py`, handed to `f`.
pub fn with_staff_status<R>(world: &World, f: impl FnOnce(Option<&V>) -> Result<R>) -> Result<R> {
    let state = world.slots.try_value(OPERATIONAL)?;
    let status = match state.as_deref() {
        None | Some(V::Null) => None,
        Some(V::Map(state)) => match state.get("staff_status") {
            Some(status @ V::Map(_)) => Some(status),
            Some(_) => return Err(Error::unsupported("a value is not a plain dict")),
            None => return Err(Error::unsupported("operational_events_v1 has no staff_status")),
        },
        Some(_) => return Err(Error::unsupported("a value is not a plain dict")),
    };
    f(status)
}

/// `staff_is_available` of one member, given `staff_status` (`with_staff_status`).
pub fn is_available(status: Option<&V>, id: &str) -> Result<bool> {
    let Some(V::Map(status)) = status else {
        return Ok(true);
    };
    match status.get(id) {
        None | Some(V::Null) => Ok(true),
        Some(V::Map(record)) => match record.get("available") {
            Some(V::Bool(available)) => Ok(*available),
            Some(_) => Err(Error::unsupported("available is not a bool")),
            None => Err(Error::unsupported("a staff status has no available")),
        },
        Some(_) => Err(Error::unsupported("a value is not a plain dict")),
    }
}

/// `_on_shift`. A config of the oldest schema has no windows, and the Python rule raises.
pub fn on_shift(config: &ConfigRow, id: &str, minute: i64) -> Result<bool> {
    let Some(windows) = &config.shift_windows_by_staff.0 else {
        return Err(Error::unsupported("the config has no shift windows"));
    };
    let Some(shifts) = windows.get(id) else {
        return Ok(true);
    };
    let week_minute = minute.rem_euclid(MINUTES_PER_WEEK);
    Ok(shifts.iter().any(|shift| shift.start_minute <= week_minute && week_minute < shift.end_minute))
}

// ---- The fatigue formula -----------------------------------------------------------------------

/// `StaffFatigueFormula.fatigue`, or None where Python's result isn't sure to be f64 division.
#[inline]
pub fn fatigue(saturating: bool, consecutive: i64, maximum: i64) -> Option<f64> {
    if maximum == 0 || consecutive.unsigned_abs() > FLOAT_EXACT_INT || maximum.unsigned_abs() > FLOAT_EXACT_INT {
        return None;
    }
    let fraction = consecutive as f64 / maximum as f64;
    if saturating {
        return Some(1.0 - exp(-fraction).ok()?);
    }
    Some(py_min(1.0, fraction))
}

// ---- update_staff_dynamics ---------------------------------------------------------------------

/// What one tick needs to know of one member, read before the row is.
struct Member {
    id: Arc<str>,
    working: bool,
    /// `staff_is_available`; only asked of who has no task.
    available: bool,
    maximum: Option<i64>,
}

struct Parameters {
    saturating: bool,
    default_maximum: i64,
    rest_minutes: i64,
    recovery: Option<f64>,
    duration: i64,
    minute: i64,
}

/// One member's next record: the loop body of `update_staff_dynamics`. None when an int leaves
/// what the kernel holds or a config key the Python rule reads is not there.
fn next_record(record: &StaffRecord, working: bool, available_on_shift: bool, maximum: i64, parameters: &Parameters) -> Option<StaffRecord> {
    let mut next = *record;
    if working {
        next.consecutive_work_minutes = next.consecutive_work_minutes.checked_add(1)?;
        next.total_work_minutes = next.total_work_minutes.checked_add(1)?;
        next.fatigue = fatigue(parameters.saturating, next.consecutive_work_minutes, maximum)?;
        next.last_activity = LastActivity::Working;
        return Some(next);
    }
    if !available_on_shift {
        next.last_activity = LastActivity::OffDuty;
        next.consecutive_work_minutes = 0;
        next.total_rest_minutes = next.total_rest_minutes.checked_add(1)?;
        next.fatigue = py_max(0.0, next.fatigue - parameters.recovery?);
        return Some(next);
    }
    if parameters.minute < next.rest_until_minute {
        next.total_rest_minutes = next.total_rest_minutes.checked_add(1)?;
        next.last_activity = LastActivity::Resting;
        return Some(next);
    }
    if next.rest_until_minute != 0 {
        next.consecutive_work_minutes = 0;
        next.fatigue = 0.0;
        next.rest_until_minute = 0;
    }
    if next.consecutive_work_minutes >= maximum {
        next.rest_until_minute = parameters.duration.min(parameters.minute.checked_add(parameters.rest_minutes)?);
        next.last_activity = LastActivity::Resting;
    } else {
        next.last_activity = LastActivity::OffDuty;
    }
    Some(next)
}

fn plan(row: &StaffDynamicsRow, members: &[Member], parameters: &Parameters) -> Result<Vec<StaffRecord>> {
    let mut planned = Vec::with_capacity(members.len());
    for member in members {
        let Some(record) = row.staff.get(&*member.id) else {
            return Err(Error::unsupported("a member has no record"));
        };
        let maximum = member.maximum.unwrap_or(parameters.default_maximum);
        // In the Python rule's order: _on_shift is only asked of who has no task and is available.
        let available_on_shift = !member.working && member.available && on_shift(&row.config, &member.id, parameters.minute)?;
        let Some(next) = next_record(record, member.working, available_on_shift, maximum, parameters) else {
            return Err(Error::unsupported("a value leaves what this kernel holds"));
        };
        planned.push(next);
    }
    Ok(planned)
}

/// `update_staff_dynamics` of a world that has the namespace, with the fatigue formula of its
/// configs (`create_staff_fatigue_formula`). The staff go in the order of `world.staff`.
pub fn update(world: &mut World, minute: i64, formula: &StaffFatigue) -> Result<()> {
    let duration = world.duration_minutes()?;
    let members = with_staff_status(world, |status| {
        let rows = world.rows()?;
        let mut members = Vec::with_capacity(rows.staff.len());
        for (id, row) in rows.staff.iter() {
            let working = row.active_task_id.is_some();
            let maximum = config_read(formula.limit(id))?;
            let available = working || is_available(status, id)?;
            members.push(Member { id: id.clone(), working, available, maximum });
        }
        Ok(members)
    })?;
    let planned = {
        let row = world.row::<StaffDynamicsRow>(NAME)?;
        let parameters = Parameters {
            saturating: formula.saturating,
            default_maximum: row.config.max_continuous_work_minutes,
            rest_minutes: row.config.rest_minutes,
            recovery: row.config.off_shift_fatigue_recovery_per_minute.0.map(|recovery| recovery.value),
            duration,
            minute,
        };
        plan(&row, &members, &parameters)?
    };
    let row = world.slots.row_mut::<StaffDynamicsRow>(NAME)?;
    for (member, next) in members.iter().zip(planned) {
        let Some(record) = row.staff.get_mut(&*member.id) else {
            return Err(Error::Runtime("the row changed under the plan".to_owned()));
        };
        *record = next;
    }
    Ok(())
}

// ---- The readers --------------------------------------------------------------------------------

/// `staff_dynamics_on_shift` of a world that has the namespace.
pub fn on_shift_now(world: &World, staff_id: &str, minute: i64) -> Result<bool> {
    let row = world.row::<StaffDynamicsRow>(NAME)?;
    let Some(record) = row.staff.get(staff_id) else {
        return Ok(false);
    };
    Ok(on_shift(&row.config, staff_id, minute)? && minute >= record.rest_until_minute)
}

/// `operation is None or operation.get("phase") == "idle"` of `staff_operations_v1`: a record of
/// the row always has a phase. Every rule writes the namespace as its row (lanes b2 and b3), so a
/// namespace whose row a write that did not fit dropped (tainted) is refused. `absent` says the
/// caller read None under the key (no state), which no row holds.
pub fn operation_is_idle(world: &World, staff_id: &str, absent: bool) -> Result<bool> {
    if absent {
        return Ok(true);
    }
    Ok(match namespace::<StaffOperationsRow>(world, OPERATIONS)? {
        None => true,
        Some(row) => row.staff.get(staff_id).is_none_or(|record| record.phase == Phase::Idle),
    })
}

/// `staff_dynamics_is_available` of a world that has the namespace: the operation is read first,
/// as the Python rule reads it, and the status last. `operations_absent` as `operation_is_idle`.
pub fn is_available_now(world: &World, staff_id: &str, minute: i64, operations_absent: bool) -> Result<bool> {
    let idle = operation_is_idle(world, staff_id, operations_absent)?;
    if !on_shift_now(world, staff_id, minute)? {
        return Ok(false);
    }
    Ok(with_staff_status(world, |status| is_available(status, staff_id))? && idle)
}

// ---- add_staff ----------------------------------------------------------------------------------

/// `add_staff` of a world that has the namespace: one new record, or the member's record made new.
pub fn add_staff(world: &mut World, staff_id: &str) -> Result<()> {
    let row = world.slots.row_mut::<StaffDynamicsRow>(NAME)?;
    let record = StaffRecord::new();
    match row.staff.get_mut(staff_id) {
        Some(slot) => *slot = record,
        None => {
            row.staff.insert(Arc::from(staff_id), record);
        }
    }
    Ok(())
}
