//! An EffectProgram event's suppressed and failed texts (Issue #225, lane M2; moved from
//! `tests/integration/test_effect_program_texts.py`).
//!
//! The environment, temporary-space, geometry and structure handlers (`rules::effects`): these texts
//! are saved, as the dispatcher writes them to the event's record as `detail`. Each case checks the
//! class and text the deleted Python rule gave (written down from it: lane L10, `kernel_only`) and
//! that the world's canonical bytes did not change.
//!
//! The worlds are the generated Scenario the Project screen makes, with each test's program
//! scheduled by `schedule_effect_program` in Python (its EffectProgram built and validated with
//! pydantic: `tests/data/generated_texts.json`), and the world of a patrol under way whose handoff
//! is due (`tests/data/handoff_texts.json`, without its EventLog; `tests/support/core_fixtures_texts.py`).
//! The host's contracts give their input back as given (decision (c)): the texts of a contract's own
//! error are pydantic's, and the pytest file keeps those cases, which also test each handler's
//! Python entry. It keeps the start the world's check turns down too: the check is a Python
//! validator's (`domain/environment.py`), which the host here does not run.

mod support;

use std::rc::Rc;
use std::sync::Arc;

use serde_json::{json, Value};
use sheltersim_core::cell::Shard;
use sheltersim_core::error::Error;
use sheltersim_core::rules::care_receipts::Made;
use sheltersim_core::rules::effects::{self, Structure};
use sheltersim_core::rules::tick::{self, Tick};
use sheltersim_core::row::ValueRow;
use sheltersim_core::state::staff_operations::{Phase, StaffOperationsRow, NAME as STAFF_OPERATIONS};
use sheltersim_core::value::V;
use sheltersim_core::world::{self, Fit, World, WorldCell, ID_COUNTERS};

use support::cases;
use support::dumps::Dumps;
use support::load;
use support::texts::{self, big, changed, DROP};
use support::TestHost;

const FILE: &str = "generated_texts";
const KEY: &str = "effect_program_v1";
const START: i64 = 5;
const END: i64 = 30;
const FAILED: &str = "EventApplicationError";
const SUPPRESSED: &str = "EventSuppressed";

fn text(value: &str) -> Arc<str> {
    Arc::from(value)
}

/// A world of the fixtures in a cell, with a host of its sections and floor plan.
struct Case {
    entry: Value,
    cell: Shard<World>,
    host: TestHost,
}

fn case(file: &str, name: &str) -> Case {
    let entry = cases::entry(file, name);
    let cell = texts::cell(&entry);
    let host = load::host(&entry);
    Case { entry, cell, host }
}

fn generated(name: &str) -> Case {
    case(FILE, name)
}

/// The handlers, by the class of the pytest file.
#[derive(Clone, Copy, Debug)]
enum Handler {
    Environment { expire: bool },
    TemporarySpace { expire: bool },
    Geometry { expire: bool },
    Structure,
}

use Handler::*;

impl Case {
    fn edit(&self, edit: impl FnOnce(&mut World)) {
        self.cell.with(edit).unwrap();
    }

    fn bytes(&self) -> String {
        texts::canonical_of(&self.cell, &self.entry)
    }

    /// `handler.apply(world, event)`: the receipt of a structural change, if any.
    fn apply(&mut self, handler: Handler, event: &Value) -> Result<Option<Made>, Error> {
        let row = texts::row(event);
        let (cell, host) = (&self.cell, &mut self.host);
        match handler {
            Environment { expire } => effects::environment_event(cell, host, &row, expire).map(|()| None),
            TemporarySpace { expire } => effects::temporary_space_event(cell, host, &row, expire).map(|()| None),
            Geometry { expire } => effects::geometry_event(cell, host, &row, expire).map(|()| None),
            Structure => match effects::structure_of(&row)? {
                Structure::TaskOrder => effects::task_order_event(cell, host, &row).map(|()| None),
                Structure::Handoff => effects::handoff_event(cell, host, &row),
                Structure::Repair => effects::repair_event(cell, host, &row),
                Structure::Role => effects::role_event(cell, host, &row).map(|()| None),
                Structure::Add => effects::add_event(cell, host, &row).map(|_| None),
            },
        }
    }

