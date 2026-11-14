//! `validate_population_lifecycle_state(world)` of `domain/population_lifecycle.py`, with Python's
//! errors and texts (lane D of #250, #349). Raw errors.
//!
//! Python's rule is the core's kernel (`kernel_only("population_lifecycle",
//! "validate_population_lifecycle_state")`, `rules::population_lifecycle::validate_facts`), so this
//! reads the same facts of the world and gives its refusal as the `KernelRefusal` Python raises.

use std::sync::Arc;

use super::View;
use crate::error::{Error, Result};
use crate::events::EventRow;
use crate::rules::kit::PRESENCE;
use crate::rules::population::NO_ROW;
use crate::rules::population_lifecycle::{validate_facts, Facts, TaskFacts, KEY};

const LABEL: &str = "population_lifecycle.validate_population_lifecycle_state";

/// The check, as `validate` (`validate/mod.rs`) calls it in Python's order.
pub fn validate(view: &View<'_>) -> Result<()> {
    let state = view.state(KEY)?;
    let presence = if view.contains(PRESENCE)? { Some(view.state(PRESENCE)?) } else { None };
    let has_agent = |id: &str| -> Result<bool> { Ok(view.rows()?.agents.contains_key(id)) };
    let has_zone = |id: &str| -> Result<bool> { Ok(view.rows()?.zones.contains_key(id)) };
    let tasks = |id: &str| -> Result<Vec<TaskFacts>> {
        let rows = view.rows()?;
        Ok(rows
            .active_tasks
            .iter()
            .filter(|(_, task)| &*task.id == id)
            .map(|(_, task)| TaskFacts { agent_id: task.agent_id.clone(), kind: task.kind.clone(), due_minute: task.due_minute })
            .collect())
    };
    let event = |id: &str| -> Result<Option<Arc<EventRow>>> {
        let queue = view.queue()?;
        match queue.serial_of(id) {
            None => Ok(None),
            Some(serial) => queue.slot(serial).row.clone().map(Some).ok_or_else(|| Error::unsupported(NO_ROW)),
        }
    };
    validate_facts(
        &Facts {
            state: state.as_deref(),
            presence: presence.as_ref().map(|presence| presence.as_deref()),
            has_agent: &has_agent,
            has_zone: &has_zone,
            tasks: &tasks,
            event: &event,
        },
        None,
    )
    .map_err(|error| Error::refused(LABEL, error))
}
