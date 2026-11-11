//! The sections of lane 7-5b, parsed from the configs as a `V`: each gives the value the kernels
//! read, or the refusal they said when they read the Python tree, in the same order of checks.

use std::collections::BTreeSet;
use std::sync::Arc;

use indexmap::IndexMap;
use serde_json::{json, Value};
use sheltersim_core::config::agent_dynamics::{
    awareness, canonical_policy, knowledge_is_limited, policy, MUST_CITE, UNSUPPORTED_FIELDS, UNSUPPORTED_VERSION,
    V1_SOURCE,
};
use sheltersim_core::config::body::{social_interaction_minutes, AffectModel, BodyModels, CareOutcome, TrustModel};
use sheltersim_core::config::environment::{Infrastructure, ZoneEnvironmentSpec};
use sheltersim_core::config::information::InformationSpread;
use sheltersim_core::config::proximity::Proximity;
use sheltersim_core::config::read::Read;
use sheltersim_core::config::staff::{
    known_staff_operations, support_task_minutes, StaffFatigue, StaffOperations, OPERATIONS_POLICY, OPERATIONS_SOURCE,
};
use sheltersim_core::config::{parse_configs, Configs, Fault, Section};
use sheltersim_core::row::ValueRow;
use sheltersim_core::world::{Place, World, MODEL_CONFIGS};
use sheltersim_core::state::agent_dynamics::{POLICY_V1, POLICY_V2, POLICY_V3};
use sheltersim_core::value::V;

fn v(value: Value) -> V {
    match value {
        Value::Null => V::Null,
        Value::Bool(flag) => V::Bool(flag),
        Value::Number(number) => match number.as_i64() {
            Some(int) if !number.to_string().contains(['.', 'e', 'E']) => V::Int(int),
            _ if !number.to_string().contains(['.', 'e', 'E']) => V::Big(number.to_string().into()),
            _ => V::Float(number.as_f64().expect("a float")),
        },
        Value::String(text) => V::Str(Arc::from(text)),
        Value::Array(items) => V::List(items.into_iter().map(v).collect()),
        Value::Object(map) => V::Map(map.into_iter().map(|(key, item)| (Arc::from(key), v(item))).collect::<IndexMap<_, _>>()),
    }
}

fn refused<T: std::fmt::Debug>(read: Read<T>) -> String {
    let fault = read.expect_err("a refusal");
    assert!(fault.is_refusal(), "{fault:?}");
    fault.text.to_string()
}

#[test]
fn the_fatigue_formula_and_the_limits_of_each_member() {
    let formula = StaffFatigue::parse(&v(json!({}))).unwrap();
    assert!(!formula.saturating);
    assert_eq!(formula.limit("s1"), Ok(None));
    let configs = v(json!({
        "policy_registry": {"staff_fatigue": "staff-fatigue-saturating-v1"},
        "staff_dynamics": {"max_continuous_work_minutes_by_staff": {"s1": 90, "s2": "x", "s3": 1e30}},
    }));
    let formula = StaffFatigue::parse(&configs).unwrap();
    assert!(formula.saturating);
    assert_eq!(formula.limit("s1"), Ok(Some(90)));
    assert_eq!(formula.limit("s4"), Ok(None));
    // A member's limit is refused where it is read, as `exact_int` refused it there.
    assert_eq!(refused(formula.limit("s2")), "a value is not an int");
    assert_eq!(refused(formula.limit("s3")), "a value is not an int");
    let big = v(json!({"staff_dynamics": {"max_continuous_work_minutes_by_staff": {"s1": 18446744073709551616u128}}}));
    assert_eq!(refused(StaffFatigue::parse(&big).unwrap().limit("s1")), "an int is too large");
    // An empty map is no limits at all.
    let empty = v(json!({"staff_dynamics": {"max_continuous_work_minutes_by_staff": {}}}));
    assert_eq!(StaffFatigue::parse(&empty).unwrap(), StaffFatigue::default());
    for (configs, reason) in [
        (json!([]), "a value is not a plain dict"),
        (json!({"policy_registry": []}), "a value is not a plain dict"),
        (json!({"policy_registry": {"staff_fatigue": 1}}), "a value is not a str"),
        (json!({"policy_registry": {"staff_fatigue": "x"}}), "another fatigue formula"),
        (json!({"staff_dynamics": 3}), "a value is not a plain dict"),
        (json!({"staff_dynamics": {"max_continuous_work_minutes_by_staff": []}}), "a value is not a plain dict"),
    ] {
        assert_eq!(refused(StaffFatigue::parse(&v(configs))), reason);
    }
}