    /// The handler's outcome is `class` with `expected`, and the world did not change
    /// (`kernel_says_python_s_text`).
    fn says(&mut self, handler: Handler, event: &Value, class: &str, expected: &str) {
        let before = self.bytes();
        let said = texts::outcome(self.apply(handler, event));
        assert_eq!(said, (class_name(class), expected.to_owned()), "{handler:?} {event}");
        assert!(self.bytes() == before, "the world changed: {handler:?} {event}");
    }

    fn pending(&self, event_type: &str, op: Option<&str>) -> Value {
        texts::pending(&self.entry, event_type, op)
    }

    fn sorted_ids(&self, table: &str) -> Vec<String> {
        let mut ids: Vec<String> = self.entry["world"][table].as_array().unwrap().iter().map(|row| row["id"].as_str().unwrap().to_owned()).collect();
        ids.sort();
        ids
    }
}

fn class_name(class: &str) -> &'static str {
    match class {
        FAILED => FAILED,
        _ => SUPPRESSED,
    }
}

/// The provenance of an event with its program's marker's fields set (`_marker`).
fn marker(event: &Value, fields: Value) -> Value {
    let mut provenance = event["provenance"].clone();
    for (key, value) in fields.as_object().unwrap() {
        provenance[KEY][key.as_str()] = value.clone();
    }
    provenance
}

fn payload(event: &Value, changes: Value) -> Value {
    changed(event, changes, json!({}))
}

// ---- EffectProgramEnvironmentHandler -------------------------------------------------------------

#[test]
fn a_zone_environment_event_s_texts_are_python_s() {
    let mut case = generated("environment");
    let start = case.pending("effect_program_environment", None);
    let expiry = case.pending("effect_program_environment_expire", None);
    let duration = case.entry["world"]["duration_minutes"].clone();
    let (apply, expire) = (Environment { expire: false }, Environment { expire: true });
    let fields = "zone environment event payload fields do not match";
    let strings = "zone environment event IDs must be strings";
    let policy = "zone environment event has no installed program policy";
    let unsupported = "zone environment operator is unsupported";
    let minute = "zone environment modifier expiry minute is invalid";
    let provenance = "zone environment assumption provenance is invalid";
    let cases: Vec<(Handler, Value, &str, &str)> = vec![
        (apply, payload(&start, json!({"other": 1})), FAILED, fields),
        (apply, payload(&start, json!({"end_minute": DROP})), FAILED, fields),
        (expire, start.clone(), FAILED, fields),
        (apply, payload(&start, json!({"zone_id": 1})), FAILED, strings),
        (expire, payload(&expiry, json!({"program_id": null})), FAILED, strings),
        (apply, payload(&start, json!({"program_id": "no-such-program"})), FAILED, policy),
        (apply, payload(&start, json!({"effect_id": "no-such-effect"})), FAILED, policy),
        (apply, changed(&start, json!({}), json!({"provenance": {}})), FAILED, policy),
        (apply, changed(&start, json!({}), json!({"provenance": marker(&start, json!({"policy_dependencies": []}))})), FAILED, policy),
        (apply, payload(&start, json!({"modifier_id": "modifier-other"})), FAILED, "zone environment modifier ID does not match its effect"),
        (expire, expiry.clone(), SUPPRESSED, "zone environment modifier is not active"),
        (apply, payload(&start, json!({"op": "set_noise"})), FAILED, unsupported),
        // A list is frozen to a tuple, which a set can hash.
        (apply, payload(&start, json!({"op": ["set_lighting"]})), FAILED, unsupported),
        (apply, payload(&start, json!({"op": null})), FAILED, unsupported),
        (apply, payload(&start, json!({"op": "set_lighting"})), FAILED, "zone environment operator does not match its program"),
        (apply, payload(&start, json!({"zone_id": "no-such-zone"})), FAILED, "zone environment target does not exist"),
        (apply, payload(&start, json!({"end_minute": big("1000000000000000000000000000000")})), FAILED, minute),
        (apply, payload(&start, json!({"end_minute": true})), FAILED, minute),
        (apply, payload(&start, json!({"end_minute": END as f64})), FAILED, minute),
        (apply, payload(&start, json!({"end_minute": START})), FAILED, minute),
        (apply, payload(&start, json!({"end_minute": duration})), FAILED, minute),
        (apply, payload(&start, json!({"end_minute": END - 1})), FAILED, "zone environment modifier has no unique expiry event"),
        (apply, changed(&start, json!({}), json!({"provenance": marker(&start, json!({"assumption_ids": ["a", 1]}))})), FAILED, provenance),
        (apply, changed(&start, json!({}), json!({"provenance": marker(&start, json!({"assumption_ids": "a"}))})), FAILED, provenance),
    ];
    for (handler, event, class, expected) in cases {
        case.says(handler, &event, class, expected);
    }
    case.apply(apply, &start).unwrap();
    case.says(apply, &start, SUPPRESSED, "zone environment modifier is already active");
    case.says(expire, &changed(&expiry, json!({}), json!({"causation_id": "another-start"})), FAILED, "zone environment expiry does not match its start");
}

