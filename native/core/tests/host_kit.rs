//! The host kit of lane P9-0 (design-p9.md §4): what the world holds (`Holding`, H1), the
//! sections of the configs a host fills (`SectionId`, H2), the dispatcher's handlers as the Run
//! reads them (`Policies::dispatch`, H3), a foreign handler (H4), and the clock's error.

mod support;

use std::rc::Rc;
use std::sync::Arc;

use indexmap::IndexMap;
use sheltersim_core::cell::Shard;
use sheltersim_core::config::{self, Configs, Parser, Section, SectionId};
use sheltersim_core::error::{Error, CLOCK};
use sheltersim_core::events::{EventKind, EventRow};
use sheltersim_core::host::{
    ErrorClass, Hold, Holding, Host, Key, HostFailure, NOT_OWNED, NO_FOREIGN, NO_PYTHON_SINK, NO_SECTIONS, TABLE_MAPPING, TABLE_NOT_PLAIN,
};
use sheltersim_core::row::{Row, ValueRow};
use sheltersim_core::rules::kit::{self, Held as Kept};
use sheltersim_core::run::dispatch::{AfterComplete, Dispatch, Handler, HandlerKind, Native, MANAGER, NOT_A_BOOL, NOT_TESTABLE, RETURN_RULE};
use sheltersim_core::run::held::{Class, Held, Obj, NO_CONTROLLER};
use sheltersim_core::run::Policies;
use sheltersim_core::value::V;
use sheltersim_core::world::{Place, World, MODEL_CONFIGS};
use support::{Call, TestHost};

fn text(value: &str) -> Arc<str> {
    Arc::from(value)
}

fn slot(world: &mut World, name: &str, place: Place, row: Option<Box<dyn Row>>) {
    let index = world.slots.add(text(name), place).unwrap();
    world.slots.at(index).unwrap().try_borrow_mut().unwrap().row = row;
}

fn configs_row(value: V) -> Option<Box<dyn Row>> {
    Some(Box::new(ValueRow(value)))
}

// ---- H1: what the world holds ----------------------------------------------------------------------

#[test]
fn a_world_without_python_views_holds_what_its_slots_hold() {
    let mut world = World::default();
    slot(&mut world, "movement_v1", Place::Namespace, configs_row(V::Map(IndexMap::new())));
    slot(&mut world, "zone_environment_v1", Place::Value, None);
    slot(&mut world, MODEL_CONFIGS, Place::Field, configs_row(V::Map(IndexMap::new())));
    let cell = Shard::new(world);
    let mut host = TestHost::default();
    let holding = kit::holding(&cell, &mut host).unwrap();
    assert_eq!(host.calls, [Call::Holding]);
    assert_eq!(holding.held("movement_v1", "other"), Kept::Held);
    assert_eq!(holding.held("zone_environment_v1", "other"), Kept::Absent);
    assert_eq!(holding.held("never_v1", "other"), Kept::Absent);
    assert_eq!(holding.is_absent("movement_v1"), Ok(false));
    assert_eq!(holding.is_absent("zone_environment_v1"), Ok(true));
    assert!(holding.field(MODEL_CONFIGS));
    assert!(holding.field("not_owned"));
}

#[test]
fn a_holding_says_each_read_sites_refusal() {
    let mut holding = Holding::default();
    holding.keys.insert(text("held_v1"), Key::new(Hold::Held, false));
    holding.keys.insert(text("held_none_v1"), Key::new(Hold::Held, true));
    holding.keys.insert(text("other_v1"), Key::new(Hold::Other, false));
    holding.keys.insert(text("none_v1"), Key::new(Hold::Other, true));
    holding.keys.insert(text("loose_v1"), Key::new(Hold::NotOwned, false));
    holding.fields.insert(text("metric_state"), false);
    assert_eq!(holding.held("held_v1", "mine"), Kept::Held);
    // The read site's own refusal of another value; the slot's of a key no slot owns.
    assert_eq!(holding.held("other_v1", "mine"), Kept::Other("mine"));
    assert_eq!(holding.held("loose_v1", "mine"), Kept::Other(NOT_OWNED));
    // `model_state.get(name) is None`: missing, or None whatever owns it.
    assert_eq!(holding.is_absent("none_v1"), Ok(true));
    assert_eq!(holding.is_absent("held_none_v1"), Ok(true));
    assert_eq!(holding.is_absent("other_v1"), Ok(false));
    assert_eq!(holding.is_absent("loose_v1"), Ok(false));
    assert!(!holding.field("metric_state"));
    holding.table = false;
    assert_eq!(holding.held("held_v1", "mine"), Kept::Other(TABLE_MAPPING));
    assert_eq!(holding.is_absent("held_v1"), Err(TABLE_NOT_PLAIN));
}

