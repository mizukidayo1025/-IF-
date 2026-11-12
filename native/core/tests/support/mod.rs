//! What the core's tests share (not part of the crate: decision 4 is about its API).
//!
//! `TestHost` is the host of a build without Python (orchestrator decision (c) on design-p8.md
//! section 4.3): its `contract` gives its input back as given, its LLM methods do what a Run
//! without an LLM does (nothing; `llm()` says there is none), its sinks write down each call, and
//! the whole world's validation passes (policy E6: the Python checks are not in the core in #152).
//!
//! Lane P9-0: its holding is the world's slots' (`Host::holding` gives None) unless a test sets
//! one; it fills every section of the configs with the values `fill_section` builds here, from
//! outside the crate (the core-only tick of P10 needs that); it runs no foreign handler; and its
//! Python decision sink writes the call down. A test hook (`on_llm`) runs at each LLM call, as an
//! LLM hook of the native host writes the world.

#![allow(dead_code)]

pub mod answers;
pub mod canonical;
pub mod cases;
pub mod contracts;
pub mod dumps;
pub mod load;
pub mod sections;
pub mod steps;
pub mod texts;
pub mod ticks;
pub mod views;

use std::sync::Arc;

use indexmap::IndexMap;
use sheltersim_core::config::body::{AffectModel, BodyModels, CareOutcome, TrustModel};
use sheltersim_core::config::environment::{Infrastructure, ZoneEnvironmentSpec};
use sheltersim_core::config::geometry::FacilityPlan;
use sheltersim_core::config::information::InformationSpread;
use sheltersim_core::config::movement::{Movement, RouteCapacities};
use sheltersim_core::config::overlay::OverlayRooms;
use sheltersim_core::config::population::AccessibilityNeeds;
use sheltersim_core::config::queues::QueueConfig;
use sheltersim_core::config::service::ResourceService;
use sheltersim_core::config::staff::{StaffFatigue, StaffOperations};
use sheltersim_core::config::{
    agent_dynamics, Configs, FacilityGeometry, Filled, Lazy, Section, SectionId, Selection, SpatialConstraints, TickPlan,
};
use sheltersim_core::error::Error;
use sheltersim_core::events::EventRow;
use sheltersim_core::host::{
    Contract, Decision, DecisionSink, Foreign, Holding, Host, Llm, Part, Phase, Stage, Trace, TraceLevel, TxnKind, NO_FOREIGN,
};
use sheltersim_core::geometry::Geo;
use sheltersim_core::rules::behavior::Proposal;
use sheltersim_core::rules::common_operations::Logged;
use sheltersim_core::rules::decisions::Observed;
use sheltersim_core::rules::metrics::MetricSample;
use sheltersim_core::rules::world_view::ViewData;
use sheltersim_core::value::V;
use sheltersim_core::world::Snapshot;

/// One call a `TestHost` was asked.
#[derive(Clone, Debug, PartialEq)]
pub enum Call {
    Validate(TxnKind, Stage),
    Canonicalized(TxnKind),
    Restored(TxnKind),
    Committed(TxnKind),
    ValidateWorld,
    ValidateExtensions,
    Contract(Contract, V),
    Prioritize { request: Arc<str>, agent: Arc<str>, hash: Arc<str> },
    SupportRequest { agent: Arc<str>, request: Arc<str> },
    Complaint { agent: Arc<str>, request: Arc<str> },
    Cooperation { initiator: Arc<str>, participant: Arc<str>, trigger: Arc<str> },
    InformationReaction { agent: Arc<str>, participant: Arc<str>, message: Arc<str>, trigger: Arc<str> },
    Metric(MetricSample, i64),
    Decision(Decision),
    Trace(Trace),
    Phase(Phase, bool),
    SpaceRefreshed,
    Draw(Vec<Part>),
    // ---- Lane P9-0 ----
    Holding,
    FillSection(SectionId),
    Foreign(Arc<str>),
    DecisionPython { agent: Arc<str>, target: Option<Arc<str>>, next: i64 },
    // ---- Lane P9-1a ----
    FindRecord(Arc<str>),
    FloorPlan,
    // ---- #214 ----
    StartNamespace(Arc<str>),
    ResourcesChanged(usize, i64),
}