// ---- EffectProgramTemporarySpaceHandler ----------------------------------------------------------

#[test]
fn a_temporary_space_event_s_texts_are_python_s() {
    let mut case = generated("temporary_space");
    let start = case.pending("effect_program_temporary_space", None);
    let expiry = case.pending("effect_program_temporary_space_expire", None);
    let (apply, expire) = (TemporarySpace { expire: false }, TemporarySpace { expire: true });
    let approved = "temporary-space event does not reference an approved installed effect";
    let not_started = "temporary-space effect did not start or does not match its expiry";
    let interval = "temporary-space end minute is outside its approved interval";
    let fields = "temporary-space event payload fields do not match";
    let cases: Vec<(Handler, Value, &str, &str)> = vec![
        (apply, payload(&start, json!({"program_id": "no-such-program"})), FAILED, approved),
        (apply, payload(&start, json!({"program_id": 1})), FAILED, approved),
        (apply, payload(&start, json!({"effect_id": "no-such-effect"})), FAILED, approved),
        (apply, payload(&start, json!({"modifier_id": "modifier-other"})), FAILED, approved),
        (apply, payload(&start, json!({"zone_id": "no-such-zone"})), FAILED, approved),
        (apply, payload(&start, json!({"zone_id": ["a", "b"]})), FAILED, approved),
        (apply, payload(&start, json!({"zone_id": DROP})), FAILED, approved),
        (apply, changed(&start, json!({}), json!({"provenance": {}})), FAILED, approved),
        (apply, changed(&start, json!({}), json!({"provenance": marker(&start, json!({"policy_dependencies": []}))})), FAILED, approved),
        (expire, payload(&expiry, json!({"op": "add_temporary_space"})), FAILED, "temporary-space expiry payload fields do not match"),
        (expire, expiry.clone(), SUPPRESSED, not_started),
        (apply, payload(&start, json!({"op": "set_lighting"})), FAILED, fields),
        (apply, payload(&start, json!({"parameters": DROP})), FAILED, fields),
        (apply, payload(&start, json!({"end_minute": big("1000000000000000000000000000000")})), FAILED, interval),
        (apply, payload(&start, json!({"end_minute": START})), FAILED, interval),
        (apply, payload(&start, json!({"end_minute": true})), FAILED, interval),
        (apply, payload(&start, json!({"end_minute": null})), FAILED, interval),
    ];
    for (handler, event, class, expected) in cases {
        case.says(handler, &event, class, expected);
    }
    case.apply(apply, &start).unwrap();
    case.says(apply, &start, FAILED, "temporary space ID already exists");
    case.says(expire, &changed(&expiry, json!({}), json!({"causation_id": "another-start"})), SUPPRESSED, not_started);
    let late = changed(&expiry, json!({}), json!({"causation_id": start["event_id"], "due_minute": END - 1}));
    case.says(expire, &late, SUPPRESSED, not_started);
}

#[test]
fn a_temporary_space_whose_guard_is_false_is_suppressed() {
    let mut case = generated("temporary_space_guard");
    let start = case.pending("effect_program_temporary_space", None);
    case.says(TemporarySpace { expire: false }, &start, SUPPRESSED, "EffectProgram guard evaluated false");
}

// ---- EffectProgramGeometryHandler ----------------------------------------------------------------

