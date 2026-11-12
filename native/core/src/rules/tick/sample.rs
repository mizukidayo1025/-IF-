//! The sample phase (`SamplePhase` of `experiments/phases/sample.py` gives its steps): the information
//! contacts of the minute, then the proximity contacts where the controller has their config.

use crate::error::Error;
use crate::host::{Again, Host, TraceLevel};
use crate::rules::information;
use crate::rules::kit::POLICY_FAULT;
use crate::rules::proximity;
use crate::value::V;
use crate::world::WorldCell;

use super::read::is_refusal;
use super::{Labelled, RunCell, Seed, Step, Tick};

pub const INFORMATION: &str = "information.sample_contacts";
pub const PROXIMITY: &str = "proximity.sample_contacts";

/// The sample phase.
pub fn sample<C: WorldCell, H: Host, R: RunCell>(tick: &mut Tick<'_, C, H, R>) -> Step<(), H::Error> {
    sample_information(tick).at(INFORMATION)?;
    let policies = tick.policies.clone();
    let configured = match &policies.proximity.config {
        Ok(config) => !matches!(config, V::Null),
        Err(fault) => return Err(tick.policy_error(&["proximity_contact_config"], fault)).bare(),
    };
    if configured {
        sample_proximity(tick).at(PROXIMITY)?;
    }
    Ok(())
}

/// `_sample_information_contacts`.
fn sample_information<C: WorldCell, H: Host, R: RunCell>(tick: &mut Tick<'_, C, H, R>) -> std::result::Result<(), H::Error> {
    let policies = tick.policies.clone();
    let model = match information::model_of(&policies.information) {
        Ok(Some(model)) => model,
        Ok(None) => return Ok(()),
        Err(bad) => return Err(tick.bad(bad)),
    };
    let seed = match &tick.seed {
        Seed::Other => return Err(Error::unsupported("the random source is not a CounterRNG").into()),
        Seed::Counter { text: None, .. } => return Err(Error::unsupported("the seed is not an int").into()),
        Seed::Counter { negative: true, .. } => return Err(Error::unsupported("the minute or the seed is out of range").into()),
        Seed::Counter { text: Some(text), .. } => text.clone(),
    };
    if tick.minute < 0 || tick.minute == i64::MAX {
        return Err(Error::unsupported("the minute or the seed is out of range").into());
    }
    let settings = information::settings(&policies.information, model).map_err(|bad| tick.bad(bad))?;
    let neighbors = tick.neighbors.clone();
    let neighbors = neighbors.information.as_ref().map_err(|reason| H::Error::from(Error::unsupported(*reason)))?;
    let recording = tick.host.trace_level()? != TraceLevel::Off;
    let inputs = information::Inputs { model, settings: &settings, minute: tick.minute, seed: &seed, neighbors, recording };
    let plan = tick.cell.with(|world| information::sample(world, &inputs)).map_err(H::Error::from)?;
    let plan = tick.written(plan)?;
    if recording {
        information::record_information(tick.host, &plan, model, &V::Int(tick.minute))?;
    }
    Ok(())
}

/// A refusal of the Run's policies whose read raised, as reading them again raises.
fn policy_fault<C: WorldCell, H: Host, R: RunCell, T>(tick: &mut Tick<'_, C, H, R>, result: std::result::Result<T, H::Error>) -> std::result::Result<T, H::Error> {
    match result {
        Err(error) if is_refusal(&error).as_deref() == Some(POLICY_FAULT) => {
            tick.host.read_again(Again::Policy(&["proximity_contact_config"]))?;
            tick.host.read_again(Again::Policy(&["proximity_policy", "config"]))?;
            Err(error)
        }
        result => result,
    }
}

/// `_sample_proximity_contacts`.
fn sample_proximity<C: WorldCell, H: Host, R: RunCell>(tick: &mut Tick<'_, C, H, R>) -> std::result::Result<(), H::Error> {
    let policies = tick.policies.clone();
    let when = proximity::when(&policies.proximity, Some(tick.minute)).map_err(H::Error::from);
    let Some(minute) = policy_fault(tick, when)? else {
        return Ok(());
    };
    let holding = tick.holding()?;
    let movement = holding.held("movement_v1", "the world holds another movement state");
    let sampled = tick.cell.with(|world| proximity::sample_contacts(world, &policies.proximity, minute, &movement)).map_err(H::Error::from)?;
    let sampled = tick.written(sampled);
    policy_fault(tick, sampled)
}
