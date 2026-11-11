//! P8-0b: a Run's `Policies` read from what the PyO3 crate read of a controller, the tick's
//! `Scratch`, and the `Host` with the test host a build without Python uses (`support::TestHost`).
//! The controllers here are built by hand as the PyO3 crate reads a real one: each class by its
//! names, each attribute a value, an object or what its read raised.

mod support;

use std::sync::Arc;

use indexmap::IndexMap;
use sheltersim_core::cell::Shard;
use sheltersim_core::config::{Fault, FaultClass};
use sheltersim_core::error::Error;
use sheltersim_core::host::{
    Contract, Decision, DecisionSink, ErrorClass, Host, HostFailure, Llm, Part, Phase, Stage, Trace, TraceLevel, TxnKind, NO_DRAW,
};
use sheltersim_core::mathx::MathError;
use sheltersim_core::run::held::{Class, Held, Obj, NOT_READ, NO_CONTROLLER};
use sheltersim_core::run::policies::CONTROLLER;
use sheltersim_core::run::{behavior, information, psychology, sinks, stress, Policies, Run, RunConfig, Scratch};
use sheltersim_core::txn::{Txn, TxnEnd};
use sheltersim_core::value::V;
use sheltersim_core::world::{Fit, World, WorldCell};
use support::{Call, TestHost};

fn text(value: &str) -> Arc<str> {
    Arc::from(value)
}

fn object(module: &str, name: &str, attrs: Vec<(&str, Result<Held, Fault>)>) -> Held {
    let mut found = Obj::new(Class::named(module, name));
    for (key, held) in attrs {
        found.attrs.insert(text(key), held);
    }
    Held::Object(Arc::new(found))
}

fn value(value: V) -> Result<Held, Fault> {
    Ok(Held::Value(value))
}

fn none() -> Result<Held, Fault> {
    value(V::Null)
}

fn with_class_own(held: Held, name: &str, own: Held) -> Held {
    let Held::Object(found) = held else { panic!("not an object") };
    let mut found = (*found).clone();
    found.class_own.insert(text(name), own);
    Held::Object(Arc::new(found))
}

/// A controller as `OfflineSimulationController.__init__` makes one for a generated Scenario.
fn controller() -> Obj {
    let parameters = object(
        "sheltersim.policies.psychology",
        "PsychologyParameters",
        vec![("toilet_growth", value(V::Float(0.001))), ("toilet_growth_by_agent", value(V::Map(IndexMap::new())))],
    );
    let psychology = object(
        "sheltersim.policies.psychology",
        "SimplePsychologyPolicy",
        vec![("policy_version", value(V::Str(text("causal-stress-v1")))), ("parameters", Ok(parameters))],
    );
    let scorer_fields = vec![("policy_version", value(V::Str(text("utility-softmax-v1")))), ("temperature", value(V::Float(0.2)))];
    let policy = object("sheltersim.policies.behavior", "UtilityBehaviorPolicy", scorer_fields);
    let decisions = object("sheltersim.policies.behavior", "BehaviorDecisionRunner", vec![("policy", Ok(policy))]);
    let stress_parameters = object("sheltersim.policies.social", "SocialPropagationParameters", vec![("enabled", value(V::Bool(false)))]);
    let stress = object("sheltersim.policies.social", "StressPropagationPolicy", vec![("parameters", Ok(stress_parameters))]);
    let config = object("sheltersim.policies.information", "InformationSpreadConfig", vec![("enabled", value(V::Bool(true)))]);
    let information = object("sheltersim.policies.information", "InformationSpreadPolicy", vec![("config", Ok(config))]);
    let toilet = object(
        "sheltersim.domain.facilities",
        "ToiletServiceManager",
        vec![("min_service_minutes", value(V::Int(3))), ("max_service_minutes", value(V::Int(8)))],
    );
    let staff = object(
        "sheltersim.domain.staff",
        "StaffService",
        vec![("cooldown_minutes", value(V::Int(30))), ("route_resolver", Ok(Held::Opaque { callable: true }))],
    );
    let artifacts = with_class_own(
        object("sheltersim.inspection.traces", "RunArtifactWriter", vec![("trace_level", Ok(Held::Text(text("summary"))))]),
        "decision_sink",
        Held::Value(V::Str(text("artifacts"))),
    );
    let mut found = Obj::new(Class::named("sheltersim.experiments.runner", "OfflineSimulationController"));
    for (key, held) in [
        ("psychology", Ok(psychology)),
        ("decisions", Ok(decisions)),
        ("stress_policy", Ok(stress)),
        ("information_policy", Ok(information)),
        ("proximity_contact_config", none()),
        ("proximity_policy", Ok(object("sheltersim.policies.proximity", "DisabledProximityPolicy", vec![]))),
        ("social_policy", none()),
        ("toilet_service", Ok(toilet)),
        ("resource_service", none()),
        ("staff_service", Ok(staff)),
        ("artifacts", Ok(artifacts)),
        ("llm_router", none()),
    ] {
        found.attrs.insert(text(key), held);
    }
    found
}

