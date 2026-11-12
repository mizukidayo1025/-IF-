//! The propose phase (`ProposePhase` of `experiments/phases/propose.py` gives its steps): the view for the
//! proposals (the update phase's, read again, or a new one, with its audit hash), the proposals of
//! those due, and for each its observed state, its effect and its record.
//!
//! What the effect reads besides the world is read as the PyO3 entry `_apply_effect` read it
//! (`native/src/rules/proposals.rs`), by the proposal's action and in its order: what the world
//! holds, the configs' sections, the floor plan, the Run's people's router, the managers and the
//! staff service as the Run read them, the information policy's threshold.

use std::sync::Arc;

use indexmap::IndexMap;

use crate::config::movement::Movement;
use crate::config::population::AccessibilityNeeds;
use crate::config::SectionId;
use crate::error::Error;
use crate::host::{DecisionSink, Holding, Host};
use crate::routes::{Router, Which};
use crate::rules::behavior::{self, Proposal as Proposed};
use crate::rules::decisions::{self, Observed, Recording, Sunk, LIFECYCLE, MEMORY};
use crate::rules::kit::{Held, Kept};
use crate::rules::proposals::{
    self, Action, Applied, Asked, Information, Inputs, Proposal, Seek, Sleep, Trips, Walk, INFORMATION, MANAGER_UNREAD, THRESHOLD_UNREAD,
};
use crate::rules::support::{RequestService, AFTER_UNREAD, COOLDOWN_UNREAD, NAMESPACE_IS_NONE, OTHER_DOCUMENT, POLICY_UNREAD};
use crate::rules::travel::{self, Graph};
use crate::rules::world_view::{self as view, ViewData};
use crate::run::held::Held as Read;
use crate::value::V;
use crate::world::{WorldCell, METRIC_STATE};

use super::read::{is_refusal, Plan};
use super::update::Gathered;
use super::{Labelled, Prior, RunCell, Seed, Step, Tick};

pub const VIEW: &str = "world_view.view_for_proposals";
pub const PROPOSE: &str = "decisions.propose_rows";
pub const OBSERVED: &str = "decisions.observed_row";
pub const EFFECT: &str = "proposals.apply_effect";
pub const RECORD: &str = "decisions.record_outcome";

const OVERLAY_KEY: &str = "geometry_overlay_v1";
const SHARED: &str = "shared_operations_v1";
/// The label of the staff router's refusal (`router_of` of the travel glue).
const ROUTE_IS_CLEAR: &str = "movement.route_is_physically_clear";

/// The propose phase.
pub fn propose<C: WorldCell, H: Host, R: RunCell>(tick: &mut Tick<'_, C, H, R>) -> Step<(), H::Error> {
    let data = view_for_proposals(tick).at(VIEW)?;
    let Some(proposals) = due(tick, &data).at(PROPOSE)? else {
        return Ok(());
    };
    for proposal in &proposals {
        let observed = observed(tick, &data, proposal).at(OBSERVED)?;
        let applied = apply_effect(tick, &data, proposal).at(EFFECT)?;
        record_outcome(tick, &data, proposal, &applied, &observed).at(RECORD)?;
    }
    Ok(())
}

/// `_view_for_proposals`: the update phase's view of this minute with what that phase wrote read
/// again, or a new view; with its audit hash. The prior view is taken out of the Run's scratch
/// once the hash is made, as the step emptied `_view_before_proposals`.
pub fn view_for_proposals<C: WorldCell, H: Host, R: RunCell>(tick: &mut Tick<'_, C, H, R>) -> std::result::Result<Arc<ViewData>, H::Error> {
    let prior = tick.on_run(|run| run.scratch.get::<Prior>().cloned())?;
    let holding = tick.holding()?;
    let gathered = tick.gather(&holding)?;
    let minute = tick.minute;
    let data = match prior {
        Some(Prior::View(prior)) if prior.minute_index == minute => tick.world(|world| view::refresh(&prior, world))?,
        Some(Prior::Python { minute: Some(at) }) if at == minute => return Err(Error::unsupported("the prior view is Python's").into()),
        _ => tick.build_view(&holding, &gathered)?,
    };
    let data = finish(tick, data, &gathered)?;
    tick.on_run(|run| run.scratch.take::<Prior>())?;
    Ok(data)
}

