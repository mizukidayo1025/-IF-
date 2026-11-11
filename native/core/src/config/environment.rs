//! The sections of the event handlers' environment: `ScenarioZoneEnvironmentConfig` as a condition
//! modifier reads it, and `ScenarioInfrastructureSpec` (lane 7-5b of Issue #152). The PyO3 crate
//! validates both with the pydantic contracts, which raise their own errors.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::sync::Arc;

use super::read::{map, text, Read};
use super::section::Fault;
use crate::value::V;

/// What `effect_program_environment_start` reads of the zone environment config.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct ZoneEnvironmentSpec {
    pub policy_version: Arc<str>,
    pub source_id: Arc<str>,
}

impl ZoneEnvironmentSpec {
    /// From `model_dump(mode="json")` of the validated model.
    pub fn from_v(spec: &V) -> Read<Self> {
        let spec = map(spec)?;
        let field = |name: &str| text(spec.get(name).unwrap_or(&V::Null)).cloned();
        Ok(Self { policy_version: field("policy_version")?, source_id: field("source_id")? })
    }
}

/// What the infrastructure handlers read of `ScenarioInfrastructureSpec`.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Infrastructure {
    pub source_id: Arc<str>,
    /// Of each dependency, the components that depend on it.
    dependents: BTreeMap<Arc<str>, BTreeSet<Arc<str>>>,
}

impl Infrastructure {
    /// From `model_dump(mode="json")` of the validated model.
    pub fn from_v(spec: &V) -> Read<Self> {
        let spec = map(spec)?;
        let source_id = text(spec.get("source_id").unwrap_or(&V::Null))?.clone();
        let mut dependents: BTreeMap<Arc<str>, BTreeSet<Arc<str>>> = BTreeMap::new();
        let edges = match spec.get("dependency_edges") {
            Some(V::List(edges)) => edges.as_slice(),
            None => &[],
            Some(_) => return Err(Fault::refused("the dependency edges are not a list")),
        };
        for edge in edges {
            let edge = map(edge)?;
            let end = |name: &str| text(edge.get(name).unwrap_or(&V::Null)).cloned();
            let (dependency, dependent) = (end("dependency_id")?, end("dependent_id")?);
            dependents.entry(dependency).or_default().insert(dependent);
        }
        Ok(Self { source_id, dependents })
    }

    /// `dependent_facilities`: the facilities that `component` is or that depend on it, at any
    /// depth, sorted.
    pub fn dependent_facilities(&self, component: &str, facilities: &BTreeSet<String>) -> Vec<String> {
        let mut reached = BTreeSet::new();
        let mut pending = VecDeque::from([Arc::<str>::from(component)]);
        let mut visited = BTreeSet::from([Arc::<str>::from(component)]);
        while let Some(current) = pending.pop_front() {
            if facilities.contains(&*current) {
                reached.insert(current.to_string());
            }
            if let Some(next) = self.dependents.get(&current) {
                for dependent in next {
                    if facilities.contains(&**dependent) {
                        reached.insert(dependent.to_string());
                    }
                    if visited.insert(dependent.clone()) {
                        pending.push_back(dependent.clone());
                    }
                }
            }
        }
        reached.into_iter().collect()
    }
}