fn put(found: &mut Obj, key: &str, held: Result<Held, Fault>) {
    found.attrs.insert(text(key), held);
}

#[test]
fn the_spec_reads_each_part_of_the_controller_once() {
    let names: Vec<&str> = CONTROLLER.attrs.iter().map(|attr| attr.name).collect();
    let mut unique = names.clone();
    unique.sort_unstable();
    unique.dedup();
    assert_eq!(unique.len(), names.len());
    // Every attribute the hand-built controller has is one the spec reads.
    for key in controller().attrs.keys() {
        assert!(names.contains(&&**key), "{key}");
    }
}

#[test]
fn policies_of_a_controller_know_its_models_and_sinks() {
    let policies = Policies::of(Arc::new(controller()));
    assert!(policies.has_controller());
    assert_eq!(policies.psychology.model, Ok(psychology::Model::Simple));
    let parameters = policies.psychology.parameters().unwrap();
    assert_eq!(parameters.value("toilet_growth"), Ok(&V::Float(0.001)));
    assert_eq!(parameters.map("toilet_growth_by_agent").map(IndexMap::len), Ok(0));
    assert_eq!(policies.behavior.rule, Ok(behavior::Rule::Utility));
    assert_eq!(policies.behavior.scorer.as_ref().unwrap().value("temperature"), Ok(&V::Float(0.2)));
    assert_eq!(policies.stress.model, Ok(stress::Model::Sparse));
    assert_eq!(policies.stress.parameters().unwrap().value("enabled"), Ok(&V::Bool(false)));
    assert_eq!(policies.information.model, Ok(Some(information::Model::Sparse)));
    assert_eq!(policies.information.config().unwrap().unwrap().value("enabled"), Ok(&V::Bool(true)));
    assert_eq!(policies.proximity.config, Ok(V::Null));
    assert_eq!(policies.proximity.known, Ok(false));
    assert_eq!(policies.social.policy, Ok(None));
    assert_eq!(policies.social.complaint_wait_minutes(), Ok(None));
    assert_eq!(policies.services.toilet.known, Ok(true));
    assert_eq!(policies.services.toilet.object().unwrap().int("max_service_minutes"), Ok(8));
    assert_eq!(policies.services.resource.is_none(), Ok(true));
    assert_eq!(policies.services.resource.known, Ok(false));
    assert_eq!(policies.services.staff.known, Ok(true));
    assert_eq!(policies.services.staff.object().unwrap().get("route_resolver"), Ok(&Held::Opaque { callable: true }));
    assert_eq!(policies.sinks.decision_sink, Ok(DecisionSink::Artifacts));
    assert_eq!(policies.sinks.trace_level, Ok(TraceLevel::Summary));
    assert_eq!(policies.llm(), Ok(Llm { router: false, social_policy: false }));
    // An attribute the spec of a part does not read is a refusal, not a guess.
    assert_eq!(policies.services.staff.object().unwrap().get("fairness_after_minutes"), Err(Fault::refused(NOT_READ)));
}