#[test]
fn the_staff_operations_formula_and_its_provenance() {
    let default = StaffOperations { model_id: Arc::from("staff-urgency-first-v1"), selected_source: None };
    assert!(!default.travel_first());
    assert_eq!(default.provenance(), (OPERATIONS_SOURCE, OPERATIONS_POLICY, OPERATIONS_POLICY));
    let selected = StaffOperations { model_id: Arc::from("staff-travel-first-v1"), selected_source: Some(Arc::from("ASM-X")) };
    assert!(selected.travel_first());
    assert_eq!(selected.provenance(), ("ASM-X", "staff-travel-first-v1", "ASM-X"));

    assert_eq!(known_staff_operations(&v(json!({}))), Ok(()));
    assert_eq!(known_staff_operations(&v(json!({"policy_registry": {"staff_operations": "staff-travel-first-v1"}}))), Ok(()));
    for (configs, reason) in [
        (json!(null), "a value is not a plain dict"),
        (json!({"policy_registry": "x"}), "a value is not a plain dict"),
        (json!({"policy_registry": {"staff_operations": 2}}), "a value is not a str"),
        (json!({"policy_registry": {"staff_operations": "other"}}), "another staff operations formula"),
    ] {
        assert_eq!(refused(known_staff_operations(&v(configs))), reason);
    }
}

#[test]
fn the_minutes_of_a_support_task_and_of_a_social_action() {
    assert_eq!(support_task_minutes(&v(json!({}))), Ok(10));
    assert_eq!(support_task_minutes(&v(json!({"staff": {}}))), Ok(10));
    assert_eq!(support_task_minutes(&v(json!({"staff": {"support_task_minutes": 25}}))), Ok(25));
    assert_eq!(refused(support_task_minutes(&v(json!({"staff": {"support_task_minutes": 2.0}})))), "a value is not an int");
    assert_eq!(refused(support_task_minutes(&v(json!({"staff": []})))), "a value is not a plain dict");
    assert_eq!(social_interaction_minutes(&v(json!({}))), Ok(5));
    assert_eq!(social_interaction_minutes(&v(json!({"body_dynamics": {"social_interaction_minutes": 9}}))), Ok(9));
    assert_eq!(refused(social_interaction_minutes(&v(json!({"body_dynamics": {"social_interaction_minutes": true}})))), "a value is not an int");
    assert_eq!(refused(social_interaction_minutes(&v(json!(7)))), "a value is not a plain dict");
}

#[test]
fn the_information_config_is_read_where_the_rules_read_it() {
    let config = v(json!({
        "present": true,
        "enabled": true,
        "minimum_contact_minutes": 3,
        "default_source_reliability": 0.7,
        "correction_source_reliability_by_id": {"SRC-A": 1, "SRC-B": "x"},
        "rumor_belief_probability": 0.5,
        "rumor_trust_sensitivity": 0.2,
        "rumor_source_sensitivity": 0.2,
        "query_pressure_per_claim": 0.5,
        "query_pressure_relief_per_correction": 0.5,
        "correction_acceptance_probability": 0.7,
        "correction_trust_sensitivity": 0.2,
        "correction_source_sensitivity": true,
    }));
    let spread = InformationSpread::from_v(&config).unwrap();
    assert!(spread.present && spread.enabled);
    assert_eq!(spread.minimum_contact_minutes, Ok(3));
    assert_eq!(spread.reliability("SRC-A"), Ok(1.0));
    assert_eq!(spread.reliability("SRC-C"), Ok(0.7));
    assert_eq!(refused(spread.reliability("SRC-B")), "a value is not an int");
    assert_eq!(spread.query_pressure_per_claim, Ok(0.5));
    // `number` refuses a bool, where the rule reads it.
    assert_eq!(refused(spread.correction_source_sensitivity.clone()), "a value is not an int");
    // Without the key the config is the class's own, and a reader by `[...]` refuses it.
    let absent = InformationSpread::from_v(&v(json!({"correction_source_reliability_by_id": {}}))).unwrap();
    assert!(!absent.present && !absent.enabled);
    assert_eq!(refused(InformationSpread::from_v(&v(json!({"correction_source_reliability_by_id": []})))), "a value is not a plain dict");
}