#[test]
fn a_host_call_that_writes_a_namespace_is_seen_at_the_next_read() {
    let mut world = World::default();
    slot(&mut world, "social_runtime_v1", Place::Value, None);
    let cell = Rc::new(Shard::new(world));
    let mut host = TestHost::default();
    // The LLM hook writes the namespace, as the native host's social policy writes model_state.
    let hooked = cell.clone();
    host.on_llm = Some(Box::new(move || {
        hooked.try_borrow_mut().unwrap().slots.get("social_runtime_v1").unwrap().try_borrow_mut().unwrap().row =
            Some(Box::new(ValueRow(V::Map(IndexMap::new()))));
    }));
    let before = kit::holding(&*cell, &mut host).unwrap();
    assert_eq!(before.held("social_runtime_v1", "other"), Kept::Absent);
    host.consider_cooperation("A1", "A2", "E1").unwrap();
    let after = kit::holding(&*cell, &mut host).unwrap();
    assert_eq!(after.held("social_runtime_v1", "other"), Kept::Held);
    // What a host gives answers instead of the slots.
    let given = Arc::new(Holding { table: false, ..Holding::default() });
    host.holding = Some(given.clone());
    assert_eq!(kit::holding(&*cell, &mut host).unwrap(), given);
}

// ---- H2: the sections of the configs -----------------------------------------------------------------

/// Every `Lazy` of `Configs`, by name: this does not compile when a field is added, so a new
/// section must join `SectionId` (and the PyO3 crate's `fill_section`).
fn fields(configs: &Configs) -> usize {
    let Configs {
        stamp: _,
        selection: _,
        tick_plan: _,
        facility_geometry: _,
        spatial_constraints: _,
        geo: _,
        staff_fatigue: _,
        staff_operations: _,
        staff_operations_known: _,
        support_task_minutes: _,
        support_cooldown_minutes: _,
        information_spread: _,
        body_models: _,
        social_interaction_minutes: _,
        care_outcome: _,
        agent_dynamics_policy: _,
        facility_awareness: _,
        knowledge_is_limited: _,
        zone_environment: _,
        infrastructure: _,
        proximity: _,
        movement: _,
        route_capacities: _,
        transit_capacities: _,
        facility_queues: _,
        overlay_schema: _,
        overlay_rooms: _,
        resource_service: _,
        accessibility_needs: _,
        extensions_enabled: _,
    } = configs;
    // Less the stamp and the plans (`geo`), which are not sections.
    30 - 2
}

#[test]
fn every_section_can_be_filled_from_outside_the_crate() {
    let configs = Configs::new(None);
    assert_eq!(SectionId::ALL.len(), fields(&configs));
    for which in SectionId::ALL {
        assert!(!which.is_filled(&configs), "{}", which.name());
        support::fill_section(&configs, *which);
        assert!(which.is_filled(&configs), "{}", which.name());
    }
    // Each with a value, none a fault.
    assert!(matches!(configs.selection.get(), Some(Section::Ok(_))));
    assert!(matches!(configs.facility_geometry.get(), Some(Section::Ok(geometry)) if geometry.independent_queues == Ok(false)));
    assert!(matches!(configs.care_outcome.get(), Some(Section::Ok(formula)) if &*formula.model_id == "care-outcome-test-v1"));
    let names: Vec<&str> = SectionId::ALL.iter().map(|which| which.name()).collect();
    assert_eq!(names.len(), names.iter().collect::<std::collections::BTreeSet<_>>().len());
    assert_eq!(SectionId::Movement.parser(), Parser::Value);
    assert_eq!(SectionId::Selection.parser(), Parser::Host);
}

/// A world whose `model_configs` slot holds `{}`.
fn configured() -> Shard<World> {
    let mut world = World::default();
    slot(&mut world, MODEL_CONFIGS, Place::Field, configs_row(V::Map(IndexMap::new())));
    Shard::new(world)
}