#[test]
fn a_class_another_kernel_does_not_know_is_its_refusal() {
    let mut found = controller();
    // A subclass in a test module: psychology knows its model by names, the others by `is`.
    let psychology = object("tests.helpers", "SimplePsychologyPolicy", vec![]);
    put(&mut found, "psychology", Ok(psychology));
    let decisions = object("sheltersim.policies.behavior", "BehaviorDecisionRunner", vec![(
        "policy",
        Ok(object("sheltersim.policies.behavior", "PriorityRuleBehaviorPolicy", vec![(
            "scorer",
            Ok(object("tests.helpers", "Counting", vec![])),
        )])),
    )]);
    put(&mut found, "decisions", Ok(decisions));
    let information = object("tests.helpers", "Other", vec![]);
    put(&mut found, "information_policy", Ok(information));
    let policies = Policies::of(Arc::new(found));
    assert_eq!(policies.psychology.model, Err(Fault::refused(psychology::UNKNOWN)));
    assert_eq!(policies.behavior.rule, Err(Fault::refused(behavior::OTHER_SCORER)));
    assert!(policies.behavior.policy.is_ok());
    assert_eq!(policies.information.model, Err(Fault::refused(information::UNKNOWN)));

    // A class whose name finds another class: known by names, not by `is`.
    let mut shadowed = Class::named("sheltersim.policies.behavior", "UtilityBehaviorPolicy");
    shadowed.resolves = false;
    assert!(shadowed.has_name("sheltersim.policies.behavior", "UtilityBehaviorPolicy"));
    assert!(!shadowed.is("sheltersim.policies.behavior", "UtilityBehaviorPolicy"));
    let mut found = controller();
    let mut runner = Obj::new(Class::named("sheltersim.policies.behavior", "BehaviorDecisionRunner"));
    runner.attrs.insert(text("policy"), Ok(Held::Object(Arc::new(Obj::new(shadowed)))));
    put(&mut found, "decisions", Ok(Held::Object(Arc::new(runner))));
    let mut psychology = Obj::new(Class::named("sheltersim.policies.psychology", "MeanRevertingDistressPolicy"));
    psychology.class.resolves = false;
    put(&mut found, "psychology", Ok(Held::Object(Arc::new(psychology))));
    let policies = Policies::of(Arc::new(found));
    assert_eq!(policies.behavior.rule, Err(Fault::refused(behavior::OTHER_POLICY)));
    assert_eq!(policies.psychology.model, Ok(psychology::Model::MeanReverting));
    let unnamed = Obj::new(Class { module: None, qualname: Some(text("X")), resolves: false });
    assert_eq!(psychology::model_of(&unnamed), Err(Fault::refused(psychology::NO_PLAIN_NAME)));
}

#[test]
fn what_a_read_raised_is_kept_for_the_read_site() {
    let raised = Fault::new(FaultClass::Other(text("builtins.AttributeError")), "no attribute 'psychology'");
    let mut found = controller();
    put(&mut found, "psychology", Err(raised.clone()));
    put(&mut found, "llm_router", Err(raised.clone()));
    // A sink declared with no str is a TypeError at the read, as `downcast_exact` raises it.
    let artifacts = with_class_own(
        object("tests.helpers", "Sink", vec![("trace_level", Ok(Held::Value(V::Str(text("verbose")))))]),
        "decision_sink",
        Held::Value(V::Int(1)),
    );
    put(&mut found, "artifacts", Ok(artifacts));
    let policies = Policies::of(Arc::new(found));
    assert_eq!(policies.psychology.model, Err(raised.clone()));
    assert_eq!(policies.psychology.policy, Err(raised.clone()));
    assert!(!policies.psychology.model.as_ref().unwrap_err().is_refusal());
    assert_eq!(policies.sinks.llm_router, Err(raised.clone()));
    assert_eq!(policies.llm(), Err(raised));
    assert_eq!(policies.sinks.decision_sink.as_ref().map_err(|fault| fault.class.clone()), Err(FaultClass::Type));
    assert_eq!(policies.sinks.trace_level, Err(Fault::refused(sinks::UNKNOWN_LEVEL)));

    // No artifacts, or a class without its own declaration: `record_decision` runs in Python.
    let mut found = controller();
    put(&mut found, "artifacts", none());
    let policies = Policies::of(Arc::new(found));
    assert_eq!(policies.sinks.decision_sink, Ok(DecisionSink::Python));
    assert_eq!(policies.sinks.trace_level.as_ref().map_err(Fault::is_refusal), Err(false));
    let mut found = controller();
    put(&mut found, "artifacts", Ok(object("tests.helpers", "Sink", vec![("trace_level", Ok(Held::Value(V::Str(text("off")))))])));
    let policies = Policies::of(Arc::new(found));
    assert_eq!(policies.sinks.decision_sink, Ok(DecisionSink::Python));
    assert_eq!(policies.sinks.trace_level, Ok(TraceLevel::Off));
}