#[test]
fn a_geometry_event_s_texts_are_python_s() {
    let mut case = generated("geometry");
    let start = case.pending("effect_program_geometry", None);
    let expiry = case.pending("effect_program_geometry_expire", None);
    let (apply, expire) = (Geometry { expire: false }, Geometry { expire: true });
    let fields = "geometry effect has invalid fields or operator";
    let approved = "geometry effect does not reference its approved program";
    let cases: Vec<(Handler, Value, &str, &str)> = vec![
        (apply, payload(&start, json!({"other": 1})), FAILED, fields),
        (apply, payload(&start, json!({"op": "set_lighting"})), FAILED, fields),
        (apply, payload(&start, json!({"op": ["set_route_access"]})), FAILED, fields),
        (apply, payload(&start, json!({"program_id": "no-such-program"})), FAILED, approved),
        (apply, payload(&start, json!({"program_id": 1})), FAILED, approved),
        (apply, payload(&start, json!({"effect_id": null})), FAILED, approved),
        (apply, payload(&start, json!({"op": "set_partition"})), FAILED, approved),
        (apply, payload(&start, json!({"modifier_id": "modifier-other"})), FAILED, approved),
        (expire, expiry.clone(), SUPPRESSED, "geometry modifier was not applied or already expired"),
        // What `apply_geometry_change` raises is the text, as the Python rule words it.
        (apply, payload(&start, json!({"target_id": "no-such-route"})), FAILED, "target_missing: route does not exist"),
    ];
    for (handler, event, class, expected) in cases {
        case.says(handler, &event, class, expected);
    }
}

// ---- EffectProgramStructureHandler: additions, roles, repairs ------------------------------------

#[test]
fn an_addition_s_texts_are_python_s() {
    for (op, table, bound) in [("add_staff", "staff", 100_i64), ("add_toilet", "facilities", 200)] {
        // The floor plan has no place for so many toilets; this is about the bound.
        let mut case = generated(op);
        assert_eq!(case.entry["world"]["model_configs"]["geometry_overlay"]["schema_version"], 3);
        let event = case.pending("effect_program_structure", Some(op));
        let invalid = "structural primitive target or operation is invalid";
        let fields = "structural primitive payload fields do not match";
        let exceed = if op == "add_staff" { "adding staff would exceed the 100-staff runtime bound" } else { "adding toilets would exceed the 200-facility bound" };
        let room = bound - case.entry["world"][table].as_array().unwrap().len() as i64;
        let cases: Vec<(Value, &str)> = vec![
            (json!({"other": 1}), fields),
            (json!({"count": DROP}), fields),
            (json!({"program_id": "no-such-program"}), invalid),
            (json!({"program_id": null}), invalid),
            (json!({"effect_id": "no-such-effect"}), invalid),
            (json!({"op": "add_bed"}), invalid),
            (json!({"op": ["add_staff"]}), invalid),
            (json!({"count": 0}), invalid),
            (json!({"count": big("-1000000000000000000000000000000")}), invalid),
            (json!({"count": true}), invalid),
            (json!({"count": 1.0}), invalid),
            (json!({"zone_id": "no-such-zone"}), invalid),
            (json!({"zone_id": 1}), invalid),
            (json!({"count": room + 1}), exceed),
            (json!({"count": big("1000000000000000000000000000000")}), exceed),
        ];
        for (change, expected) in cases {
            case.says(Structure, &payload(&event, change), FAILED, expected);
        }
        case.apply(Structure, &event).unwrap();
        let exists = if op == "add_staff" { "generated staff ID already exists" } else { "generated toilet ID already exists" };
        case.says(Structure, &event, FAILED, exists);
    }
}

#[test]
fn a_toilet_without_a_free_point_is_python_s_text() {
    let mut case = generated("add_toilet_plain");
    // Each toilet's point is chosen on the floor plan with the toilets placed before it, as the
    // native host builds the plan again from the world's overlay at each call.
    let loaded = Rc::new(load::loaded(&case.entry));
    case.host = load::following_host(&case.entry, &loaded);
    let cell = &loaded.cell;
    let event = case.pending("effect_program_structure", Some("add_toilet"));
    let facilities = case.entry["world"]["facilities"].as_array().unwrap().len() as i64;
    let many = payload(&event, json!({"count": 200 - facilities}));
    // The first toilet the floor plan has no free point for, as the Python rule named it.
    let suffix = event["payload"]["modifier_id"].as_str().unwrap().strip_prefix("modifier-").unwrap().to_owned();
    assert_eq!(suffix, "1a1febf9fee93453e26e19f2ed94ab22");
    let before = texts::canonical_of(cell, &case.entry);
    let row = texts::row(&many);
    let said = texts::outcome(effects::add_event(cell, &mut case.host, &row));
    assert_eq!(said, (FAILED, format!("no free floor-plan point for installed facility 'effect-toilet-{suffix}-16'")));
    assert!(texts::canonical_of(cell, &case.entry) == before);
}

