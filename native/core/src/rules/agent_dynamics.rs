//! `domain/agent_dynamics.py`: decision memory and facility awareness. Its rules are these kernels
//! alone (lane L10, `kernel_only`), and what they refuse is a KernelRefusal there
//! (docs/rust-port/lanes/f-action-lifecycle.md). Moved to the core in lane P8 a2.
//!
//! Each kernel reads the rows of the world and the config sections the PyO3 crate read for it
//! (`kit::Kept`), computes, and writes the row of `agent_dynamics_v1` alone: its tree, if Python
//! reads one, follows the row when the world is given back (lane P8-0c).

use std::collections::BTreeSet;
use std::sync::Arc;

use crate::config::agent_dynamics::{self, Awareness};
use crate::config::SectionId;
use crate::host::Host;
use crate::entities::{AgentRow, Entities};
use crate::error::{Error, Result};
use crate::row::IdMap;
use crate::rules::kit::{self, Kept};
use crate::state::agent_dynamics::{AgentDynamicsRow, AgentMemory, NAME, POLICY_V1, POLICY_V3};
use crate::world::{World, WorldCell};

/// `ORIENTATION_FACILITY_KINDS`.
const ORIENTATION_KINDS: [&str; 6] =
    ["intake", "meal_distribution", "medical_support", "quiet_space", "toilet", "water_station"];

/// What `record_proposal` stores of an applied decision.
pub struct Decision<'a> {
    pub agent_id: &'a str,
    pub action: &'a str,
    pub target_id: Option<&'a str>,
    pub minute: i64,
    pub next_decision_at: i64,
    pub trigger: Option<&'a str>,
}

/// An ID the PyO3 crate read: None for a value that is not an exact str, refused where the Python
/// rule would first use it.
#[track_caller]
fn id_of(id: Option<&str>) -> Result<&str> {
    id.ok_or_else(|| Error::unsupported("a value is not a str"))
}

/// `record_proposal`: the agent's record takes the decision; an agent without a record has none.
/// Unsupported without the namespace's row, for the Python rule to create it.
pub fn remember_decision(world: &World, decision: &Decision<'_>) -> Result<()> {
    if !world.row::<AgentDynamicsRow>(NAME)?.agents.contains_key(decision.agent_id) {
        return Ok(());
    }
    let mut row = world.row_mut::<AgentDynamicsRow>(NAME)?;
    let Some(record) = row.agents.get_mut(decision.agent_id) else {
        return Ok(());
    };
    record.last_action = Some(Arc::from(decision.action));
    record.last_target_id = decision.target_id.map(Arc::from);
    record.last_decision_minute = Some(decision.minute);
    record.decision_due_minute = decision.next_decision_at;
    record.decision_reason = decision.trigger.map(Arc::from);
    Ok(())
}

/// `agent_knows_facility`: always true while knowledge is not limited; false without the
/// namespace.
pub fn knows_facility(world: &World, limited: &Kept<bool>, agent_id: Option<&str>, facility_id: Option<&str>) -> Result<bool> {
    if !*kit::kept(limited)? {
        return Ok(true);
    }
    if kit::namespace_absent(world, NAME)? {
        return Ok(false);
    }
    let (agent_id, facility_id) = (id_of(agent_id)?, id_of(facility_id)?);
    let row = world.row::<AgentDynamicsRow>(NAME)?;
    Ok(row
        .agents
        .get(agent_id)
        .is_some_and(|record| record.known_facility_ids.iter().any(|id| &**id == facility_id)))
}

// ---- What a new agent knows ---------------------------------------------------------------------

/// `_new_agent_dynamics_record`.
fn new_memory(rows: &Entities, agent: &AgentRow, oriented: bool, awareness: Option<&Awareness>) -> AgentMemory {
    let mut known: BTreeSet<Arc<str>> = rows
        .facilities
        .iter()
        .filter(|(_, facility)| {
            facility.zone_id == agent.zone_id || (oriented && ORIENTATION_KINDS.contains(&&*facility.kind))
        })
        .map(|(id, _)| id.clone())
        .collect();
    if let Some(awareness) = awareness {
        known.extend(awareness.initial.iter().cloned());
    }
    AgentMemory {
        known_facility_ids: known.into_iter().collect(),
        last_action: None,
        last_target_id: None,
        last_decision_minute: None,
        decision_due_minute: agent.next_decision_at,
        decision_reason: None,
    }
}