/// The view's audit hash, as `state_hash` of the view.
fn finish<C: WorldCell, H: Host, R: RunCell>(tick: &mut Tick<'_, C, H, R>, data: ViewData, gathered: &Gathered) -> std::result::Result<Arc<ViewData>, H::Error> {
    let hash = tick.world(|world| view::state_hash(&data, world, gathered.configs.as_ref()))?;
    tick.on_run(|run| run.scratch.put(Hash(Arc::from(hash))))?;
    Ok(Arc::new(data))
}

/// The audit hash of the proposals' view, kept with it for the tick.
#[derive(Clone, Debug)]
pub struct Hash(pub Arc<str>);

/// `_propose_due`: the proposals of those due, or None when nobody is due.
fn due<C: WorldCell, H: Host, R: RunCell>(tick: &mut Tick<'_, C, H, R>, data: &ViewData) -> std::result::Result<Option<Vec<Proposed>>, H::Error> {
    let policies = tick.policies.clone();
    let scoring = behavior::scoring_of(&policies.behavior).map_err(|bad| tick.bad(bad))?;
    let seed = match &tick.seed {
        Seed::Other => return Err(Error::unsupported("the RNG is not a CounterRNG").into()),
        Seed::Counter { text: None, .. } => return Err(Error::unsupported("the seed is not an int").into()),
        Seed::Counter { negative: true, .. } => return Err(Error::unsupported("the seed is negative").into()),
        Seed::Counter { text: Some(text), .. } => text.clone(),
    };
    let Some(Hash(hash)) = tick.on_run(|run| run.scratch.get::<Hash>().cloned())? else {
        return Err(Error::unsupported("the view has no hash of a state").into());
    };
    behavior::due(data, &scoring, &seed, hash).map_err(|bad| tick.bad(bad))
}

/// `_observed_state`: a sink in Python, or a writer's, reads the document as a dict.
fn observed<C: WorldCell, H: Host, R: RunCell>(tick: &mut Tick<'_, C, H, R>, data: &ViewData, proposal: &Proposed) -> std::result::Result<Observed<'static>, H::Error> {
    let sink = tick.host.decision_sink()?;
    if matches!(sink, DecisionSink::Python | DecisionSink::Artifacts) {
        return Ok(Observed::Value(behavior::observed_value(data, proposal.index)));
    }
    Ok(Observed::Other)
}

// ---- The effect -----------------------------------------------------------------------------------

/// What a trip action reads besides the world.
struct TripsIn<E> {
    plan: Plan<E>,
    section: Kept<Movement>,
    movement: Held,
    staff: Held,
    graph: Graph,
    knows: Kept<bool>,
    queues: Held,
    needs: Kept<AccessibilityNeeds>,
    patched: bool,
    /// The resource service's config, or None where reading it raised; None for the toilet.
    config: Option<Option<IndexMap<Arc<str>, V>>>,
    service: Held,
    ledger: Held,
}

/// What `sleep` reads besides the world.
struct SleepIn<E> {
    plan: Plan<E>,
    section: Kept<Movement>,
    movement: Held,
    schema: Kept<Option<i64>>,
}

/// What the effect reads besides the world, by the action.
struct Read_<E> {
    overlay: Held,
    metric_state_held: bool,
    social_minutes: Option<i64>,
    sleep: Option<SleepIn<E>>,
    trips: Option<TripsIn<E>>,
    seek: Option<Seek>,
}

impl<E> Read_<E> {
    fn inputs(&self) -> Inputs<'_> {
        Inputs {
            overlay: self.overlay.clone(),
            metric_state_held: self.metric_state_held,
            social_minutes: self.social_minutes,
            sleep: self.sleep.as_ref().map(|sleep| Sleep {
                walk: Walk { movement: sleep.movement.clone(), section: &sleep.section, geo: sleep.plan.get() },
                schema: &sleep.schema,
            }),
            trips: self.trips.as_ref().map(|trips| Trips {
                walk: Walk { movement: trips.movement.clone(), section: &trips.section, geo: trips.plan.get() },
                staff: trips.staff.clone(),
                graph: &trips.graph,
                knows: &trips.knows,
                queues: trips.queues.clone(),
                needs: &trips.needs,
                asked: match &trips.config {
                    None => Asked::Toilet { patched: trips.patched },
                    Some(config) => Asked::Resource { patched: trips.patched, config: config.as_ref(), service: trips.service.clone(), ledger: trips.ledger.clone() },
                },
            }),
            seek: self.seek.as_ref().map(|seek| Seek {
                service: seek.service.clone(),
                shared: seek.shared.clone(),
                information: seek.information.as_ref().map(|information| Information { threshold: information.threshold, held: information.held.clone() }),
            }),
        }
    }

    /// The floor plan the action read, if any.
    fn take_plan(self) -> Option<Plan<E>> {
        match (self.sleep, self.trips) {
            (Some(sleep), _) => Some(sleep.plan),
            (None, Some(trips)) => Some(trips.plan),
            _ => None,
        }
    }
}