#[test]
fn a_section_a_contract_parses_is_filled_by_the_host_once_for_the_stamp() {
    let cell = configured();
    let mut host = TestHost::default();
    let holding = kit::holding(&cell, &mut host).unwrap();
    host.calls.clear();
    let first = kit::section(&cell, &mut host, &holding, SectionId::Selection, |configs| &configs.selection).unwrap();
    assert!(matches!(first, Section::Ok(_)));
    assert_eq!(host.calls, [Call::FillSection(SectionId::Selection)]);
    // Kept for the stamp: the host is not asked again.
    kit::section(&cell, &mut host, &holding, SectionId::Selection, |configs| &configs.selection).unwrap();
    assert_eq!(host.calls.len(), 1);
    // A write to the configs gives a new stamp: the host fills the section again.
    cell.try_borrow_mut().unwrap().slots.get(MODEL_CONFIGS).unwrap().try_borrow_mut().unwrap().touch();
    kit::section(&cell, &mut host, &holding, SectionId::Selection, |configs| &configs.selection).unwrap();
    assert_eq!(host.calls.len(), 2);
}

#[test]
fn a_section_the_core_parses_is_parsed_from_the_row_and_counted() {
    let cell = configured();
    let mut host = TestHost::default();
    let holding = kit::holding(&cell, &mut host).unwrap();
    host.calls.clear();
    let read = |host: &mut TestHost, holding: &Holding| {
        kit::section_value(&cell, host, holding, SectionId::Movement, |configs| &configs.movement, config::movement::Movement::parse)
    };
    let before = config::parses();
    let first = read(&mut host, &holding).unwrap();
    assert!(matches!(&first, Section::Ok(movement) if !movement.has_model));
    // Parsed here, from the row, without the host; counted as the PyO3 crate counts its parses.
    assert!(host.calls.is_empty());
    assert!(config::parses() > before);
    // Kept for the stamp: neither parsed nor asked again (other tests count parses at the same time,
    // so the count is not compared).
    read(&mut host, &holding).unwrap();
    assert!(host.calls.is_empty());
    // Configs the world does not hold as its field: the host reads them.
    let mut loose = (*holding).clone();
    loose.fields.insert(text(MODEL_CONFIGS), false);
    read(&mut host, &loose).unwrap();
    assert_eq!(host.calls, [Call::FillSection(SectionId::Movement)]);
}

#[test]
fn a_host_without_the_parsers_refuses_to_fill_a_section() {
    let cell = configured();
    let holding = Holding::default();
    let mut host = support::Refusing;
    let error = kit::section(&cell, &mut host, &holding, SectionId::TickPlan, |configs| &configs.tick_plan).err();
    assert_eq!(error, Some(Error::Unsupported(NO_SECTIONS.into())));
    // Its holding is the world's.
    assert_eq!(host.holding(), Ok(None));
}

// ---- H3: the dispatcher's handlers --------------------------------------------------------------------

fn handler(kind: HandlerKind, attrs: Vec<(&str, Held)>) -> Native {
    let (module, qualname) = kind.class();
    let mut object = Obj::new(Class::named(module, qualname));
    for (name, held) in attrs {
        object.attrs.insert(text(name), Ok(held));
    }
    Native { kind, handler: Arc::new(object), own_managers: true, after_arrival_as_defined: true }
}

#[test]
fn the_core_knows_the_29_handler_classes_by_their_names() {
    assert_eq!(HandlerKind::ALL.len(), 29);
    for kind in HandlerKind::ALL {
        let (module, qualname) = kind.class();
        assert_eq!(HandlerKind::of_class(module, qualname), Some(*kind));
    }
    assert_eq!(HandlerKind::of_class("sheltersim.events.handlers", "ProbeHandler"), None);
    // The class's module counts: a handler of that name elsewhere is not one the core runs.
    assert_eq!(HandlerKind::of_class("tests.fakes", "SocialInteractionHandler"), None);
}

