//! `effect_program_v1`: the row is the core's (`sheltersim_core::state::effect_program`); here is
//! its Python side.

use pyo3::prelude::*;

pub(crate) use sheltersim_core::state::effect_program::*;

use crate::core::row::{py_row};
use crate::core::world_core::RowType;

py_row! { Modifier { modifier_id, program_id, effect_id, working_units, start_minute, sequence } }
py_row! { FacilityModifiers { baseline_working_units, modifiers } }
py_row! {
    Program {
        program_hash, source_text_hash, narrative, compiler_provenance, assumptions, support_mode,
        effect_ids, operators, scheduled_event_ids,
    }
}
py_row! { EffectProgramRow { schema_version, programs, active_facility_modifiers } }

pub fn register(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add("ROW", Py::new(m.py(), RowType::of::<EffectProgramRow>(NAME))?)?;
    Ok(())
}