/// `"request" in manager.__dict__` of the manager as the Run read it.
fn patched(manager: &crate::run::held::Obj) -> bool {
    manager.own.contains_key("request")
}

/// The router of the Run's graph `which`, built from the world's tables where the last refresh
/// built none (`rows_of` and `staff_router_rows` of the travel glue).
pub fn router<C: WorldCell, H: Host, R: RunCell>(tick: &mut Tick<'_, C, H, R>, which: Which) -> std::result::Result<Arc<Router>, H::Error> {
    let (kept, epoch) = tick.on_run(|run| (run.routes.get(which), run.routes.epoch()))?;
    if let Some(router) = kept {
        return Ok(router);
    }
    let capacities = tick.on_run(|run| run.config.route_capacities().cloned())?.map_err(H::Error::from)?;
    let plan = tick.plan();
    let built = tick.world(|world| travel::routers_of(world, &capacities, plan.get()))?;
    let Some((people, staff)) = built else {
        return Err(Error::unsupported(travel::ZONES_TAINTED).into());
    };
    let router = match which {
        Which::People => people,
        Which::Staff => {
            let staff = staff.map_err(H::Error::from);
            plan.finish(staff, is_refusal).map_err(|error| refused(ROUTE_IS_CLEAR, error))?
        }
    };
    tick.on_run(|run| run.routes.keep(which, epoch, router))
}

/// A refusal as the KernelRefusal of `label`; any other error as it is.
pub fn refused<E: crate::host::HostFailure + From<Error>>(label: &'static str, error: E) -> E {
    match is_refusal(&error) {
        Some(reason) => Error::Refused { label: label.to_owned(), reason: reason.into() }.into(),
        None => error,
    }
}

/// The people's graph of the Run (`rows_of`).
pub fn graph<C: WorldCell, H: Host, R: RunCell>(tick: &mut Tick<'_, C, H, R>) -> std::result::Result<Graph, H::Error> {
    let capacities = tick.on_run(|run| run.config.route_capacities().cloned())?.map_err(H::Error::from)?;
    let router = router(tick, Which::People)?;
    Ok(Graph { router, capacities })
}

/// What `_apply_effect` reads besides the world, by the action (`Read::of` of its glue).
fn read_effect<C: WorldCell, H: Host, R: RunCell>(tick: &mut Tick<'_, C, H, R>, holding: &Holding, action: Action) -> std::result::Result<Read_<H::Error>, H::Error> {
    use crate::config::{agent_dynamics, body};
    let overlay = holding.held(OVERLAY_KEY, "the world holds another overlay state");
    let metric_state_held = holding.field(METRIC_STATE);
    let social_minutes = if action.is_social() {
        Some(*tick.value_now(holding, SectionId::SocialInteractionMinutes, |configs| &configs.social_interaction_minutes, body::social_interaction_minutes)?)
    } else {
        None
    };
    let sleep = if action == Action::Sleep {
        let plan = tick.plan();
        let section = tick.value_section(holding, SectionId::Movement, |configs| &configs.movement, Movement::parse)?;
        let movement = holding.held("movement_v1", "the world holds another movement state");
        let schema = tick.value_section(holding, SectionId::OverlaySchema, |configs| &configs.overlay_schema, crate::config::overlay::overlay_schema)?;
        Some(SleepIn { plan, section, movement, schema })
    } else {
        None
    };
    let trips = if action.trips() { trips_of(tick, holding, action)? } else { None };
    let seek = if action.seeks() { Some(seek_of(tick, holding)) } else { None };
    let _ = agent_dynamics::knowledge_is_limited;
    Ok(Read_ { overlay, metric_state_held, social_minutes, sleep, trips, seek })
}

/// The resource manager's `config` as the Run read it: None where its read raised or it is not a
/// plain dict (`manager_config`).
fn manager_config(manager: &crate::run::held::Obj) -> Option<IndexMap<Arc<str>, V>> {
    match manager.get("config") {
        Ok(Read::Value(V::Map(config))) => Some(config.clone()),
        _ => None,
    }
}