#[test]
fn a_role_change_s_texts_are_python_s() {
    let mut case = generated("role");
    let event = case.pending("effect_program_structure", Some("set_staff_role"));
    let target = "staff-role event does not reference an approved staff target";
    for change in [
        json!({"program_id": "no-such-program"}),
        json!({"program_id": 1}),
        json!({"effect_id": "no-such-effect"}),
        json!({"op": "add_staff"}),
        json!({"staff_id": "no-such-staff"}),
        json!({"staff_id": 1}),
    ] {
        case.says(Structure, &payload(&event, change), FAILED, target);
    }
}

#[test]
fn a_repair_s_texts_are_python_s() {
    let mut case = generated("repair");
    let event = case.pending("effect_program_structure", Some("facility_repair"));
    let facility = event["payload"]["facility_id"].as_str().unwrap().to_owned();
    let capacity = case.cell.with(|world| world.rows().unwrap().facilities.get(&facility).unwrap().capacity).unwrap();
    let approved = "facility repair does not reference an approved program";
    for change in [
        json!({"program_id": "no-such-program"}),
        json!({"effect_id": "no-such-effect"}),
        json!({"op": "facility_outage"}),
        json!({"modifier_id": "modifier-other"}),
        json!({"facility_id": 1}),
    ] {
        case.says(Structure, &payload(&event, change), FAILED, approved);
    }
    for fields in [json!({"op": "add_staff"}), json!({"phase": "end"}), json!({"program_id": "other"}), json!({"effect_id": null})] {
        let provenance = marker(&event, fields);
        case.says(Structure, &changed(&event, json!({}), json!({"provenance": provenance})), FAILED, approved);
    }
    case.says(Structure, &changed(&event, json!({}), json!({"event_id": "another-event"})), FAILED, approved);
    let other = case.sorted_ids("facilities").into_iter().find(|id| *id != facility).unwrap();
    case.says(Structure, &payload(&event, json!({"facility_id": other})), FAILED, "facility repair target does not match a known facility");
    case.edit(|world| world.rows_mut().unwrap().facilities.get_mut(&facility).unwrap().working_units = capacity);
    case.says(Structure, &event, FAILED, "facility repair preconditions no longer match the live facility state");
    case.edit(|world| {
        world.rows_mut().unwrap().facilities.get_mut(&facility).unwrap().working_units = 0;
        world.slots.get("shared_operations_v1").unwrap().try_borrow_mut().unwrap().row = None;
    });
    case.says(Structure, &event, FAILED, "facility repair requires shared receipt state");
}

// ---- EffectProgramStructureHandler: a handoff ----------------------------------------------------

const HANDOFF: &str = "handoff_texts";

fn handing_off() -> Case {
    case(HANDOFF, "handing_off")
}

/// `EventRecord.to_dict()` of a receipt.
fn record_document(made: &Made) -> V {
    let event = &made.event;
    let text = |value: &str| V::Str(text(value));
    let optional = |value: &Option<Arc<str>>| value.as_deref().map_or(V::Null, text);
    let event = V::Map(
        [
            ("event_id", text(&event.event_id)),
            ("kind", text(event.kind.as_str())),
            ("due_minute", V::Int(event.due_minute)),
            ("priority", V::Int(event.priority)),
            ("sequence", V::Int(event.sequence)),
            ("event_type", text(&event.event_type)),
            ("payload", event.payload.clone()),
            ("source_id", optional(&event.source_id)),
            ("causation_id", optional(&event.causation_id)),
            ("provenance", event.provenance.clone()),
        ]
        .into_iter()
        .map(|(key, value)| (Arc::from(key), value))
        .collect(),
    );
    V::Map(
        [("event", event), ("realized_minute", V::Int(made.minute)), ("status", text("applied")), ("detail", text(&made.detail))]
            .into_iter()
            .map(|(key, value)| (Arc::from(key), value))
            .collect(),
    )
}