/// A host without Python that writes down what it was asked.
pub struct TestHost {
    pub calls: Vec<Call>,
    pub llm: Llm,
    pub sink: DecisionSink,
    pub level: TraceLevel,
    /// The draws `draw` gives, in turn; none means it refuses as every host but a test's does.
    pub draws: Vec<f64>,
    /// What `decision_source` answers (lane P8 a5).
    pub source: Option<Option<Arc<str>>>,
    /// What `holding` answers; None: the world's slots answer (lane P9-0).
    pub holding: Option<Arc<Holding>>,
    /// Run at each LLM call (`consider_*`, `prioritize_support_request`), as an LLM hook of the
    /// native host writes the world.
    pub on_llm: Option<Box<dyn FnMut()>>,
    /// The EventLog's records `find_record` finds, by event ID (lane P9-1a); past them, those of
    /// the loaded world's log where `world` is set (#225 lane M9).
    pub records: IndexMap<Arc<str>, Logged>,
    /// The floor plan `floor_plan` gives; none means it refuses as a host without one does.
    pub plan: Option<Arc<Geo>>,
    /// The loaded world this host is the owner of (`load::following_host`, #214): where set, the
    /// floor plan is the world's as it is at each call (not `plan`), and the records are the world's
    /// log's. None: `plan`, and the trait's defaults.
    pub world: Option<load::Following>,
    /// Sections `fill_section` fills with these instead (a section's fault, a selection).
    pub sections: Vec<(SectionId, Box<dyn Fn(&Configs)>)>,
    /// What `apply_foreign` does (lane P9-1c): a test's handler; none refuses.
    pub foreign: Option<Box<dyn FnMut(&EventRow) -> Result<Foreign, Error>>>,
    /// What `contract` does (lane P9-3a): a test's contract; none gives the input back.
    pub contracts: Option<Box<dyn FnMut(&Contract, &V) -> Result<V, Error>>>,
    /// Run before (true) and after (false) each phase of a tick, as `TickEngine.phase_observer`
    /// (Issue #225, lane M10b): a test reads the world and the Run there.
    pub on_phase: Option<Box<dyn FnMut(Phase, bool)>>,
}

impl std::fmt::Debug for TestHost {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TestHost").field("calls", &self.calls).finish_non_exhaustive()
    }
}

impl Default for TestHost {
    fn default() -> Self {
        Self {
            calls: Vec::new(),
            llm: Llm::default(),
            sink: DecisionSink::Silent,
            level: TraceLevel::Off,
            draws: Vec::new(),
            source: Some(None),
            holding: None,
            on_llm: None,
            records: IndexMap::new(),
            plan: None,
            world: None,
            sections: Vec::new(),
            foreign: None,
            contracts: None,
            on_phase: None,
        }
    }
}

impl TestHost {
    fn llm_ran(&mut self) {
        if let Some(hook) = &mut self.on_llm {
            hook();
        }
    }
}

/// A section's value, put in the configs if they do not hold one yet.
fn put<T>(lazy: &Lazy<T>, value: T) {
    let _ = lazy.get_or_parse(|| Section::Ok(Arc::new(value)));
}

