//! The update phase (`UpdatePhase` of `experiments/phases/update.py` gives its steps): movement, the view,
//! each person's psychology, the social model, the psychology trace, the long-term distress, the new
//! values, and the view handed on to the propose phase (`Run.scratch`, decision D4).
//!
//! The steps pass the core's values to each other (the view's data, the deltas), where the PyO3
//! entries made Python objects between them; what those entries checked of the objects is checked
//! here where it can fail on the core's values (a stress within 0..1).

use std::sync::Arc;

use indexmap::IndexMap;

use crate::config::movement::Movement;
use crate::config::overlay::overlay_schema as parse_overlay_schema;
use crate::config::SectionId;
use crate::error::Error;
use crate::events::writer::WriteError;
use crate::host::{Again, Host, Holding, TraceLevel};
use crate::rules::care_receipts;
use crate::rules::kit::{check_metric_state, POLICY_FAULT};
use crate::rules::movement::{self, KEY as MOVEMENT};
use crate::rules::overlay::{Beds, OVERLAY_KEY, PLAN_UNREAD};
use crate::rules::replan::{self, Replanning, Second};
use crate::rules::travel::{Routing, Walking};
use crate::rules::psychology::{self, Applied, Delta, PsychologyResult};
use crate::rules::social_propagation as social;
use crate::rules::trace::{self, TraceInputs};
use crate::rules::world_view::{self as view, Inputs, ViewData};
use crate::run::held::Held as Read;
use crate::state::body_state::NAME as BODY_STATE;
use crate::value::V;
use crate::world::{WorldCell, METRIC_STATE, MODEL_CONFIGS};

use super::propose::refused;
use super::read::is_refusal;
use super::{Labelled, Prior, RunCell, Seed, Step, Tick};

pub const ADVANCE: &str = "movement.advance_movement";
pub const REPLAN: &str = "replan.replan_journeys";
const INGEST: &str = "care_receipts.ingest";
const FINISH: &str = "movement.finish_movement";

pub const VIEW: &str = "world_view.view_after_movement";
pub const PSYCHOLOGY: &str = "psychology.update_psychology";
pub const SOCIAL: &str = "social_propagation.update_social_propagation";
pub const TRACE: &str = "trace.record_psychology_trace";
pub const LONG_TERM: &str = "psychology.update_long_term";
pub const APPLY: &str = "psychology.apply_deltas";

/// The refusal of configs that are not a plain dict (`configs_of` of the view's glue).
pub const CONFIGS_NOT_PLAIN: &str = "the configs are not a plain dict";

/// What a view reads besides the rows: the model selection, and `world.model_configs` while the
/// world's slot does not hold it (`gather` of the view's glue).
pub struct Gathered {
    pub selection: std::result::Result<Arc<crate::config::Selection>, crate::config::Fault>,
    pub configs: Option<V>,
}

impl<C: WorldCell, H: Host, R: RunCell> Tick<'_, C, H, R> {
    /// `gather`: the configs must be a plain dict; then whether the slot holds them, then the
    /// selection.
    pub fn gather(&mut self, holding: &Holding) -> std::result::Result<Gathered, H::Error> {
        let configs = if holding.field(MODEL_CONFIGS) {
            None
        } else {
            match self.host.configs_given()? {
                Some(configs @ V::Map(_)) => Some(configs),
                _ => return Err(Error::unsupported(CONFIGS_NOT_PLAIN).into()),
            }
        };
        let selection = self.host_section(holding, SectionId::Selection, |configs| &configs.selection)?;
        let selection = selection.map_err(|fault| if fault.text.as_ref() == super::read::NO_SUCH_SECTION { crate::config::Fault::refused("not a selection") } else { fault });
        Ok(Gathered { selection, configs })
    }

    /// `simulator_extensions.get("enabled")` of the configs, truthy (`extensions_enabled_of`).
    pub fn extensions_enabled(&mut self, holding: &Holding) -> std::result::Result<bool, H::Error> {
        match self.host_section(holding, SectionId::ExtensionsEnabled, |configs| &configs.extensions_enabled)? {
            Ok(enabled) => Ok(*enabled),
            Err(fault) => Err(self.raise(SectionId::ExtensionsEnabled, &fault)),
        }
    }

    /// `from_world` without its hash: the core's `build` with what the Run keeps, then the
    /// extensions' flag, which `from_world` read last.
    pub fn build_view(&mut self, holding: &Holding, gathered: &Gathered) -> std::result::Result<ViewData, H::Error> {
        let mut cache = self.on_run(|run| std::mem::take(&mut run.views))?;
        let inputs = Inputs { selection: &gathered.selection, configs: gathered.configs.as_ref() };
        let built = self.cell.with(|world| view::build(world, &inputs, &mut cache));
        self.on_run(|run| run.views = cache)?;
        let draft = built.and_then(|built| built).map_err(H::Error::from)?;
        Ok(draft.finish(self.extensions_enabled(holding)?))
    }
}