/// `json.dumps(world.to_dict(), ensure_ascii=False) + "\n" + json.dumps([record.to_dict()],
/// ensure_ascii=False)`, the documents the pytest file took the digest of.
fn documents(case: &Case, made: &Made) -> String {
    let canonical: Value = serde_json::from_str(&case.bytes()).unwrap();
    let dumps = Dumps::default().unicode();
    format!("{}\n{}", dumps.text(&load::v(&canonical)), dumps.text(&V::List(vec![record_document(made)])))
}

fn staff_of<'a>(world: &'a World, id: &str) -> (Option<Arc<str>>, Arc<str>) {
    let rows = world.rows().unwrap();
    let staff = rows.staff.get(id).unwrap();
    (staff.active_task_id.clone(), staff.zone_id.clone())
}

fn phase_of(case: &Case, id: &str) -> Phase {
    case.cell.with(|world| world.slots.try_row::<StaffOperationsRow>(STAFF_OPERATIONS).unwrap().staff.get(id).unwrap().phase).unwrap()
}

fn get<'a>(value: &'a V, path: &[&str]) -> &'a V {
    path.iter().fold(value, |value, key| match value {
        V::Map(map) => map.get(*key).unwrap_or_else(|| panic!("no {key}")),
        other => panic!("not a map at {key}: {other:?}"),
    })
}