/// What a test host fills each section with: the values of configs `{}` where a parser of the core
/// reads the configs' `V`, fixed values for what the Python contracts and factories parse. Each is
/// built with the crate's public API only, which is what a host outside the crate (P10's) has.
pub fn fill_section(configs: &Configs, which: SectionId) {
    let empty = V::Map(IndexMap::new());
    let text = |value: &str| -> Arc<str> { Arc::from(value) };
    match which {
        SectionId::Selection => put(&configs.selection, Selection::new(Vec::new())),
        SectionId::TickPlan => put(&configs.tick_plan, TickPlan { operational_enabled: Some(false), ..TickPlan::default() }),
        SectionId::FacilityGeometry => put(
            &configs.facility_geometry,
            FacilityGeometry {
                plan: Ok(Arc::new(FacilityPlan {
                    zones: Default::default(),
                    routes: Default::default(),
                    facility_points: Default::default(),
                    service_points: Vec::new(),
                })),
                approach_routes: Ok(Arc::from(Vec::new())),
                independent_queues: Ok(false),
            },
        ),
        SectionId::SpatialConstraints => put(&configs.spatial_constraints, SpatialConstraints::default()),
        SectionId::StaffFatigue => put(&configs.staff_fatigue, StaffFatigue::parse(&empty).unwrap()),
        SectionId::StaffOperations => {
            put(&configs.staff_operations, StaffOperations { model_id: text("staff-operations-test-v1"), selected_source: None })
        }
        SectionId::StaffOperationsKnown => put(&configs.staff_operations_known, ()),
        SectionId::SupportTaskMinutes => put(&configs.support_task_minutes, 10),
        SectionId::SupportCooldownMinutes => put(&configs.support_cooldown_minutes, 30),
        SectionId::InformationSpread => {
            // `InformationSpreadConfig()`'s attributes: off, with no source's reliability.
            let config = V::Map(IndexMap::from([(text("correction_source_reliability_by_id"), empty.clone())]));
            put(&configs.information_spread, InformationSpread::from_v(&config).unwrap())
        }
        SectionId::BodyModels => put(&configs.body_models, BodyModels { affect: AffectModel::Linear, trust: TrustModel::DurationOnly }),
        SectionId::SocialInteractionMinutes => put(&configs.social_interaction_minutes, 15),
        SectionId::CareOutcome => {
            put(&configs.care_outcome, CareOutcome::new(text("care-outcome-test-v1"), text("ASM-TEST"), empty.clone()).unwrap())
        }
        SectionId::AgentDynamicsPolicy => put(&configs.agent_dynamics_policy, agent_dynamics::policy(&empty).unwrap()),
        SectionId::FacilityAwareness => put(&configs.facility_awareness, agent_dynamics::awareness(&empty).unwrap()),
        SectionId::KnowledgeIsLimited => put(&configs.knowledge_is_limited, agent_dynamics::knowledge_is_limited(&empty).unwrap()),
        SectionId::ZoneEnvironment => put(
            &configs.zone_environment,
            ZoneEnvironmentSpec { policy_version: text("zone-environment-v1"), source_id: text("ASM-TEST") },
        ),
        SectionId::Infrastructure => put(
            &configs.infrastructure,
            Infrastructure::from_v(&V::Map(IndexMap::from([(text("source_id"), V::Str(text("ASM-TEST")))]))).unwrap(),
        ),
        SectionId::Proximity => put(&configs.proximity, None),
        SectionId::Movement => put(&configs.movement, Movement::parse(&empty).unwrap()),
        SectionId::RouteCapacities => put(&configs.route_capacities, RouteCapacities::parse(&empty).unwrap()),
        SectionId::TransitCapacities => put(&configs.transit_capacities, false),
        SectionId::FacilityQueues => put(&configs.facility_queues, QueueConfig::parse(&empty).unwrap()),
        SectionId::OverlaySchema => put(&configs.overlay_schema, None),
        SectionId::OverlayRooms => put(&configs.overlay_rooms, OverlayRooms { is_dict: false, room_schema: Ok(false) }),
        SectionId::ResourceService => put(&configs.resource_service, ResourceService::parse(&empty).unwrap()),
        SectionId::AccessibilityNeeds => put(&configs.accessibility_needs, AccessibilityNeeds::parse(&empty).unwrap()),
        SectionId::ExtensionsEnabled => put(&configs.extensions_enabled, false),
    }
}

fn text(value: &str) -> Arc<str> {
    Arc::from(value)
}

impl Host for TestHost {
    type Error = Error;

    fn validate_candidate(&mut self, kind: TxnKind, stage: Stage) -> Result<(), Error> {
        self.calls.push(Call::Validate(kind, stage));
        Ok(())
    }

    fn canonicalized(&mut self, kind: TxnKind, _before: &Snapshot) -> Result<(), Error> {
        self.calls.push(Call::Canonicalized(kind));
        Ok(())
    }

    fn restored(&mut self, kind: TxnKind) -> Result<(), Error> {
        self.calls.push(Call::Restored(kind));
        Ok(())
    }