#[test]
fn a_social_policy_gives_its_limits_and_the_llm_flag() {
    let mut found = controller();
    let limits = object("sheltersim.llm.runtime", "SocialRuntimeLimits", vec![("complaint_wait_minutes", value(V::Int(45)))]);
    let social = object("sheltersim.llm.runtime", "EventTriggeredSocialPolicy", vec![("limits", Ok(limits))]);
    put(&mut found, "social_policy", Ok(social));
    put(&mut found, "llm_router", Ok(Held::Opaque { callable: false }));
    let policies = Policies::of(Arc::new(found));
    assert_eq!(policies.social.complaint_wait_minutes(), Ok(Some(45)));
    assert_eq!(policies.llm(), Ok(Llm { router: true, social_policy: true }));

    let mut found = controller();
    let limits = object("sheltersim.llm.runtime", "SocialRuntimeLimits", vec![("complaint_wait_minutes", value(V::Float(45.0)))]);
    let social = object("sheltersim.llm.runtime", "EventTriggeredSocialPolicy", vec![("limits", Ok(limits))]);
    put(&mut found, "social_policy", Ok(social));
    let policies = Policies::of(Arc::new(found));
    assert_eq!(policies.social.complaint_wait_minutes(), Err(Fault::refused("a value is not an int")));
}

#[test]
fn a_run_without_a_controller_refuses_every_policy() {
    let run = Run::new(RunConfig { route_capacities: Err("no capacities") });
    let refused = Fault::refused(NO_CONTROLLER);
    assert!(!run.policies.has_controller());
    assert_eq!(run.policies.psychology.model, Err(refused.clone()));
    assert_eq!(run.policies.behavior.rule, Err(refused.clone()));
    assert_eq!(run.policies.sinks.trace_level, Err(refused.clone()));
    assert_eq!(run.policies.llm(), Err(refused.clone()));
    assert_eq!(run.policies.services.staff.known, Err(refused));
    assert!(run.scratch.is_empty());
}

#[test]
fn scratch_keeps_a_ticks_values_by_type_until_the_next_tick() {
    #[derive(Debug, PartialEq)]
    struct View(i64);
    #[derive(Debug, PartialEq)]
    struct Deltas(Vec<f64>);
    let mut scratch = Scratch::default();
    assert_eq!(scratch.minute(), None);
    scratch.begin(10);
    assert_eq!(scratch.put(View(10)), None);
    assert_eq!(scratch.put(Deltas(vec![0.5])), None);
    assert_eq!(scratch.put(View(11)), Some(View(10)));
    assert_eq!(scratch.get::<View>(), Some(&View(11)));
    scratch.get_mut::<Deltas>().unwrap().0.push(0.25);
    assert_eq!(scratch.take::<Deltas>(), Some(Deltas(vec![0.5, 0.25])));
    assert_eq!(scratch.take::<Deltas>(), None);
    assert_eq!(scratch.len(), 1);
    scratch.begin(11);
    assert_eq!(scratch.minute(), Some(11));
    assert!(scratch.is_empty());
    assert_eq!(scratch.get::<View>(), None);
}

#[test]
fn the_test_host_takes_a_transaction() {
    let cell = Shard::new(World::default());
    let mut host = TestHost::default();
    let kept = Txn::run(&cell, &mut host, TxnKind::Effect, |cell, _| {
        cell.with(|world| world.scalars.get_mut().minute_index = Fit::Ok(3))?;
        Ok(TxnEnd::Commit(()))
    });
    assert_eq!(kept, Ok(()));
    assert_eq!(
        host.calls,
        [
            Call::Validate(TxnKind::Effect, Stage::World),
            Call::Canonicalized(TxnKind::Effect),
            Call::Validate(TxnKind::Effect, Stage::Candidate),
            Call::Committed(TxnKind::Effect),
        ]
    );
}

/// A rule over a cell and a host as the core's rules will be (rule R6 of design-p8.md): it reads
/// the world, lets go of it, calls the host, and reads the world again after the call.
fn complain<C: WorldCell, H: Host>(cell: &C, host: &mut H, due: &[(&str, &str)]) -> Result<i64, H::Error> {
    let minute = cell.with(|world| world.scalars.get_mut().minute_index.clone())?;
    let Fit::Ok(minute) = minute else {
        return Err(Error::unsupported("no minute").into());
    };
    if host.llm()?.social_policy {
        for (agent, request) in due {
            host.consider_complaint_response(agent, request)?;
        }
    }
    if host.trace_level()? != TraceLevel::Off {
        host.trace(Trace::SocialContact(V::Int(minute)))?;
    }
    if host.decision_sink()? == DecisionSink::Indexed {
        host.decision(Decision::Indexed(b"{}".to_vec()))?;
    }
    let after = cell.with(|world| world.scalars.get_mut().minute_index.clone())?;
    match after {
        Fit::Ok(after) => Ok(after),
        _ => Err(Error::unsupported("no minute").into()),
    }
}