#[test]
fn a_handoff_s_texts_are_python_s() {
    let base = handing_off();
    let event = base.pending("effect_program_structure", Some("staff_handoff"));
    let worker = event["payload"]["staff_id"].as_str().unwrap().to_owned();
    let parameters = event["payload"]["parameters"].clone();
    let replacement = parameters["replacement_staff_id"].as_str().unwrap().to_owned();
    let task_id = parameters["task_id"].as_str().unwrap().to_owned();
    // Someone not idle at the patrol: the generated workers are all elsewhere or busy.
    let replacement_zone = base.cell.with(|world| staff_of(world, &replacement).1).unwrap();
    let other = base
        .sorted_ids("staff")
        .into_iter()
        .find(|id| *id != worker && *id != replacement && base.cell.with(|world| staff_of(world, id).1).unwrap() != replacement_zone)
        .unwrap();

    // The handoff itself is the Python rule's, receipt and all.
    let mut case = handing_off();
    let made = case.apply(Structure, &event).unwrap().expect("a receipt");
    assert_eq!(made.detail, "staff task assignment updated before handoff receipt");
    let receipt = |name: &str| match &made.event.payload {
        V::Map(payload) => payload[name].clone(),
        _ => panic!(),
    };
    assert_eq!(
        [receipt("task_id"), receipt("previous_staff_id"), receipt("replacement_staff_id"), receipt("status")],
        [V::Str(text(&task_id)), V::Str(text(&worker)), V::Str(text(&replacement)), V::Str(text("reassigned"))]
    );
    case.cell
        .with(|world| {
            assert_eq!(staff_of(world, &replacement).0.as_deref(), Some(task_id.as_str()));
            assert_eq!(staff_of(world, &worker).0, None);
            assert_eq!(world.rows().unwrap().active_tasks.get(&task_id).unwrap().staff_id.as_deref(), Some(replacement.as_str()));
        })
        .unwrap();
    assert_eq!((phase_of(&case, &worker), phase_of(&case, &replacement)), (Phase::Idle, Phase::Working));
    // The whole world (without the EventLog the fixture leaves out) and the receipt, as the pytest
    // file took them: recorded from the core in #225 lane M2. On the same kernel the pytest
    // test_effect_program_texts.py::test_a_handoff_s_texts_are_python_s, whose world had its
    // EventLog, passed with the digest b2c8d110332d (recorded from the Python rule on 2d8fb46).
    assert_eq!(texts::sha(&documents(&case, &made)), "e6d8acece25965b78b621a68c4fef6bdf2251f54134fbc54c7fb5debb0195487");

    let approved = "staff handoff does not reference an approved program";
    for change in [json!({"program_id": "no-such-program"}), json!({"op": "staff_task_order_x"}), json!({"staff_id": 1}), json!({"modifier_id": "modifier-other"})] {
        handing_off().says(Structure, &payload(&event, change), FAILED, approved);
    }
    handing_off().says(Structure, &changed(&event, json!({}), json!({"provenance": {}})), FAILED, approved);
    let participants = "staff handoff participants or task are invalid";
    for change in [json!({"replacement_staff_id": "no-such-staff"}), json!({"replacement_staff_id": null}), json!({"task_id": "no-such-task"})] {
        let mut changed_parameters = parameters.clone();
        for (key, value) in change.as_object().unwrap() {
            changed_parameters[key.as_str()] = value.clone();
        }
        handing_off().says(Structure, &payload(&event, json!({"parameters": changed_parameters})), FAILED, participants);
    }
    let mut changed_parameters = parameters.clone();
    changed_parameters["replacement_staff_id"] = json!(other);
    handing_off().says(Structure, &payload(&event, json!({"parameters": changed_parameters})), FAILED, "staff handoff requires a qualified idle replacement already at active work");

    // The replacement called away, through the dispatcher, at this minute (the handoff kept back
    // five minutes): one tick of the core, as `TickEngine.step` runs it.
    let mut unavailable = handing_off();
    let loaded = load::loaded(&unavailable.entry);
    let minute = loaded.cell.with(|world| world.minute_index().unwrap()).unwrap();
    let later = texts::row(&changed(&event, json!({}), json!({"due_minute": event["due_minute"].as_i64().unwrap() + 5})));
    loaded
        .cell
        .with(|world| {
            let sequence = world.id_counter("event").unwrap().unwrap();
            world::set_id_counter(&mut world.row_mut::<ValueRow>(ID_COUNTERS).unwrap(), "event", sequence + 1).unwrap();
            let absence = texts::row(&json!({
                "event_id": "test-absence",
                "kind": "exogenous",
                "due_minute": minute,
                "priority": 0,
                "sequence": sequence,
                "event_type": "staff_unavailable",
                "payload": {"staff_id": &replacement, "until_minute": minute + 30},
                "source_id": "SRC-TEST",
                "causation_id": null,
                "provenance": {"source_type": "assumption", "source_id": "SRC-TEST"},
            }));
            let mut queue = world.queue_mut().unwrap();
            queue.take_slot(event["event_id"].as_str().unwrap()).unwrap();
            assert_eq!(queue.schedule_row(later), 0);
            assert_eq!(queue.schedule_row(absence), 0);
        })
        .unwrap();
    let mut tick = Tick {
        cell: &loaded.cell,
        host: &mut unavailable.host,
        run: &loaded.run,
        policies: loaded.policies.clone(),
        minute,
        seed: loaded.seed.clone(),
        neighbors: loaded.neighbors.clone(),
    };
    if let Err(failed) = tick::run_tick(&mut tick) {
        panic!("{:?} {:?}", failed.label, failed.error);
    }
    loaded.cell.with(|world| world.scalars.get_mut().minute_index = Fit::Ok(minute + 1)).unwrap();
    unavailable.cell = loaded.cell;
    let status = unavailable
        .cell
        .with(|world| get(&world.slots.try_value("operational_events_v1").unwrap().unwrap(), &["staff_status", &replacement, "available"]).clone())
        .unwrap();
    assert_eq!(status, V::Bool(false));
    unavailable.says(Structure, &event, FAILED, "staff handoff replacement is unavailable");

    let mut without_shared = handing_off();
    without_shared.edit(|world| world.slots.get("shared_operations_v1").unwrap().try_borrow_mut().unwrap().row = None);
    without_shared.says(Structure, &event, FAILED, participants);
}

#[test]
fn a_role_change_during_work_is_python_s_text() {
    let mut case = handing_off();
    let role = case.pending("effect_program_structure", Some("set_staff_role"));
    case.says(Structure, &role, FAILED, "staff role cannot change while work is active");
}

/// The loaded worlds write the canonical text Python wrote of them.
#[test]
fn the_loaded_worlds_write_python_s_canonical_text() {
    for (file, name) in [(FILE, "generated"), (FILE, "environment"), (FILE, "repair"), (HANDOFF, "handing_off")] {
        let case = case(file, name);
        let python = Dumps::default().unicode().compact().sorted().text(&load::v(&case.entry["world"]));
        assert!(case.bytes() == python, "{file} {name}");
    }
}