    fn committed(&mut self, kind: TxnKind) -> Result<(), Error> {
        self.calls.push(Call::Committed(kind));
        Ok(())
    }

    fn validate_world(&mut self) -> Result<(), Error> {
        self.calls.push(Call::ValidateWorld);
        Ok(())
    }

    fn validate_extensions(&mut self) -> Result<(), Error> {
        self.calls.push(Call::ValidateExtensions);
        Ok(())
    }

    fn contract(&mut self, which: &Contract, input: &V) -> Result<V, Error> {
        self.calls.push(Call::Contract(which.clone(), input.clone()));
        match &mut self.contracts {
            Some(contract) => contract(which, input),
            None => Ok(input.clone()),
        }
    }

    fn llm(&self) -> Result<Llm, Error> {
        Ok(self.llm)
    }

    fn prioritize_support_request(&mut self, request: &str, agent: &str, hash: &str) -> Result<(), Error> {
        self.llm_ran();
        self.calls.push(Call::Prioritize { request: text(request), agent: text(agent), hash: text(hash) });
        Ok(())
    }

    fn consider_support_request(&mut self, agent: &str, request: &str) -> Result<(), Error> {
        self.llm_ran();
        self.calls.push(Call::SupportRequest { agent: text(agent), request: text(request) });
        Ok(())
    }

    fn consider_complaint_response(&mut self, agent: &str, request: &str) -> Result<(), Error> {
        self.llm_ran();
        self.calls.push(Call::Complaint { agent: text(agent), request: text(request) });
        Ok(())
    }

    fn consider_cooperation(&mut self, initiator: &str, participant: &str, trigger: &str) -> Result<(), Error> {
        self.llm_ran();
        self.calls.push(Call::Cooperation { initiator: text(initiator), participant: text(participant), trigger: text(trigger) });
        Ok(())
    }

    fn consider_information_reaction(&mut self, agent: &str, participant: &str, message: &str, trigger: &str) -> Result<(), Error> {
        self.llm_ran();
        self.calls.push(Call::InformationReaction {
            agent: text(agent),
            participant: text(participant),
            message: text(message),
            trigger: text(trigger),
        });
        Ok(())
    }

    fn capture_metric(&mut self, sample: &MetricSample, minute: i64) -> Result<(), Error> {
        self.calls.push(Call::Metric(sample.clone(), minute));
        Ok(())
    }

    fn decision_sink(&self) -> Result<DecisionSink, Error> {
        Ok(self.sink)
    }

    fn decision(&mut self, decision: Decision) -> Result<(), Error> {
        self.calls.push(Call::Decision(decision));
        Ok(())
    }

    fn decision_source(&mut self, _policy_version: &str) -> Result<Option<Option<Arc<str>>>, Error> {
        Ok(self.source.clone())
    }

    fn trace_level(&self) -> Result<TraceLevel, Error> {
        Ok(self.level)
    }

    fn trace(&mut self, document: Trace) -> Result<(), Error> {
        self.calls.push(Call::Trace(document));
        Ok(())
    }

    fn phase(&mut self, phase: Phase, before: bool) -> Result<(), Error> {
        self.calls.push(Call::Phase(phase, before));
        if let Some(hook) = &mut self.on_phase {
            hook(phase, before);
        }
        Ok(())
    }

    fn space_refreshed(&mut self) -> Result<(), Error> {
        self.calls.push(Call::SpaceRefreshed);
        Ok(())
    }

    fn holding(&mut self) -> Result<Option<Arc<Holding>>, Error> {
        self.calls.push(Call::Holding);
        Ok(self.holding.clone())
    }

    fn fill_section(&mut self, configs: &Arc<Configs>, which: SectionId) -> Result<Filled, Error> {
        self.calls.push(Call::FillSection(which));
        match self.sections.iter().find(|(id, _)| *id == which) {
            Some((_, fill)) => fill(configs),
            None => fill_section(configs, which),
        }
        Ok(Filled::Kept)
    }

    fn find_record(&mut self, event_id: &str) -> Result<Option<Logged>, Error> {
        self.calls.push(Call::FindRecord(text(event_id)));
        if let Some(found) = self.records.get(event_id) {
            return Ok(Some(found.clone()));
        }
        match &self.world {
            Some(world) => world.find_record(event_id),
            None => Ok(None),
        }
    }