/// `create_agent_dynamics_state`: a record for each agent, in ID order.
pub fn create_state(world: &World, policy: &'static str, awareness: Option<&Awareness>) -> Result<AgentDynamicsRow> {
    let rows = world.rows()?;
    let mut ids: Vec<Arc<str>> = rows.agents.iter().map(|(id, _)| id.clone()).collect();
    ids.sort_unstable();
    let mut agents = IdMap::default();
    for id in ids {
        let Some(agent) = rows.agents.get(&id) else {
            return Err(Error::unsupported("an agent has no row"));
        };
        agents.insert(id, new_memory(&rows, agent, policy != POLICY_V1, awareness));
    }
    Ok(AgentDynamicsRow { schema_version: 1, policy_version: Arc::from(policy), agents })
}

/// What `add_agent` refuses for a person about to be added, for a plan that adds one: the configs,
/// and a namespace without its row.
pub fn check_add_agent(world: &World, policy: &Kept<&'static str>, awareness: &Kept<Option<Awareness>>) -> Result<()> {
    kit::kept(policy)?;
    kit::kept(awareness)?;
    world.row::<AgentDynamicsRow>(NAME).map(drop)
}

/// `add_agent`: one new record, or the record made again, in its place.
pub fn add_agent(
    world: &World,
    agent_id: Option<&str>,
    policy: &Kept<&'static str>,
    awareness: &Kept<Option<Awareness>>,
) -> Result<()> {
    if kit::namespace_absent(world, NAME)? {
        return Ok(());
    }
    let id = id_of(agent_id)?;
    // Python asks these only once the agent is there, so a bad config with no agent is no error.
    if world.rows()?.agents.get(id).is_none() {
        return Ok(());
    }
    let policy = *kit::kept(policy)?;
    let awareness = kit::kept(awareness)?.as_ref();
    world.row::<AgentDynamicsRow>(NAME)?;
    let record = {
        let rows = world.rows()?;
        let Some(agent) = rows.agents.get(id) else {
            return Err(Error::unsupported("an agent has no row"));
        };
        new_memory(&rows, agent, policy != POLICY_V1, awareness)
    };
    let mut row = world.row_mut::<AgentDynamicsRow>(NAME)?;
    match row.agents.get_mut(id) {
        Some(slot) => *slot = record,
        None => {
            row.agents.insert(Arc::from(id), record);
        }
    }
    Ok(())
}

// ---- What an agent learns -------------------------------------------------------------------------

/// `learn_facility`: the facility, and with `discover` every facility of its zone. A list that
/// would come out the same is not written.
pub fn learn_facility(
    world: &World,
    agent_id: Option<&str>,
    facility_id: Option<&str>,
    awareness: &Kept<Option<Awareness>>,
) -> Result<()> {
    if kit::namespace_absent(world, NAME)? {
        return Ok(());
    }
    let (agent_id, facility_id) = (id_of(agent_id)?, id_of(facility_id)?);
    let Some(zone) = world.rows()?.facilities.get(facility_id).map(|facility| facility.zone_id.clone()) else {
        return Ok(());
    };
    if !world.row::<AgentDynamicsRow>(NAME)?.agents.contains_key(agent_id) {
        return Ok(());
    }
    // Asked after the record, as the Python rule does, so a bad config with no record is no error.
    let discover = kit::kept(awareness)?.as_ref().is_some_and(|awareness| awareness.discover);
    let mut discovered: Vec<Arc<str>> = vec![Arc::from(facility_id)];
    if discover {
        let rows = world.rows()?;
        discovered.extend(rows.facilities.iter().filter(|(_, facility)| facility.zone_id == zone).map(|(id, _)| id.clone()));
    }
    let next = {
        let row = world.row::<AgentDynamicsRow>(NAME)?;
        let Some(record) = row.agents.get(agent_id) else {
            return Err(Error::unsupported("the row changed under the plan"));
        };
        let mut next: Vec<Arc<str>> = record.known_facility_ids.iter().chain(&discovered).cloned().collect();
        next.sort_unstable();
        next.dedup();
        (next != record.known_facility_ids).then_some(next)
    };
    if let Some(next) = next {
        let mut row = world.row_mut::<AgentDynamicsRow>(NAME)?;
        match row.agents.get_mut(agent_id) {
            Some(record) => record.known_facility_ids = next,
            None => return Err(Error::unsupported("the row changed under the plan")),
        }
    }
    Ok(())
}