#[test]
fn a_handlers_values_read_as_its_rule_reads_them() {
    let support = handler(
        HandlerKind::StaffSupportCompletion,
        vec![("cooldown_minutes", Held::Value(V::Int(30))), ("require_colocated", Held::Value(V::Bool(true)))],
    );
    assert_eq!(support.cooldown_minutes(), Ok(30));
    assert_eq!(support.require_colocated(), Ok(true));
    // `bool(x)` of a plain value; `exact_int` refuses a bool.
    let odd = handler(
        HandlerKind::SupportRequestTimeout,
        vec![("cooldown_minutes", Held::Value(V::Bool(true))), ("require_colocated", Held::Value(V::Str(text(""))))],
    );
    assert!(odd.cooldown_minutes().unwrap_err().is_refusal());
    assert_eq!(odd.require_colocated(), Ok(false));
    let opaque = handler(HandlerKind::StaffSupportCompletion, vec![("require_colocated", Held::Opaque { callable: false })]);
    assert_eq!(opaque.require_colocated().unwrap_err().text.as_ref(), NOT_TESTABLE);
    let expires = handler(HandlerKind::EffectProgramFacility, vec![("expire", Held::Value(V::Bool(true)))]);
    assert_eq!(expires.expire(), Ok(true));
    let not_bool = handler(HandlerKind::EffectProgramFacility, vec![("expire", Held::Value(V::Int(1)))]);
    assert_eq!(not_bool.expire().unwrap_err().text.as_ref(), NOT_A_BOOL);
    let ledger = handler(HandlerKind::ResourceLedger, vec![("operation", Held::Value(V::Str(text("reserve"))))]);
    assert_eq!(ledger.operation().as_deref(), Ok("reserve"));
    for (held, after) in [
        (Held::Value(V::Null), AfterComplete::None),
        (Held::Method(text(RETURN_RULE)), AfterComplete::Own),
        (Held::Method(text("_route")), AfterComplete::Other),
        (Held::Opaque { callable: true }, AfterComplete::Other),
    ] {
        assert_eq!(handler(HandlerKind::ToiletServiceCompletion, vec![("after_complete", held)]).after_complete(), Ok(after));
    }
}

#[test]
fn a_manager_with_its_own_request_is_seen() {
    let manager = |own: bool| {
        let mut object = Obj::new(Class::named("sheltersim.domain.facilities", "ToiletServiceManager"));
        if own {
            object.own.insert(text("request"), Held::Opaque { callable: true });
        }
        Held::Object(Arc::new(object))
    };
    assert_eq!(MANAGER.own, ["request"]);
    let travel = handler(
        HandlerKind::AgentTravelCompletion,
        vec![("toilet_service", manager(true)), ("resource_service", Held::Value(V::Null)), ("validate_request_state", Held::Value(V::Bool(false)))],
    );
    assert_eq!(travel.own_request("toilet_service"), Ok(true));
    assert_eq!(travel.own_request("resource_service"), Ok(false));
    assert_eq!(travel.validate_request_state(), Ok(false));
    let plain = handler(HandlerKind::AgentTravelCompletion, vec![("toilet_service", manager(false))]);
    assert_eq!(plain.own_request("toilet_service"), Ok(false));
}

#[test]
fn a_run_reads_its_dispatcher_or_refuses() {
    // A Run without a controller refuses; a controller's Run waits for its dispatcher.
    let absent = Policies::absent();
    assert_eq!(absent.dispatch.handler("social_interaction").unwrap_err().text.as_ref(), NO_CONTROLLER);
    let table = IndexMap::from([
        (text("social_interaction"), Handler::Native(handler(HandlerKind::SocialInteraction, vec![]))),
        (text("probe"), Handler::Foreign),
    ]);
    let dispatch = Dispatch { known: Ok(true), handlers: Ok(Arc::new(table)) };
    assert!(matches!(dispatch.handler("social_interaction"), Ok(Some(Handler::Native(native))) if native.kind == HandlerKind::SocialInteraction));
    assert_eq!(dispatch.handler("probe"), Ok(Some(&Handler::Foreign)));
    assert_eq!(dispatch.handler("nothing"), Ok(None));
}

// ---- H4, H5 and the clock -----------------------------------------------------------------------------

#[test]
fn a_host_without_python_runs_no_foreign_handler() {
    let event = EventRow::new(text("E1"), EventKind::Exogenous, 3, 0, 1, text("probe"), IndexMap::new(), None, None, IndexMap::new()).unwrap();
    let mut host = TestHost::default();
    assert_eq!(host.apply_foreign(&event), Err(Error::Unsupported(NO_FOREIGN.into())));
    assert_eq!(host.calls, [Call::Foreign(text("probe"))]);
    let mut bare = support::Refusing;
    assert_eq!(bare.apply_foreign(&event), Err(Error::Unsupported(NO_FOREIGN.into())));
    assert!(NO_PYTHON_SINK.contains("Python"));
}

#[test]
fn the_clock_error_is_the_engines_text_and_a_runtime_error() {
    assert_eq!(Error::Clock.to_string(), CLOCK);
    assert_eq!(CLOCK, "tick phase attempted to change the engine-owned clock");
    assert_eq!(Error::Clock.class(), ErrorClass::Other);
    assert!(!Error::Clock.is_outcome());
}