#[test]
fn the_body_operators_of_the_models_this_kernel_knows() {
    let none = v(json!({}));
    assert_eq!(
        BodyModels::parse("linear-positive-negative-v1", None, &none),
        Ok(BodyModels { affect: AffectModel::Linear, trust: TrustModel::DurationOnly })
    );
    assert_eq!(
        BodyModels::parse("trust-weighted-positive-negative-v1", Some("duration-contact-trust-v1"), &none).unwrap().affect,
        AffectModel::TrustWeighted
    );
    let distance = "distance-duration-repeat-trust-v1";
    assert_eq!(
        BodyModels::parse("linear-positive-negative-v1", Some(distance), &none).unwrap().trust,
        TrustModel::DistanceDurationRepeat { distance_scale_m: 2.0, repeat_contact_bonus: 0.05 }
    );
    let configs = v(json!({"body_dynamics": {"relationship_distance_scale_m": 4, "relationship_repeat_contact_bonus": 0.1}}));
    assert_eq!(
        BodyModels::parse("linear-positive-negative-v1", Some(distance), &configs).unwrap().trust,
        TrustModel::DistanceDurationRepeat { distance_scale_m: 4.0, repeat_contact_bonus: 0.1 }
    );
    for (affect, relationship, configs, reason) in [
        ("other", None, json!({}), "an affect model this kernel doesn't know"),
        ("linear-positive-negative-v1", Some("other"), json!({}), "a relationship model this kernel doesn't know"),
        ("linear-positive-negative-v1", Some(distance), json!({"body_dynamics": {"relationship_distance_scale_m": 0}}), "the distance scale is not positive"),
        ("linear-positive-negative-v1", Some(distance), json!({"body_dynamics": {"relationship_distance_scale_m": null}}), "a relationship parameter is not a number"),
        ("linear-positive-negative-v1", Some(distance), json!({"body_dynamics": []}), "a value is not a plain dict"),
        ("linear-positive-negative-v1", Some(distance), json!([]), "a value is not a plain dict"),
    ] {
        assert_eq!(refused(BodyModels::parse(affect, relationship, &v(configs))), reason);
    }
}

#[test]
fn the_care_formula_gives_the_deltas_of_its_config() {
    let config = v(json!({
        "caregiver_assistance_stress_relief": 0.1,
        "caregiver_assistance_reassurance_gain_by_agent": {"a1": 0.5},
        "language_access_uncertainty_relief": null,
    }));
    let typed = CareOutcome::new(Arc::from("typed-care-support-response-v1"), Arc::from("ASM-C"), config.clone()).unwrap();
    assert_eq!(typed.deltas("caregiver_assistance", "a1"), Ok(vec![("stress", -0.1), ("reassurance", 0.5)]));
    assert_eq!(typed.deltas("caregiver_assistance", "a2"), Ok(vec![("stress", -0.1), ("reassurance", 0.02)]));
    assert_eq!(typed.deltas("medication_support", "a1"), Ok(vec![("uncertainty", -0.01)]));
    assert_eq!(typed.deltas("other", "a1"), Ok(vec![]));
    // A value there that is None is read, and refused, as `number` refused it.
    assert_eq!(refused(typed.deltas("language_access", "a1")), "a value is not an int");
    let fixed = CareOutcome::new(Arc::from("hygiene-support-fixed-relief-v1"), Arc::from("ASM-C"), config.clone()).unwrap();
    assert_eq!(fixed.deltas("caregiver_assistance", "a1"), Ok(vec![]));
    assert_eq!(fixed.hygiene_after(0.5, 0.2), 0.3);
    let proportional = CareOutcome::new(Arc::from("hygiene-support-proportional-relief-v1"), Arc::from("ASM-C"), config).unwrap();
    assert_eq!(proportional.hygiene_after(0.5, 0.2), 0.5 * (1.0 - 0.2));
    assert_eq!(proportional.hygiene_after(2.0, -1.0), 1.0);
    assert_eq!(refused(CareOutcome::new(Arc::from("x"), Arc::from("y"), V::Null)), "a value is not a plain dict");
}