#[test]
fn a_rule_asks_the_host_and_reads_the_world_again() {
    let mut world = World::default();
    world.scalars.get_mut().minute_index = Fit::Ok(7);
    let cell = Shard::new(world);
    // Without an LLM, as every gate and 7-day Run: the LLM is not asked, nothing is recorded.
    let mut host = TestHost::default();
    assert_eq!(complain(&cell, &mut host, &[("a", "r1")]), Ok(7));
    assert!(host.calls.is_empty());
    let mut host = TestHost {
        llm: Llm { router: false, social_policy: true },
        level: TraceLevel::Full,
        sink: DecisionSink::Indexed,
        ..TestHost::default()
    };
    assert_eq!(complain(&cell, &mut host, &[("a", "r1"), ("b", "r2")]), Ok(7));
    assert_eq!(
        host.calls,
        [
            Call::Complaint { agent: text("a"), request: text("r1") },
            Call::Complaint { agent: text("b"), request: text("r2") },
            Call::Trace(Trace::SocialContact(V::Int(7))),
            Call::Decision(Decision::Indexed(b"{}".to_vec())),
        ]
    );
}

#[test]
fn the_test_host_gives_contracts_back_and_draws_only_when_told() {
    let mut host = TestHost::default();
    let input = V::Map([(text("zone_id"), V::Str(text("hall")))].into_iter().collect());
    assert_eq!(host.contract(&Contract::EffectParameters(text("set_lighting")), &input), Ok(input.clone()));
    assert_eq!(host.calls, [Call::Contract(Contract::EffectParameters(text("set_lighting")), input)]);
    let parts = [Part::Text(text("toilet_service")), Part::Int(5)];
    assert_eq!(host.draw(&parts), Err(Error::Unsupported(NO_DRAW.into())));
    host.draws = vec![0.25];
    assert_eq!(host.draw(&parts), Ok(0.25));
    host.phase(Phase::CollectMetrics, false).unwrap();
    assert_eq!(host.calls.last(), Some(&Call::Phase(Phase::CollectMetrics, false)));
    host.validate_world().unwrap();
    host.validate_extensions().unwrap();
    host.space_refreshed().unwrap();
}

#[test]
fn contracts_name_their_models_and_phases_their_python_values() {
    assert_eq!(Contract::Guard.model(), ("sheltersim.events.effects", "GuardExpression"));
    assert_eq!(Contract::EffectParameters(text("set_privacy")).model(), ("sheltersim.events.effects", "_PARAMETER_MODELS"));
    assert_eq!(Contract::TemporaryRoom.model(), ("sheltersim.contracts.operations", "TemporaryRoomGeometry"));
    let names: Vec<&str> = Phase::ORDER.iter().map(|phase| phase.name()).collect();
    assert_eq!(
        names,
        [
            "dispatch_due_events",
            "complete_scheduled_tasks",
            "update_agent_state",
            "propose_actions",
            "arbitrate_actions",
            "sample_endogenous_events",
            "collect_metrics",
        ]
    );
}

#[test]
fn core_errors_have_the_class_python_gives_them() {
    assert_eq!(Error::unsupported("x").class(), ErrorClass::Refused);
    assert_eq!(Error::Value("x".into()).class(), ErrorClass::Value);
    assert_eq!(Error::suppressed("x").class(), ErrorClass::Value);
    assert_eq!(Error::Invariant("x".into()).class(), ErrorClass::Value);
    assert_eq!(Error::refused("rules.x", Error::unsupported("y")).class(), ErrorClass::Value);
    assert_eq!(Error::Key("x".into()).class(), ErrorClass::Key);
    assert_eq!(Error::Type("x".into()).class(), ErrorClass::Type);
    assert_eq!(Error::Runtime("x".into()).class(), ErrorClass::Other);
    assert_eq!(Error::Math(MathError::Range).class(), ErrorClass::Other);
    assert_eq!(Error::Math(MathError::NegativeToFraction).class(), ErrorClass::Value);
}