/// The update phase.
pub fn update<C: WorldCell, H: Host, R: RunCell>(tick: &mut Tick<'_, C, H, R>) -> Step<(), H::Error>
where
    H::Error: From<WriteError>,
{
    update_movement(tick)?;
    let data = view_after_movement(tick).at(VIEW)?;
    let result = update_psychology(tick, &data).at(PSYCHOLOGY)?;
    let finals = update_social(tick, &data, &result).at(SOCIAL)?;
    record_trace(tick, &data, &result, &finals).at(TRACE)?;
    update_long_term(tick, &finals).at(LONG_TERM)?;
    apply_deltas(tick, &finals).at(APPLY)?;
    let data = Arc::new(data);
    tick.on_run(|run| run.scratch.put(Prior::View(data))).bare()?;
    Ok(())
}

/// The movement of the phase (lane P9-3b): with a movement state in `model_state` (a key whose value is not
/// None), the walk of the minute, and the replan of those whose route changed under them.
pub fn update_movement<C: WorldCell, H: Host, R: RunCell>(tick: &mut Tick<'_, C, H, R>) -> Step<(), H::Error>
where
    H::Error: From<WriteError>,
{
    let holding = tick.holding().bare()?;
    if !holding.keys.get(MOVEMENT).is_some_and(|key| !key.none) {
        return Ok(());
    }
    let minute = tick.minute;
    let moved = movement::advance_through(tick.cell, tick.host, minute).at(ADVANCE)?;
    if !moved.is_empty() {
        replan_journeys(tick, &moved).at(REPLAN)?;
    }
    Ok(())
}

/// `_replan_journeys` of the people `advance_movement` returned, in that order (lane c6; its inputs
/// through the host since lane P9-3b): what the world holds, then the Run's graph, the rows of the
/// queue, the floor plan, the movement section and the overlay's schema, read once. Each person is
/// two steps; a refusal before the first stop of the call changes nothing, and one after it is a
/// `RuntimeError`.
pub fn replan_journeys<C: WorldCell, H: Host, R: RunCell>(tick: &mut Tick<'_, C, H, R>, ids: &[Arc<str>]) -> std::result::Result<(), H::Error>
where
    H::Error: From<WriteError>,
{
    let holding = tick.holding()?;
    movement::check_held(&holding)?;
    tick.world(|world| world.queue().map(drop))?;
    let metric_state = holding.field(METRIC_STATE);
    tick.world(|world| check_metric_state(world, metric_state))?;
    if ids.is_empty() {
        return Ok(());
    }
    // What the steps read besides the world, read once: no step changes it.
    let graph = super::propose::graph(tick)?;
    tick.host.read_queue_rows()?;
    let plan = tick.plan();
    let section = tick.value_section(&holding, SectionId::Movement, |configs| &configs.movement, Movement::parse)?;
    let moving = holding.held(MOVEMENT, movement::OTHER_MOVEMENT);
    let staff = holding.held(crate::state::staff_operations::NAME, "the world holds another document than the row's");
    let overlay = holding.held(OVERLAY_KEY, replan::OTHER_OVERLAY);
    let schema = tick.value_section(&holding, SectionId::OverlaySchema, |configs| &configs.overlay_schema, parse_overlay_schema)?;
    let policies = tick.policies.clone();
    let replanning = Replanning {
        routing: Routing { graph: &graph, geo: plan.get(), movement: &section, staff: &staff },
        walking: Walking { movement: &moving, geo: plan.get() },
        beds: Beds { overlay: &overlay, movement: &moving, schema: &schema, walking: &section, geo: plan.get().map(|geo| &**geo) },
        movement: &section,
        staff_service: &policies.services.staff,
    };
    let mut written = false;
    for agent_id in ids {
        let result = replan_one(tick, &replanning, plan.get().is_some(), agent_id, &mut written);
        match result {
            // Say Unsupported no more: a stop has been written.
            Err(error) if written => match is_refusal(&error) {
                Some(reason) => {
                    crate::census::caught("replan after a stop", reason.clone());
                    return Err(Error::Runtime(format!("a kernel said Unsupported after a replan was written: {reason}")).into());
                }
                None => return Err(error),
            },
            result => result?,
        }
    }
    Ok(())
}

/// The two steps of one person.
fn replan_one<C: WorldCell, H: Host, R: RunCell>(
    tick: &mut Tick<'_, C, H, R>,
    replanning: &Replanning<'_>,
    planned: bool,
    agent_id: &Arc<str>,
    written: &mut bool,
) -> std::result::Result<(), H::Error>
where
    H::Error: From<WriteError>,
{
    let minute = tick.minute;
    let Some(journey) = tick.world(|world| replan::journey_of(world, agent_id))? else {
        return Ok(());
    };
    // Says Unsupported only before its first write.
    let stopped = tick.cell.with(|world| replan::stop(world, agent_id, &journey, minute)).map_err(|error: Error| <H::Error as From<Error>>::from(error))?;
    let taken = tick.written(stopped)?;
    *written = true;
    tick.host.slots_changed(&taken)?;
    let second = tick.cell.with(|world| replan::second_step(world, replanning, agent_id, &journey, minute)).map_err(|error: Error| <H::Error as From<Error>>::from(error))?;
    let second = tick.written(second);
    // What reading the floor plan raised, where the step read it (`PLAN_UNREAD`): read again.
    let second = match second {
        Err(error) if !planned && is_refusal(&error).as_deref() == Some(PLAN_UNREAD) => match tick.host.floor_plan() {
            Err(read) => Err(read),
            Ok(_) => Err(error),
        },
        second => second,
    };
    // A refusal of the Run's `staff_service` whose read raised, as reading it again raises (policy
    // E14).
    let second = match second {
        Err(error) if is_refusal(&error).as_deref() == Some(POLICY_FAULT) => {
            tick.host.read_again(Again::Policy(&["staff_service", "policy_version"]))?;
            tick.host.read_again(Again::Policy(&["staff_service", "cooldown_minutes"]))?;
            Err(error)
        }
        second => second,
    }?;
    match second {
        Second::Done(taken) => tick.host.slots_changed(&taken),
        Second::Continuity { event, minute: realized, taken } => {
            tick.host.slots_changed(&taken)?;
            // The receipt's projection and the end of the walk (`Host::find_record` for the
            // records the receipt reads, lane P9-1a), each its entry's KernelRefusal.
            care_receipts::ingest_applied(tick.cell, tick.host, &event, realized).map_err(|error| refused(INGEST, error))?;
            let finished = (|| -> std::result::Result<(), H::Error> {
                let holding = tick.holding()?;
                if movement::finish_held(&holding)? {
                    movement::finish_through(tick.cell, tick.host, agent_id)?;
                }
                Ok(())
            })();
            finished.map_err(|error| refused(FINISH, error))?;
            tick.world(|world| replan::after_continuity(world))
        }
    }
}

/// `_view_after_movement`.
pub fn view_after_movement<C: WorldCell, H: Host, R: RunCell>(tick: &mut Tick<'_, C, H, R>) -> std::result::Result<ViewData, H::Error> {
    let holding = tick.holding()?;
    let gathered = tick.gather(&holding)?;
    tick.build_view(&holding, &gathered)
}

/// `_psychology_update`: the deltas of each person, and the trace's rows.
fn update_psychology<C: WorldCell, H: Host, R: RunCell>(tick: &mut Tick<'_, C, H, R>, data: &ViewData) -> std::result::Result<PsychologyResult, H::Error> {
    let policies = tick.policies.clone();
    let traced = tick.host.trace_level()? != TraceLevel::Off;
    let psychology = &policies.psychology;
    let model = match psychology::model_of(psychology) {
        Ok(model) => model,
        Err(bad) => return Err(tick.bad(bad)),
    };
    let params = match psychology::params(psychology, model, Some(data.resource_service_enabled), traced) {
        Ok(params) => params,
        Err(bad) => return Err(tick.bad(bad)),
    };
    let minute = tick.minute;
    tick.world(|world| psychology::update(world, data, &params, minute, traced))
}

/// `_social_propagation`: the deltas after the social model, in ID order.
fn update_social<C: WorldCell, H: Host, R: RunCell>(tick: &mut Tick<'_, C, H, R>, data: &ViewData, result: &PsychologyResult) -> std::result::Result<Vec<Delta>, H::Error> {
    let policies = tick.policies.clone();
    let stress = &policies.stress;
    match social::enabled(stress) {
        Ok(true) => {}
        Ok(false) => return Ok(result.deltas.to_vec()),
        Err(bad) => return Err(tick.bad(bad)),
    }
    let traced = tick.host.trace_level()? != TraceLevel::Off;
    let Seed::Counter { text, .. } = tick.seed.clone() else {
        return Err(Error::unsupported("the RNG is not a CounterRNG").into());
    };
    let settings = social::settings(stress).map_err(|bad| tick.bad(bad))?;
    let expressiveness = social::expressiveness(stress).map_err(|bad| tick.bad(bad))?;
    let neighbors = tick.neighbors.clone();
    let Some(seed) = text else {
        return Err(Error::unsupported("the seed is not an int").into());
    };
    let inputs = social::Inputs {
        settings: &settings,
        expressiveness,
        view: data,
        deltas: &result.deltas,
        minute: tick.minute,
        seed: &seed,
        neighbors: &neighbors.social,
        traced,
    };
    let mut planned = tick.world(|world| social::update(world, &inputs))?;
    if planned.first_use {
        // `_read_runtime_state(None)` of the first tick: the step makes the namespace.
        let mut state = IndexMap::new();
        state.insert(Arc::from("schema_version"), V::Int(crate::state::social_propagation::SCHEMA_VERSION));
        state.insert(Arc::from("contacts"), V::Map(IndexMap::new()));
        state.insert(Arc::from("agent_cooldown_until"), V::Map(IndexMap::new()));
        let state = V::Map(state);
        let row = <crate::state::social_propagation::SocialRow as crate::row::Field>::from_value(&state)
            .map_err(|misfit| Error::Runtime(format!("a new social state does not fit its row: {}", misfit.0)))?;
        tick.world(|world| world.start_namespace(crate::state::social_propagation::NAME, Box::new(row)))?;
        tick.host.start_namespace(crate::state::social_propagation::NAME, &state)?;
        tick.world(|world| social::write(world, &mut planned))?;
    }
    if traced {
        let version = social::policy_version(stress).map_err(|bad| tick.bad(bad))?;
        social::record_social(tick.host, &planned, &V::Int(tick.minute), &version)?;
    }
    let mut finals = Vec::with_capacity(planned.order.len());
    for (&place, &stress) in planned.order.iter().zip(&planned.plan.stresses) {
        let mut delta = result.deltas[place].clone();
        delta.stress = stress;
        finals.push(delta);
    }
    Ok(finals)
}

/// A value of the controller's policies as the document holds it (`held_value` of the trace's
/// glue): a str subclass by its text.
fn held_value(held: &Read) -> crate::error::Result<V> {
    match held {
        Read::Value(value) => Ok(value.clone()),
        Read::Text(text) => Ok(V::Str(text.clone())),
        _ => Err(Error::unsupported("a source id is not a plain value")),
    }
}

/// `_record_psychology_trace`.
fn record_trace<C: WorldCell, H: Host, R: RunCell>(tick: &mut Tick<'_, C, H, R>, data: &ViewData, result: &PsychologyResult, finals: &[Delta]) -> std::result::Result<(), H::Error> {
    if tick.host.trace_level()? == TraceLevel::Off {
        return Ok(());
    }
    let Some(traces) = &result.traces else {
        return Err(Error::unsupported("the traces are not the psychology kernel's rows").into());
    };
    let by_agent: Vec<(&str, f64)> = finals.iter().map(|delta| (&*delta.agent_id, delta.stress)).collect();
    let policies = tick.policies.clone();
    let source_id = match policies.psychology.policy.as_ref() {
        Ok(policy) => match policy.get("source_id") {
            Ok(held) => held_value(held)?,
            Err(fault) => return Err(tick.policy_error(&["psychology", "source_id"], &fault)),
        },
        Err(fault) => return Err(tick.policy_error(&["psychology"], fault)),
    };
    let inputs = TraceInputs { source_id: &source_id, policy_version: &V::Str(result.policy_version.clone()), minute: &V::Int(tick.minute) };
    trace::record_psychology(tick.host, data, traces, &by_agent, &inputs)
}

/// An exact float within 0..1 (`unit_float` of the psychology glue).
fn unit(value: f64) -> crate::error::Result<f64> {
    if !(0.0..=1.0).contains(&value) {
        return Err(Error::unsupported("a value is outside 0..1"));
    }
    Ok(value)
}

/// `_update_long_term_psychology`.
fn update_long_term<C: WorldCell, H: Host, R: RunCell>(tick: &mut Tick<'_, C, H, R>, finals: &[Delta]) -> std::result::Result<(), H::Error> {
    let policies = tick.policies.clone();
    let psychology = &policies.psychology;
    match psychology::keeps_long_term(psychology) {
        Ok(true) => {}
        Ok(false) => return Ok(()),
        Err(bad) => return Err(tick.bad(bad)),
    }
    let long_term = psychology::long_term_of(psychology).map_err(|bad| tick.bad(bad))?;
    let mut people = Vec::with_capacity(finals.len());
    for delta in finals {
        people.push((&*delta.agent_id, unit(delta.stress)?));
    }
    let held = tick.holding()?.field(METRIC_STATE);
    let minute = tick.minute;
    tick.world(|world| psychology::update_long_term(world, &long_term, &people, minute, held))
}

/// `_apply_state_deltas`: every delta is checked before anything changes.
fn apply_deltas<C: WorldCell, H: Host, R: RunCell>(tick: &mut Tick<'_, C, H, R>, finals: &[Delta]) -> std::result::Result<(), H::Error> {
    let holding = tick.holding()?;
    if !holding.table {
        return Err(Error::unsupported(super::read::NOT_READABLE).into());
    }
    let body_state = match holding.held(BODY_STATE, "body_state_v1 is not the tree of its row") {
        crate::rules::kit::Held::Absent => false,
        crate::rules::kit::Held::Held => true,
        crate::rules::kit::Held::Other(reason) => return Err(Error::unsupported(reason).into()),
    };
    let mut applied = Vec::with_capacity(finals.len());
    for delta in finals {
        let values = [delta.toilet_urgency, delta.hunger, delta.fatigue, delta.hydration_need, delta.stress, delta.trust];
        for value in values {
            unit(value)?;
        }
        let body = match delta.body {
            Some(body) => body.map(Some),
            None => [None; 7],
        };
        applied.push(Applied { agent_id: &delta.agent_id, values, body });
    }
    tick.world(|world| psychology::apply(world, &applied, body_state))
}
