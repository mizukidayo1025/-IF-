//! `staff_dynamics_v1`: shift and fatigue memory of each staff member. The rules that write and
//! read it are `rules/staff_dynamics.rs`; `domain/staff_dynamics.py` keeps the checks of a config
//! and of a saved namespace (docs/rust-port/lanes/s-staff-dynamics.md). The PyO3 crate's
//! `state/staff_dynamics.rs` reads it from Python and holds its validator.

use crate::error::{Misfit, Result};
use crate::json::Encoder;
use crate::row::{Field, IdMap, Maybe};
use crate::value::V;
use crate::{state_row, str_enum};

pub const NAME: &str = "staff_dynamics_v1";
pub const POLICY_V1: &str = "continuous-duty-fatigue-v1";
pub const POLICY_V2: &str = "continuous-duty-fatigue-v2";
pub const MINUTES_PER_DAY: i64 = 24 * 60;
pub const MAX_REST_MINUTES: i64 = 8 * 60;
pub const MINUTES_PER_WEEK: i64 = 10_080;
pub const MAX_SHIFTS: usize = 21;

str_enum! {
    pub enum LastActivity { Working = "working", Resting = "resting", OffDuty = "off_duty" }
}

state_row! {
    #[derive(Copy)]
    pub struct StaffRecord {
        consecutive_work_minutes: i64 => set_consecutive_work_minutes,
        total_work_minutes: i64 => set_total_work_minutes,
        total_rest_minutes: i64 => set_total_rest_minutes,
        fatigue: f64 => set_fatigue,
        rest_until_minute: i64 => set_rest_until_minute,
        last_activity: LastActivity => set_last_activity,
    }
}

state_row! {
    #[derive(Copy)]
    pub struct Shift {
        start_minute: i64,
        end_minute: i64,
    }
}

/// The off-shift recovery as a config holds it: a float, or an int (0 or 1, its range) that the
/// Python rule's arithmetic used as the float of the same value. The tree keeps the int, and so does
/// the row, for the bytes.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Recovery {
    pub value: f64,
    int: bool,
}

impl Recovery {
    /// A float of the config.
    pub fn float(value: f64) -> Self {
        Self { value, int: false }
    }

    /// An int of the config, which a float holds exactly.
    pub fn int(value: i64) -> Self {
        Self { value: value as f64, int: true }
    }

    /// Whether the config holds an int.
    pub fn is_int(&self) -> bool {
        self.int
    }
}

impl Field for Recovery {
    fn canonicalize(&mut self) {}

    /// A float, or an int that a float holds exactly.
    fn from_value(value: &V) -> std::result::Result<Self, Misfit> {
        match value {
            V::Float(number) => Ok(Self::float(*number)),
            V::Int(number) if number.unsigned_abs() > 1 << 53 => Err(Misfit::new("an int a float can't hold")),
            V::Int(number) => Ok(Self::int(*number)),
            V::Big(_) => Err(Misfit::new("an int a float can't hold")),
            _ => Err(Misfit::new("not a float or an int")),
        }
    }

    fn write_json(&self, encoder: &mut Encoder) -> Result<()> {
        if self.int {
            encoder.int(self.value as i64);
            Ok(())
        } else {
            encoder.try_float(self.value)
        }
    }
}

state_row! {
    // A world keeps this as `canonical_staff_dynamics_config` left it, less the keys below that
    // an older schema of the namespace did not have.
    pub struct ConfigRow {
        schema_version: i64,
        policy_version: std::sync::Arc<str>,
        source_id: std::sync::Arc<str>,
        max_continuous_work_minutes: i64,
        max_continuous_work_minutes_by_staff: Maybe<IdMap<i64>>,
        rest_minutes: i64,
        shift_windows_by_staff: Maybe<IdMap<Vec<Shift>>>,
        off_shift_fatigue_recovery_per_minute: Maybe<Recovery>,
    }
}

state_row! {
    pub struct StaffDynamicsRow {
        schema_version: i64,
        policy_version: std::sync::Arc<str>,
        source_id: std::sync::Arc<str>,
        config: ConfigRow,
        staff: IdMap<StaffRecord>,
    }
}

impl StaffRecord {
    /// A new member: `create_staff_dynamics_state` and `add_staff`.
    pub fn new() -> Self {
        Self {
            consecutive_work_minutes: 0,
            total_work_minutes: 0,
            total_rest_minutes: 0,
            fatigue: 0.0,
            rest_until_minute: 0,
            last_activity: LastActivity::OffDuty,
        }
    }

    pub fn is_valid(&self) -> bool {
        self.consecutive_work_minutes >= 0
            && self.total_work_minutes >= 0
            && self.total_rest_minutes >= 0
            && self.rest_until_minute >= 0
            && (0.0..=1.0).contains(&self.fatigue)
    }
}

/// `[start, end)` intervals of one person the Python rule passes: non-empty, in the week, at most
/// `MAX_SHIFTS`, and none overlapping.
pub fn shifts_are_valid(shifts: &[Shift]) -> bool {
    if shifts.len() > MAX_SHIFTS
        || !shifts.iter().all(|shift| {
            0 <= shift.start_minute
                && shift.start_minute < shift.end_minute
                && shift.end_minute <= MINUTES_PER_WEEK
        })
    {
        return false;
    }
    let mut ordered: Vec<(i64, i64)> =
        shifts.iter().map(|shift| (shift.start_minute, shift.end_minute)).collect();
    ordered.sort_unstable();
    ordered.windows(2).all(|pair| pair[0].1 <= pair[1].0)
}

impl ConfigRow {
    /// Whether `canonical_staff_dynamics_config` passes this and leaves it as it is, as far as
    /// `validate_staff_dynamics_state` asks: with all its keys, or without the per-staff limits,
    /// or for policy v1 without any of the three later keys.
    pub fn is_valid(&self) -> bool {
        let v1 = &*self.policy_version == POLICY_V1;
        if self.schema_version != 1
            || !(v1 || &*self.policy_version == POLICY_V2)
            || self.source_id.is_empty()
            || !(1..=MINUTES_PER_DAY).contains(&self.max_continuous_work_minutes)
            || !(1..=MAX_REST_MINUTES).contains(&self.rest_minutes)
        {
            return false;
        }
        let limits_are_valid = self.max_continuous_work_minutes_by_staff.0.as_ref().is_none_or(|limits| {
            limits
                .iter()
                .all(|(id, limit)| !id.is_empty() && (1..=MINUTES_PER_DAY).contains(limit))
        });
        if !limits_are_valid {
            return false;
        }
        match (&self.shift_windows_by_staff.0, self.off_shift_fatigue_recovery_per_minute.0.map(|recovery| recovery.value)) {
            (Some(windows), Some(recovery)) => {
                (0.0..=1.0).contains(&recovery)
                    && (!v1 || (windows.is_empty() && recovery == 0.0))
                    && windows
                        .iter()
                        .all(|(id, shifts)| !id.is_empty() && shifts_are_valid(shifts))
            }
            (None, None) => v1 && self.max_continuous_work_minutes_by_staff.0.is_none(),
            _ => false,
        }
    }
}

impl StaffDynamicsRow {
    pub fn is_valid_without_staff_ids(&self) -> bool {
        self.schema_version == 1
            && self.config.is_valid()
            && self.policy_version == self.config.policy_version
            && self.source_id == self.config.source_id
            && self.staff.values().all(StaffRecord::is_valid)
    }
}