#[test]
fn the_policy_of_agent_dynamics_in_the_order_of_the_python_checks() {
    let raw = |value: Value| v(value);
    assert_eq!(canonical_policy(None), Ok(Ok(POLICY_V1)));
    assert_eq!(canonical_policy(Some(&V::Null)), Ok(Ok(POLICY_V1)));
    assert_eq!(canonical_policy(Some(&raw(json!(3)))), Ok(Err(UNSUPPORTED_FIELDS)));
    assert_eq!(canonical_policy(Some(&raw(json!({"other": 1})))), Ok(Err(UNSUPPORTED_FIELDS)));
    assert_eq!(canonical_policy(Some(&raw(json!({"schema_version": 1.0})))), Ok(Ok(POLICY_V1)));
    assert_eq!(canonical_policy(Some(&raw(json!({"schema_version": true})))), Ok(Ok(POLICY_V1)));
    assert_eq!(canonical_policy(Some(&raw(json!({"schema_version": 2})))), Ok(Err(UNSUPPORTED_VERSION)));
    assert_eq!(canonical_policy(Some(&raw(json!({"schema_version": "1"})))), Ok(Err(UNSUPPORTED_VERSION)));
    assert_eq!(canonical_policy(Some(&raw(json!({"policy_version": 3})))), Ok(Err(UNSUPPORTED_VERSION)));
    assert_eq!(refused(canonical_policy(Some(&raw(json!({"policy_version": []}))))), "an unhashable policy");
    assert_eq!(canonical_policy(Some(&raw(json!({"source_id": "x"})))), Ok(Err(V1_SOURCE)));
    assert_eq!(canonical_policy(Some(&raw(json!({"policy_version": POLICY_V2})))), Ok(Ok(POLICY_V2)));
    assert_eq!(
        canonical_policy(Some(&raw(json!({"policy_version": POLICY_V3, "source_id": "ASM-INTAKE-FACILITY-ORIENTATION-V1"})))),
        Ok(Ok(POLICY_V3))
    );
    assert_eq!(canonical_policy(Some(&raw(json!({"policy_version": POLICY_V3, "source_id": 1})))), Ok(Err(MUST_CITE)));
    assert_eq!(policy(&raw(json!({}))), Ok(POLICY_V1));
    assert_eq!(policy(&raw(json!({"agent_dynamics": {"policy_version": POLICY_V2}}))), Ok(POLICY_V2));
    assert_eq!(refused(policy(&raw(json!({"agent_dynamics": {"x": 1}})))), "a config the Python rule refuses");
    assert_eq!(refused(policy(&raw(json!([])))), "a value is not a plain dict");
}

#[test]
fn the_facility_awareness_and_whether_knowledge_is_limited() {
    assert_eq!(awareness(&v(json!({}))), Ok(None));
    assert_eq!(awareness(&v(json!({"facility_awareness": null}))), Ok(None));
    let read = awareness(&v(json!({"facility_awareness": {
        "schema_version": 1,
        "initial_known_facility_ids": ["toilet-b", "toilet-a"],
        "discover_co_located_facilities_on_arrival": false,
        "source_type": "assumption",
        "source_id": "ASM-FACILITY-ENTRY-ORIENTATION-V1",
    }})))
    .unwrap()
    .unwrap();
    assert_eq!(read.initial, vec![Arc::<str>::from("toilet-a"), Arc::from("toilet-b")]);
    assert!(!read.discover);
    for (document, reason) in [
        (json!(1), "a value is not a plain dict"),
        (json!({"schema_version": 2}), "another awareness schema"),
        (json!({"schema_version": "1"}), "a value is not an int"),
        (json!({"initial_known_facility_ids": "a"}), "a value is not a plain list"),
        (json!({"initial_known_facility_ids": [1]}), "a value is not a str"),
        (json!({"initial_known_facility_ids": ["a b"]}), "an initial facility is not a stable ID"),
        (json!({"initial_known_facility_ids": ["a", "a"]}), "an initial facility repeats"),
        (json!({"discover_co_located_facilities_on_arrival": 1}), "discovery is not a bool"),
        (json!({"source_type": "survey"}), "another awareness source type"),
        (json!({"source_id": "ASM-OTHER"}), "another awareness source"),
        (json!({"other": 1}), "the awareness config has other fields"),
    ] {
        assert_eq!(refused(awareness(&v(json!({"facility_awareness": document})))), reason);
    }
    assert_eq!(knowledge_is_limited(&v(json!({}))), Ok(false));
    assert_eq!(knowledge_is_limited(&v(json!({"agent_dynamics": null}))), Ok(true));
    assert_eq!(knowledge_is_limited(&v(json!({"facility_awareness": {}}))), Ok(true));
    assert_eq!(refused(knowledge_is_limited(&V::Null)), "a value is not a plain dict");
}