/// What `request_toilet`, `fetch_water` and `collect_meal` read: None for a resource action
/// without a resource service.
fn trips_of<C: WorldCell, H: Host, R: RunCell>(tick: &mut Tick<'_, C, H, R>, holding: &Holding, action: Action) -> std::result::Result<Option<TripsIn<H::Error>>, H::Error> {
    let policies = tick.policies.clone();
    let resource = action.resource().is_some();
    let services = &policies.services;
    let manager = if resource {
        if services.resource.is_none().map_err(|fault| tick.policy_error(&["resource_service"], &fault))? {
            return Ok(None);
        }
        services.resource.object().map_err(|fault| tick.policy_error(&["resource_service"], &fault))?
    } else {
        services.toilet.object().map_err(|fault| tick.policy_error(&["toilet_service"], &fault))?
    };
    let graph = graph(tick)?;
    let knows = tick.value_section(holding, SectionId::KnowledgeIsLimited, |configs| &configs.knowledge_is_limited, crate::config::agent_dynamics::knowledge_is_limited)?;
    // The rows of the events Python scheduled, for the core's reads of the whole queue.
    if graph.capacities.any || resource {
        tick.host.read_queue_rows()?;
    }
    let config = if resource { Some(manager_config(manager)) } else { None };
    let plan = tick.plan();
    let section = tick.value_section(holding, SectionId::Movement, |configs| &configs.movement, Movement::parse)?;
    let movement = holding.held("movement_v1", "the world holds another movement state");
    let staff = holding.held("staff_operations_v1", OTHER_DOCUMENT);
    let queues = holding.held("facility_queues_v1", "the world holds another queue state");
    let needs = tick.value_section(holding, SectionId::AccessibilityNeeds, |configs| &configs.accessibility_needs, AccessibilityNeeds::parse)?;
    Ok(Some(TripsIn {
        plan,
        section,
        movement,
        staff,
        graph,
        knows,
        queues,
        needs,
        patched: patched(manager),
        config,
        service: holding.held("resource_service_v1", "the world holds another service state"),
        ledger: holding.held("resource_ledger_v1", "the world holds another ledger"),
    }))
}

/// What `seek_help` and `seek_information` read: a value whose read raised is None, raised again
/// where the core says the rule reads it.
fn seek_of<C: WorldCell, H: Host, R: RunCell>(tick: &mut Tick<'_, C, H, R>, holding: &Holding) -> Seek {
    let policies = tick.policies.clone();
    let service = policies.services.staff.object().ok();
    let int = |name: &str| service.and_then(|service| service.int(name).ok());
    let policy_version = service.and_then(|service| match service.get("policy_version") {
        Ok(Read::Value(V::Str(text))) => Some(text.clone()),
        _ => None,
    });
    let information = match policies.information.policy.as_ref() {
        Ok(None) => None,
        _ => Some(Information {
            threshold: match policies.information.config() {
                Ok(Some(config)) => match config.get("query_threshold") {
                    Ok(Read::Value(V::Float(threshold))) => Some(*threshold),
                    _ => None,
                },
                _ => None,
            },
            held: holding.held(INFORMATION, "the information state is for the Python rule"),
        }),
    };
    let shared = match holding.is_absent(SHARED) {
        Ok(true) => {
            if holding.keys.contains_key(SHARED) {
                Held::Other(NAMESPACE_IS_NONE)
            } else {
                Held::Absent
            }
        }
        Ok(false) => holding.held(SHARED, OTHER_DOCUMENT),
        Err(reason) => Held::Other(reason),
    };
    Seek {
        service: RequestService { unresolved_after_minutes: int("unresolved_after_minutes"), cooldown_minutes: int("cooldown_minutes"), policy_version },
        shared,
        information,
    }
}

