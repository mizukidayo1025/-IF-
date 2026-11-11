//! The section of the Scenario's population as the queue choice reads it (lane 7-5a of Issue #152):
//! `model_configs["population"]["people"][agent_id]["accessibility_needs"]`.

use std::sync::Arc;

use indexmap::IndexMap;

use super::read::{map, Read};
use super::section::Fault;
use crate::value::V;

/// The accessibility needs of each person of the population, or what reading them refuses: the
/// refusal of one person is said only for that person, as each was read alone.
#[derive(Clone, PartialEq, Debug, Default)]
pub struct AccessibilityNeeds {
    people: IndexMap<Arc<str>, Read<Vec<Arc<str>>>>,
}

impl AccessibilityNeeds {
    /// `exact_dict` of the configs, of `population` and of its `people`, where each is there.
    pub fn parse(configs: &V) -> Read<Self> {
        let mut people = IndexMap::new();
        if let Some(population) = map(configs)?.get("population") {
            if let Some(listed) = map(population)?.get("people") {
                for (agent_id, traits) in map(listed)? {
                    people.insert(agent_id.clone(), needs_of(traits));
                }
            }
        }
        Ok(Self { people })
    }

    /// The needs of a person; none for a person the population does not list.
    pub fn of(&self, agent_id: &str) -> Read<&[Arc<str>]> {
        match self.people.get(agent_id) {
            None => Ok(&[]),
            Some(Ok(needs)) => Ok(needs),
            Some(Err(fault)) => Err(fault.clone()),
        }
    }
}

fn needs_of(traits: &V) -> Read<Vec<Arc<str>>> {
    let Some(needs) = map(traits)?.get("accessibility_needs") else {
        return Ok(Vec::new());
    };
    let V::List(needs) = needs else {
        return Err(Fault::refused("the needs are not a list"));
    };
    needs
        .iter()
        .map(|need| match need {
            V::Str(need) => Ok(need.clone()),
            _ => Err(Fault::refused("a need is not a str")),
        })
        .collect()
}
