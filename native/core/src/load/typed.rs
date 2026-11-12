//! The typed row of each namespace of `model_state` (#225, F1), for a world read from JSON.
//!
//! The PyO3 crate has no such table: Python's `OWNED_NAMESPACES` (`core/state_mirror.py`) names
//! each namespace's module, and the module's `ROW` (`native/src/state/<module>.rs`) is the core's
//! row that the slot then holds, in `Place::Namespace`. This is the same table for the namespaces
//! whose row is typed; the others (the population's, the temporary spaces', the overlay's) and the
//! keys no lane owns are `ValueRow`s. `tests/data/typed_rows.json` lists the typed names Python has
//! (`tests/support/core_typed_rows.py`), and `tests/typed_rows.rs` checks this table is that list.

use crate::error::Misfit;
use crate::row::{Field, Row};
use crate::state::agent_dynamics::AgentDynamicsRow;
use crate::state::body_state::BodyRow;
use crate::state::effect_program::EffectProgramRow;
use crate::state::facility_queues::QueuesRow;
use crate::state::information::InformationRow;
use crate::state::movement::MovementRow;
use crate::state::psychological_distress::DistressRow;
use crate::state::resource_ledger::LedgerRow;
use crate::state::resource_service::ServiceRow;
use crate::state::shared_operations::SharedOperationsRow;
use crate::state::social_network::SocialNetworkRow;
use crate::state::social_propagation::SocialRow;
use crate::state::staff_dynamics::StaffDynamicsRow;
use crate::state::staff_operations::StaffOperationsRow;
use crate::value::V;

fn read<T: Field>(value: &V) -> Result<Box<dyn Row>, Misfit> {
    Ok(Box::new(T::from_value(value)?))
}

/// The reader of a typed namespace's row.
pub type ReadRow = fn(&V) -> Result<Box<dyn Row>, Misfit>;

/// Each typed namespace and the reader of its row, in the order of `OWNED_NAMESPACES`.
pub const TYPED: [(&str, ReadRow); 14] = [
    ("psychological_distress_v1", read::<DistressRow>),
    ("staff_dynamics_v1", read::<StaffDynamicsRow>),
    ("information_spread_v1", read::<InformationRow>),
    ("social_propagation_v1", read::<SocialRow>),
    ("movement_v1", read::<MovementRow>),
    ("body_state_v1", read::<BodyRow>),
    ("social_network_v1", read::<SocialNetworkRow>),
    ("agent_dynamics_v1", read::<AgentDynamicsRow>),
    ("staff_operations_v1", read::<StaffOperationsRow>),
    ("shared_operations_v1", read::<SharedOperationsRow>),
    ("resource_ledger_v1", read::<LedgerRow>),
    ("resource_service_v1", read::<ServiceRow>),
    ("effect_program_v1", read::<EffectProgramRow>),
    ("facility_queues_v1", read::<QueuesRow>),
];

/// The typed row of a namespace read from its value, or None for a namespace whose row is a
/// `ValueRow`. A value that does not fit is the misfit that would taint the namespace in the
/// PyO3 crate.
pub fn typed_row(name: &str, value: &V) -> Option<Result<Box<dyn Row>, Misfit>> {
    TYPED.iter().find(|(typed, _)| *typed == name).map(|(_, read)| read(value))
}