#[test]
fn the_environment_specs_and_the_dependents_of_a_component() {
    let spec = ZoneEnvironmentSpec::from_v(&v(json!({"policy_version": "zone-environment-v2", "source_id": "ASM-Z"}))).unwrap();
    assert_eq!((&*spec.policy_version, &*spec.source_id), ("zone-environment-v2", "ASM-Z"));
    let infrastructure = Infrastructure::from_v(&v(json!({
        "source_id": "ASM-I",
        "dependency_edges": [
            {"dependent_id": "pump", "dependency_id": "grid"},
            {"dependent_id": "toilet-1", "dependency_id": "pump"},
            {"dependent_id": "lamp", "dependency_id": "grid"},
            {"dependent_id": "grid", "dependency_id": "lamp"},
        ],
    })))
    .unwrap();
    assert_eq!(&*infrastructure.source_id, "ASM-I");
    let facilities: BTreeSet<String> = ["toilet-1", "lamp", "kitchen"].map(String::from).into();
    assert_eq!(infrastructure.dependent_facilities("grid", &facilities), vec!["lamp", "toilet-1"]);
    assert_eq!(infrastructure.dependent_facilities("kitchen", &facilities), vec!["kitchen"]);
    assert_eq!(infrastructure.dependent_facilities("other", &facilities), Vec::<String>::new());
    assert_eq!(Infrastructure::from_v(&v(json!({"source_id": "ASM-I"}))).unwrap().dependent_facilities("grid", &facilities), Vec::<String>::new());
}

#[test]
fn the_proximity_config_is_none_where_the_sampler_is_off() {
    assert_eq!(Proximity::from_v(&V::Null), Ok(None));
    let config = Proximity::from_v(&v(json!({"policy_version": "distance-sampled-proximity-v1", "max_distance_m": 2.5})))
        .unwrap()
        .unwrap();
    assert_eq!(&*config.policy_version, "distance-sampled-proximity-v1");
    assert_eq!(config.max_distance_m, Ok(2.5));
}

#[test]
fn a_section_of_these_is_parsed_once_and_keeps_its_refusal() {
    let configs = Configs::new(None);
    let mut parses = 0;
    for _ in 0..3 {
        let section = configs.staff_fatigue.get_or_parse(|| {
            parses += 1;
            Section::from(StaffFatigue::parse(&v(json!({"policy_registry": {"staff_fatigue": "x"}}))))
        });
        let Section::Fault(fault) = section else { panic!("a fault") };
        assert_eq!(fault, &Fault::refused("another fatigue formula"));
    }
    assert_eq!(parses, 1);
}

#[test]
fn a_section_is_parsed_from_the_row_of_the_configs_slot() {
    let mut world = World::default();
    // No slot, then a slot without a row: the caller reads the tree.
    assert!(parse_configs(&world, support_task_minutes).unwrap().is_none());
    let index = world.slots.add(Arc::from(MODEL_CONFIGS), Place::Field).unwrap();
    assert!(parse_configs(&world, support_task_minutes).unwrap().is_none());
    world.slots.at(index).unwrap().try_borrow_mut().unwrap().row =
        Some(Box::new(ValueRow(v(json!({"staff": {"support_task_minutes": 12}})))));
    let Some(Section::Ok(minutes)) = parse_configs(&world, support_task_minutes).unwrap() else { panic!("a value") };
    assert_eq!(*minutes, 12);
    world.slots.at(index).unwrap().try_borrow_mut().unwrap().row =
        Some(Box::new(ValueRow(v(json!({"policy_registry": {"staff_operations": "other"}})))));
    let Some(Section::Fault(fault)) = parse_configs(&world, known_staff_operations).unwrap() else { panic!("a fault") };
    assert_eq!(fault, Fault::refused("another staff operations formula"));
    // A borrowed slot is the core's error, not a parse.
    let _held = world.slots.at(index).unwrap().try_borrow_mut().unwrap();
    assert!(parse_configs(&world, support_task_minutes).is_err());
}