    fn floor_plan(&mut self) -> Result<Arc<Geo>, Error> {
        self.calls.push(Call::FloorPlan);
        if let Some(world) = &mut self.world {
            return world.floor_plan();
        }
        self.plan.clone().ok_or_else(|| Error::unsupported(sheltersim_core::host::NO_PLAN))
    }

    fn start_namespace(&mut self, name: &str, _value: &V) -> Result<(), Error> {
        self.calls.push(Call::StartNamespace(text(name)));
        Ok(())
    }

    fn resources_changed(&mut self, index: usize, remaining: i64) -> Result<(), Error> {
        self.calls.push(Call::ResourcesChanged(index, remaining));
        Ok(())
    }

    fn apply_foreign(&mut self, event: &EventRow) -> Result<Foreign, Error> {
        self.calls.push(Call::Foreign(event.event_type.clone()));
        match &mut self.foreign {
            Some(handler) => handler(event),
            None => Err(Error::unsupported(NO_FOREIGN)),
        }
    }

    fn record_decision_python(
        &mut self,
        data: &ViewData,
        proposal: &Proposal,
        target_id: Option<&str>,
        next_decision_at: i64,
        _observed: &Observed<'_>,
    ) -> Result<(), Error> {
        self.calls.push(Call::DecisionPython {
            agent: data.agents[proposal.index].id.clone(),
            target: target_id.map(text),
            next: next_decision_at,
        });
        Ok(())
    }

    fn draw(&mut self, parts: &[Part]) -> Result<f64, Error> {
        self.calls.push(Call::Draw(parts.to_vec()));
        if self.draws.is_empty() {
            return Err(Error::unsupported(sheltersim_core::host::NO_DRAW));
        }
        Ok(self.draws.remove(0))
    }
}

/// A host that leaves every method of lane P9-0 to the trait's defaults, and refuses or passes
/// the rest: what a host that knows nothing of them answers.
pub struct Refusing;

impl Host for Refusing {
    type Error = Error;

    fn validate_candidate(&mut self, _kind: TxnKind, _stage: Stage) -> Result<(), Error> {
        Ok(())
    }

    fn validate_world(&mut self) -> Result<(), Error> {
        Ok(())
    }

    fn validate_extensions(&mut self) -> Result<(), Error> {
        Ok(())
    }

    fn contract(&mut self, _which: &Contract, input: &V) -> Result<V, Error> {
        Ok(input.clone())
    }

    fn llm(&self) -> Result<Llm, Error> {
        Ok(Llm::default())
    }

    fn prioritize_support_request(&mut self, _request: &str, _agent: &str, _hash: &str) -> Result<(), Error> {
        Ok(())
    }

    fn consider_support_request(&mut self, _agent: &str, _request: &str) -> Result<(), Error> {
        Ok(())
    }

    fn consider_complaint_response(&mut self, _agent: &str, _request: &str) -> Result<(), Error> {
        Ok(())
    }

    fn consider_cooperation(&mut self, _initiator: &str, _participant: &str, _trigger: &str) -> Result<(), Error> {
        Ok(())
    }

    fn consider_information_reaction(&mut self, _agent: &str, _participant: &str, _message: &str, _trigger: &str) -> Result<(), Error> {
        Ok(())
    }

    fn capture_metric(&mut self, _sample: &MetricSample, _minute: i64) -> Result<(), Error> {
        Ok(())
    }

    fn decision_sink(&self) -> Result<DecisionSink, Error> {
        Ok(DecisionSink::Silent)
    }

    fn decision(&mut self, _decision: Decision) -> Result<(), Error> {
        Ok(())
    }

    fn trace_level(&self) -> Result<TraceLevel, Error> {
        Ok(TraceLevel::Off)
    }

    fn trace(&mut self, _document: Trace) -> Result<(), Error> {
        Ok(())
    }

    fn phase(&mut self, _phase: Phase, _before: bool) -> Result<(), Error> {
        Ok(())
    }

    fn space_refreshed(&mut self) -> Result<(), Error> {
        Ok(())
    }
}