/// `announce_installed_facility`: with the v3 policy, every agent learns a new facility of an
/// orientation kind.
pub fn announce_installed_facility(world: &World, facility_id: Option<&str>, policy: &Kept<&'static str>) -> Result<()> {
    if kit::namespace_absent(world, NAME)? {
        return Ok(());
    }
    let facility_id = id_of(facility_id)?;
    let shared = world
        .rows()?
        .facilities
        .get(facility_id)
        .is_some_and(|facility| ORIENTATION_KINDS.contains(&&*facility.kind));
    if !shared || *kit::kept(policy)? != POLICY_V3 {
        return Ok(());
    }
    let announced: Arc<str> = Arc::from(facility_id);
    if world.row::<AgentDynamicsRow>(NAME)?.agents.values().all(|record| record.known_facility_ids.contains(&announced)) {
        return Ok(());
    }
    let mut row = world.row_mut::<AgentDynamicsRow>(NAME)?;
    for record in row.agents.values_mut() {
        if !record.known_facility_ids.contains(&announced) {
            record.known_facility_ids.push(announced.clone());
            record.known_facility_ids.sort_unstable();
        }
    }
    Ok(())
}

// ---- The configs a rule reads, through the host (lane P9-1a) ---------------------------------------

/// The sections the rules of this namespace read, each read through the host (H2) where a rule
/// reads it: kept for the configs' stamp, or parsed from the configs' row, or filled by the host.
pub struct Configs;

impl Configs {
    /// `model_configs["facility_awareness"]`, or its fault.
    pub fn awareness<C: WorldCell, H: Host>(cell: &C, host: &mut H) -> std::result::Result<Kept<Option<Awareness>>, H::Error> {
        let holding = kit::holding(cell, host)?;
        kit::kept_section_value(cell, host, &holding, SectionId::FacilityAwareness, |configs| &configs.facility_awareness, agent_dynamics::awareness)
    }

    /// The policy `model_configs` selects, or its fault.
    pub fn policy<C: WorldCell, H: Host>(cell: &C, host: &mut H) -> std::result::Result<Kept<&'static str>, H::Error> {
        let holding = kit::holding(cell, host)?;
        kit::kept_section_value(cell, host, &holding, SectionId::AgentDynamicsPolicy, |configs| &configs.agent_dynamics_policy, agent_dynamics::policy)
    }

    /// Whether what a person knows of the facilities is limited, or its fault.
    pub fn knowledge_is_limited<C: WorldCell, H: Host>(cell: &C, host: &mut H) -> std::result::Result<Kept<bool>, H::Error> {
        let holding = kit::holding(cell, host)?;
        kit::kept_section_value(cell, host, &holding, SectionId::KnowledgeIsLimited, |configs| &configs.knowledge_is_limited, agent_dynamics::knowledge_is_limited)
    }
}

/// `learn_facility` with its section read through the host (lane P9-1a).
pub fn learn_facility_through<C: WorldCell, H: Host>(cell: &C, host: &mut H, agent_id: Option<&str>, facility_id: Option<&str>) -> std::result::Result<(), H::Error> {
    let awareness = Configs::awareness(cell, host)?;
    Ok(cell.with(|world| learn_facility(world, agent_id, facility_id, &awareness))??)
}