/// The core's refusal of a value whose read raised, as what reading it again raises.
fn read_again<C: WorldCell, H: Host, R: RunCell, T>(tick: &mut Tick<'_, C, H, R>, plan: Option<Plan<H::Error>>, result: std::result::Result<T, H::Error>) -> std::result::Result<T, H::Error> {
    let result = match plan {
        Some(plan) => plan.finish(result, is_refusal),
        None => result,
    };
    let Err(error) = result else {
        return result;
    };
    let Some(reason) = is_refusal(&error) else {
        return Err(error);
    };
    let path: &[&str] = match reason.as_str() {
        MANAGER_UNREAD => &["resource_service", "config"],
        AFTER_UNREAD => &["staff_service", "unresolved_after_minutes"],
        COOLDOWN_UNREAD => &["staff_service", "cooldown_minutes"],
        POLICY_UNREAD => &["staff_service", "policy_version"],
        THRESHOLD_UNREAD => &["information_policy", "config", "query_threshold"],
        _ => return Err(error),
    };
    tick.host.read_again(crate::host::Again::Policy(path))?;
    Err(error)
}

/// `_apply_effect`: the target, the next decision, and whether the decision committed.
fn apply_effect<C: WorldCell, H: Host, R: RunCell>(tick: &mut Tick<'_, C, H, R>, data: &ViewData, proposed: &Proposed) -> std::result::Result<Applied, H::Error> {
    let outcome = &proposed.outcome;
    let Some(action) = Action::parse(outcome.action.value()) else {
        return Err(Error::unsupported("an action unknown here").into());
    };
    let agent_id = data.agents[proposed.index].id.clone();
    let target_id = if action.is_social() { outcome.target.map(|target| data.agents[target].id.clone()) } else { None };
    let observed_state_hash = if action.seeks() { Some(proposed.common.state_hash.clone()) } else { None };
    apply_alone(tick, Proposal { action, agent_id, next_decision_at: outcome.next_decision_at, target_id, observed_state_hash })
}

/// `_apply_effect` of one proposal, as the PyO3 entry `proposals.apply_effect` runs it: what the
/// action reads, then its effect (also for the core's tests, Issue #225). A seek's proposal has its
/// observed state hash.
pub fn apply_alone<C: WorldCell, H: Host, R: RunCell>(tick: &mut Tick<'_, C, H, R>, proposal: Proposal) -> std::result::Result<Applied, H::Error> {
    let holding = tick.holding()?;
    if !holding.table {
        return Err(Error::unsupported(super::read::NOT_PLAIN).into());
    }
    let read = read_effect(tick, &holding, proposal.action)?;
    if !proposal.action.seeks() {
        let inputs = read.inputs();
        let applied = tick.cell.with(|world| proposals::apply(world, &inputs, &proposal)).map_err(H::Error::from)?;
        let applied = tick.written(applied);
        return read_again(tick, read.take_plan(), applied);
    }
    let inputs = read.inputs();
    let sought = tick.cell.with(|world| proposals::seek_write(world, &inputs, &proposal)).map_err(H::Error::from)?;
    let sought = tick.written(sought);
    let sought = match read_again(tick, None, sought) {
        Ok(sought) => sought,
        Err(error) => return Err(error),
    };
    proposals::seek_after(tick.cell, tick.host, &inputs, &proposal, sought)
}

/// `_record_outcome`: the record, then the lifecycle and the memory. A sink in Python records it
/// through the host (H5), after which the core runs the same rules.
fn record_outcome<C: WorldCell, H: Host, R: RunCell>(
    tick: &mut Tick<'_, C, H, R>,
    data: &ViewData,
    proposal: &Proposed,
    applied: &Applied,
    observed: &Observed<'_>,
) -> std::result::Result<(), H::Error> {
    let observed = match observed {
        Observed::Other => Observed::Row { data, index: proposal.index },
        other => other.clone(),
    };
    let holding = tick.holding()?;
    let remembers = holding.keys.contains_key("agent_dynamics_v1");
    let recording = Recording {
        data,
        proposal,
        target_id: applied.target.as_deref(),
        next_decision_at: applied.next_decision_at,
        committed: applied.committed,
        observed: &observed,
        remembers,
    };
    let python = applied.next_decision_at <= data.minute_index || decisions::record(tick.cell, tick.host, &recording)? == Sunk::Python;
    if python {
        tick.host.record_decision_python(data, proposal, recording.target_id, recording.next_decision_at, &observed)?;
    }
    if recording.committed {
        let committed = tick.run.with(|run| tick.cell.with(|world| decisions::commit(world, &mut run.actions, &recording)));
        committed.and_then(|done| done).and_then(|done| done).map_err(|error| Error::refused(LIFECYCLE, error))?;
    }
    tick.world(|world| decisions::remember(world, &recording)).map_err(|error| refused(MEMORY, error))
}
