//! `effect_program_v1`: the approved programs of a Run and the stack of availability modifiers of
//! each facility (`events/effects.py`). `rules/effects.rs` reads it and writes it one record at a
//! time. `schedule_effect_program` adds a program through the forwarded writes, on the copy it
//! makes of the world (docs/rust-port/lanes/m6-effects.md).
//!
//! A value of another type than the rules write taints the namespace, and the Python rules and
//! their errors run on the tree.

use std::sync::Arc;

use crate::row::IdMap;
use crate::state_row;
use crate::value::V;

pub const NAME: &str = "effect_program_v1";

state_row! {
    pub struct Modifier {
        modifier_id: Arc<str>,
        program_id: Arc<str>,
        effect_id: Arc<str>,
        working_units: i64,
        start_minute: i64,
        sequence: i64,
    }
}

state_row! {
    pub struct FacilityModifiers {
        baseline_working_units: i64,
        modifiers: Vec<Modifier>,
    }
}

state_row! {
    pub struct Program {
        program_hash: Arc<str>,
        source_text_hash: Arc<str>,
        narrative: Arc<str>,
        compiler_provenance: V,
        assumptions: V,
        support_mode: Arc<str>,
        effect_ids: Vec<Arc<str>>,
        operators: IdMap<Arc<str>>,
        scheduled_event_ids: Vec<Arc<str>>,
    }
}

state_row! {
    pub struct EffectProgramRow {
        schema_version: i64,
        programs: IdMap<Program>,
        active_facility_modifiers: IdMap<FacilityModifiers>,
    }
}
