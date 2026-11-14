//! `agent_dynamics_v1`: each agent's decision memory and known facilities. The rules that write and
//! read it are `rules/agent_dynamics.rs`; `domain/agent_dynamics.py` keeps the validation of a saved
//! namespace (docs/rust-port/lanes/f-action-lifecycle.md).
//!
//! The PyO3 crate's `state/agent_dynamics.rs` reads it from Python, reads its config and holds its
//! validator.

use std::sync::Arc;

use crate::entities::Entities;
use crate::row::IdMap;
use crate::state_row;

pub const NAME: &str = "agent_dynamics_v1";
pub const POLICY_V1: &str = "need-led-autonomy-memory-v1";
pub const POLICY_V2: &str = "need-led-autonomy-memory-v2";
pub const POLICY_V3: &str = "need-led-autonomy-memory-v3";
pub const ORIENTATION_SOURCE: &str = "ASM-INTAKE-FACILITY-ORIENTATION-V1";

state_row! {
    pub struct AgentMemory {
        known_facility_ids: Vec<Arc<str>>,
        last_action: Option<Arc<str>> => set_last_action,
        last_target_id: Option<Arc<str>> => set_last_target_id,
        last_decision_minute: Option<i64> => set_last_decision_minute,
        decision_due_minute: i64 => set_decision_due_minute,
        decision_reason: Option<Arc<str>> => set_decision_reason,
    }
}

state_row! {
    pub struct AgentDynamicsRow {
        schema_version: i64,
        policy_version: Arc<str>,
        agents: IdMap<AgentMemory>,
    }
}

impl AgentMemory {
    /// The per-record rules of `validate_agent_dynamics_state`, for a record already of its types.
    pub fn is_valid(&self, is_facility: impl Fn(&str) -> bool) -> bool {
        self.decision_due_minute >= 0
            && self.last_decision_minute.is_none_or(|minute| minute >= 0)
            && self.known_facility_ids.iter().all(|id| is_facility(id))
            && has_no_repeats(&self.known_facility_ids)
    }
}

impl AgentDynamicsRow {
    /// `validate_agent_dynamics_state` of a world whose configs select this policy, for a row of its
    /// types (#152; in the core since #250): true only if the Python rule passes. A record for each
    /// Agent and none else, each valid and knowing only facilities of the world.
    pub fn accepts(&self, policy: &str, rows: &Entities) -> bool {
        if self.schema_version != 1 || &*self.policy_version != policy {
            return false;
        }
        self.agents.len() == rows.agents.len()
            && self.agents.iter().all(|(id, record)| rows.agents.get(id).is_some() && record.is_valid(|id| rows.facilities.get(id).is_some()))
    }
}

/// Nearly always a sorted list, which needs no set.
fn has_no_repeats(ids: &[Arc<str>]) -> bool {
    if ids.windows(2).all(|pair| pair[0] < pair[1]) {
        return true;
    }
    let mut ordered: Vec<&Arc<str>> = ids.iter().collect();
    ordered.sort_unstable();
    ordered.windows(2).all(|pair| pair[0] != pair[1])
}
